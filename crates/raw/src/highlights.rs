//! Highlight reconstruction: values for the samples that clipped, estimated
//! from the colours that did not.
//!
//! The idea is darktable's "inpaint opposed" (Hanno Schwalm, darktable 4.2):
//! a clipped colour is estimated from the other colours around it, with the
//! colour relations measured next to the clipped areas. It runs on
//! white-balanced data (the as-shot gains, as for RCD), before the demosaic:
//!
//! 1. A sample of colour c is clipped when its raw value is ≥ [`CLIP`].
//! 2. Around it, each other colour d whose samples in the 3×3 neighbourhood
//!    are all unclipped gives an estimate `mean_d · k(c, d)`. With two, they
//!    are combined on cube roots, as darktable does. With none, the sample is
//!    made neutral, as bright as the brightest clipped sample around it
//!    (white balanced): left at its clip level, a green next to a red that
//!    clipped higher draws a magenta outline around blown areas.
//! 3. The ratios `k(c, d)` keep the colour of the surroundings: next to the
//!    areas where c clips, over unclipped samples of c, `Σ c / Σ mean_d`.
//!    darktable keeps one offset (not a ratio) per colour for the whole
//!    image and takes the mean of both other colours even when one of them
//!    clipped too; both are wrong in bright skies, where green and blue clip
//!    together, and gave magenta there. Here the ratios are local: summed per
//!    cell of [`CELL`] pixels and spread into the clipped areas from coarser
//!    levels (push-pull), so a clipped sky and a clipped red flower in the
//!    same photo each keep their own colour.
//! 4. Clipped value = max(value, estimate): a clipped sample only says that
//!    the light was at least that bright.
//!
//! Pixels where every colour clipped are made neutral again in
//! `Photo::to_working`, with the white balance chosen then. The result goes
//! above 1.0, the clip point, which the tone stage compresses: negative
//! Exposure or Highlights brings the detail back.

use rayon::prelude::*;

use crate::demosaic::Cfa;
use crate::Image;

/// Raw value (1.0 = white level) from which a sample counts as clipped.
pub const CLIP: f32 = 0.98;

/// Side of the cells over which the colour ratios are summed, in pixels.
pub const CELL: usize = 16;

/// Weight, in samples, of the coarser level's ratio in a cell's: a cell
/// needs well over this many samples of its own to depart from the ratio of
/// its surroundings.
const PRIOR: f64 = 256.0;

/// Samples darker than this fraction of the clip point do not count for the
/// ratios (noisy, and far from the clipped colours).
const MIN_LEVEL: f32 = 0.2;

/// Limits of a colour ratio.
const RATIO: (f32, f32) = (0.125, 8.0);

/// Reconstructs the clipped photosites of a normalised `mosaic` in place.
/// `gains` are the white balance multipliers. Returns how many changed.
pub fn reconstruct_mosaic(mosaic: &mut [f32], width: usize, height: usize, cfa: &Cfa, gains: [f32; 3]) -> usize {
    reconstruct(Samples { values: mosaic, width, height, layout: Layout::Mosaic(cfa), gains })
}

/// As [`reconstruct_mosaic`] for three-channel data (LinearRaw DNGs).
pub fn reconstruct_rgb(img: &mut Image, gains: [f32; 3]) -> usize {
    let (width, height) = (img.width, img.height);
    reconstruct(Samples { values: img.pixels.as_flattened_mut(), width, height, layout: Layout::Rgb, gains })
}

enum Layout<'a> {
    /// One sample per pixel, of the pattern's colour.
    Mosaic(&'a Cfa),
    /// Three samples per pixel.
    Rgb,
}

struct Samples<'a> {
    values: &'a mut [f32],
    width: usize,
    height: usize,
    layout: Layout<'a>,
    gains: [f32; 3],
}

/// Read-only view used by the parallel passes.
struct View<'a> {
    values: &'a [f32],
    width: usize,
    height: usize,
    layout: &'a Layout<'a>,
    gains: [f32; 3],
}

/// Slot of the ratio of colour `c` to colour `d`.
#[inline]
fn slot(c: usize, d: usize) -> usize {
    c * 3 + d
}

impl View<'_> {
    /// Calls `f(index, colour)` for each sample of pixel (x, y).
    #[inline]
    fn for_samples(&self, x: usize, y: usize, mut f: impl FnMut(usize, usize)) {
        let i = y * self.width + x;
        match self.layout {
            Layout::Mosaic(cfa) => f(i, cfa.color_at(x, y)),
            Layout::Rgb => (0..3).for_each(|c| f(3 * i + c, c)),
        }
    }

    /// Over the 3×3 neighbourhood, white balanced: the mean of each colour,
    /// which colours have no clipped sample there (bit c), and the brightest
    /// clipped sample.
    fn around(&self, x: usize, y: usize) -> Around {
        let (mut sum, mut n, mut clipped, mut brightest) = ([0.0f32; 3], [0u32; 3], 0u8, 0.0f32);
        for ny in y.saturating_sub(1)..(y + 2).min(self.height) {
            for nx in x.saturating_sub(1)..(x + 2).min(self.width) {
                self.for_samples(nx, ny, |i, c| {
                    let v = self.values[i];
                    sum[c] += v.max(0.0);
                    n[c] += 1;
                    if v >= CLIP {
                        clipped |= 1 << c;
                        brightest = brightest.max(v * self.gains[c]);
                    }
                });
            }
        }
        Around {
            means: std::array::from_fn(|c| if n[c] > 0 { sum[c] / n[c] as f32 * self.gains[c] } else { 0.0 }),
            usable: (0..3).fold(0u8, |bits, c| bits | (u8::from(n[c] > 0 && clipped >> c & 1 == 0) << c)),
            brightest,
        }
    }
}

#[derive(Clone, Copy)]
struct Around {
    means: [f32; 3],
    usable: u8,
    brightest: f32,
}

impl Around {
    fn usable(&self, d: usize) -> bool {
        self.usable >> d & 1 == 1
    }
}

fn reconstruct(s: Samples) -> usize {
    let Samples { values, width: w, height: h, layout, gains } = s;
    if w == 0 || h == 0 || !values.par_iter().any(|&v| v >= CLIP) {
        return 0;
    }
    let view = View { values, width: w, height: h, layout: &layout, gains };
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));

    // Which colours clipped in each cell, then grown by one cell: where the
    // colour ratios next to the clipped areas are measured.
    let clipped: Vec<u8> = (0..cw * ch)
        .into_par_iter()
        .map(|cell| {
            let mut bits = 0u8;
            cell_pixels(cell, cw, w, h, |x, y| view.for_samples(x, y, |i, c| bits |= u8::from(view.values[i] >= CLIP) << c));
            bits
        })
        .collect();
    let near: Vec<u8> = (0..cw * ch)
        .map(|cell| {
            let (cx, cy) = (cell % cw, cell / cw);
            let mut bits = 0;
            for ny in cy.saturating_sub(1)..(cy + 2).min(ch) {
                for nx in cx.saturating_sub(1)..(cx + 2).min(cw) {
                    bits |= clipped[ny * cw + nx];
                }
            }
            bits
        })
        .collect();

    // Ratio sums per cell and pair of colours.
    let sums: Vec<Sums> = (0..cw * ch)
        .into_par_iter()
        .map(|cell| {
            let mut sums = Sums::default();
            let bits = near[cell];
            if bits != 0 {
                cell_pixels(cell, cw, w, h, |x, y| {
                    let mut around = None;
                    view.for_samples(x, y, |i, c| {
                        let v = view.values[i];
                        if bits >> c & 1 == 1 && v < CLIP && v > MIN_LEVEL * CLIP {
                            let a = *around.get_or_insert_with(|| view.around(x, y));
                            for d in (0..3).filter(|&d| d != c && a.usable(d)) {
                                let k = slot(c, d);
                                sums.value[k] += (v * gains[c]) as f64;
                                sums.reference[k] += a.means[d] as f64;
                                sums.n[k] += 1.0;
                            }
                        }
                    });
                });
            }
            sums
        })
        .collect();
    let ratios = push_pull(sums, cw, ch);

    // Estimates for the clipped samples, written once all are known: they
    // read the original values around them.
    let writes: Vec<(usize, f32)> = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            let mut out = Vec::new();
            for x in 0..w {
                let mut around = None;
                view.for_samples(x, y, |i, c| {
                    let v = view.values[i];
                    if v < CLIP {
                        return;
                    }
                    let a = *around.get_or_insert_with(|| view.around(x, y));
                    let (mut root, mut n) = (0.0f32, 0);
                    for d in (0..3).filter(|&d| d != c && a.usable(d)) {
                        root += (a.means[d] * ratios.at(x, y, slot(c, d))).cbrt();
                        n += 1;
                    }
                    let balanced = if n > 0 {
                        let r = root / n as f32;
                        r * r * r
                    } else {
                        // Every colour clipped around: neutral, as bright as
                        // the brightest clipped sample (each is a lower
                        // bound). Not from the means, which on the edge of a
                        // blown area mix in darker samples, differently for
                        // each colour: the demosaic turns that into a fringe.
                        a.brightest
                    };
                    let estimate = balanced / gains[c];
                    if estimate > v {
                        out.push((i, estimate));
                    }
                });
            }
            out
        })
        .collect();
    for &(i, v) in &writes {
        values[i] = v;
    }
    writes.len()
}

/// Calls `f(x, y)` for each pixel of cell number `cell`.
fn cell_pixels(cell: usize, cw: usize, w: usize, h: usize, mut f: impl FnMut(usize, usize)) {
    let (x0, y0) = (cell % cw * CELL, cell / cw * CELL);
    for y in y0..(y0 + CELL).min(h) {
        for x in x0..(x0 + CELL).min(w) {
            f(x, y);
        }
    }
}

/// Per pair of colours (c, d): the white-balanced values of the unclipped
/// samples of c next to clipped areas, the means of d around them, and how
/// many there are.
#[derive(Clone, Copy, Default)]
struct Sums {
    value: [f64; 9],
    reference: [f64; 9],
    n: [f64; 9],
}

impl Sums {
    fn add(&mut self, o: &Sums) {
        for k in 0..9 {
            self.value[k] += o.value[k];
            self.reference[k] += o.reference[k];
            self.n[k] += o.n[k];
        }
    }
}

/// The colour ratios: one set per cell.
struct Ratios {
    cw: usize,
    ch: usize,
    cells: Vec<[f32; 9]>,
}

impl Ratios {
    /// Ratio `slot` at pixel (x, y), bilinear between cell centres.
    fn at(&self, x: usize, y: usize, slot: usize) -> f32 {
        let gx = ((x as f32 + 0.5) / CELL as f32 - 0.5).clamp(0.0, (self.cw - 1) as f32);
        let gy = ((y as f32 + 0.5) / CELL as f32 - 0.5).clamp(0.0, (self.ch - 1) as f32);
        bilinear(&self.cells, self.cw, self.ch, gx, gy, slot)
    }
}

fn bilinear(grid: &[[f32; 9]], w: usize, h: usize, gx: f32, gy: f32, k: usize) -> f32 {
    let (x0, y0) = (gx as usize, gy as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (gx - x0 as f32, gy - y0 as f32);
    let g = |x: usize, y: usize| grid[y * w + x][k];
    let top = g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx;
    let bottom = g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Push-pull: sums are pooled into ever coarser grids up to one cell, then,
/// coming back down, each cell's ratio is pulled towards the coarser level's
/// (interpolated) one by [`PRIOR`] samples. Cells without samples, inside
/// large clipped areas, take the ratios of their surroundings; with no
/// samples at all a ratio is 1 (neutral after white balance).
fn push_pull(sums: Vec<Sums>, cw: usize, ch: usize) -> Ratios {
    let mut levels = vec![(sums, cw, ch)];
    while levels.last().is_some_and(|&(_, w, h)| w > 1 || h > 1) {
        let (fine, w, h) = levels.last().unwrap();
        let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
        let mut coarse = vec![Sums::default(); nw * nh];
        for y in 0..*h {
            for x in 0..*w {
                coarse[y / 2 * nw + x / 2].add(&fine[y * w + x]);
            }
        }
        levels.push((coarse, nw, nh));
    }
    let estimate = |s: &Sums, prior: &dyn Fn(usize) -> f32, weight: f64| -> [f32; 9] {
        std::array::from_fn(|k| {
            let p = prior(k) as f64;
            if s.n[k] == 0.0 {
                return p as f32;
            }
            let own = s.value[k] / s.reference[k].max(1e-9);
            ((s.n[k] * own + weight * p) / (s.n[k] + weight)) as f32
        })
        .map(|k: f32| k.clamp(RATIO.0, RATIO.1))
    };
    let (top, _, _) = levels.pop().unwrap();
    // The whole image: its own ratios, 1 where it has no samples.
    let mut est = vec![estimate(&top[0], &|_| 1.0, 0.0)];
    let (mut ew, mut eh) = (1, 1);
    while let Some((cells, w, h)) = levels.pop() {
        est = (0..w * h)
            .map(|i| {
                let gx = (((i % w) as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ew - 1) as f32);
                let gy = (((i / w) as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (eh - 1) as f32);
                estimate(&cells[i], &|k| bilinear(&est, ew, eh, gx, gy, k), PRIOR)
            })
            .collect();
        (ew, eh) = (w, h);
    }
    Ratios { cw: ew, ch: eh, cells: est }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mosaic_of(img: &Image, cfa: &Cfa) -> Vec<f32> {
        (0..img.height).flat_map(|y| (0..img.width).map(move |x| (x, y))).map(|(x, y)| img.get(x, y)[cfa.color_at(x, y)]).collect()
    }

    #[test]
    fn nothing_clipped_nothing_changes() {
        let cfa = Cfa::bayer("RGGB").unwrap();
        let img = Image::from_fn(40, 30, |x, y| [0.3 + 0.01 * x as f32, 0.5, 0.2 + 0.01 * y as f32]);
        let mut m = mosaic_of(&img, &cfa);
        let before = m.clone();
        assert_eq!(reconstruct_mosaic(&mut m, 40, 30, &cfa, [2.0, 1.0, 1.5]), 0);
        assert_eq!(m, before);
    }

    /// A bright patch of a known colour whose green clips: the green comes
    /// back from red and blue with the chrominance of the patch's edge.
    #[test]
    fn a_clipped_channel_comes_back_with_the_colour_around_it() {
        let (w, h) = (96, 96);
        let gains = [2.0, 1.0, 1.5];
        // Balanced colour (0.8, 1.0, 0.9) scaled by a radial ramp up to 1.6×.
        let scene = |x: usize, y: usize| -> [f32; 3] {
            let d = ((x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2)).sqrt();
            let k = 1.6 - d / 48.0;
            [0.8 * k, 1.0 * k, 0.9 * k]
        };
        let raw = Image::from_fn(w, h, |x, y| {
            let b = scene(x, y);
            std::array::from_fn(|c| (b[c] / gains[c]).min(1.0))
        });
        for name in ["RGGB", "GBRG"] {
            let cfa = Cfa::bayer(name).unwrap();
            let mut m = mosaic_of(&raw, &cfa);
            assert!(reconstruct_mosaic(&mut m, w, h, &cfa, gains) > 0);
            let (mut worst, mut n) = (0.0f32, 0);
            for y in 0..h {
                for x in 0..w {
                    let c = cfa.color_at(x, y);
                    let truth = scene(x, y)[c] / gains[c];
                    // Only where this colour clipped and the others did not.
                    if c == 1 && truth > 1.02 && scene(x, y)[0] / gains[0] < CLIP - 0.02 {
                        worst = worst.max((m[y * w + x] / truth - 1.0).abs());
                        n += 1;
                    }
                }
            }
            assert!(n > 100, "{n}");
            assert!(worst < 0.03, "{name}: {worst}");
        }
    }

    #[test]
    fn three_channel_data_is_reconstructed_too() {
        let gains = [2.0, 1.0, 1.5];
        let scene = |x: usize| -> [f32; 3] {
            let k = 0.6 + x as f32 / 50.0;
            [0.8 * k, k, 0.9 * k]
        };
        let mut img = Image::from_fn(64, 8, |x, _| {
            let b = scene(x);
            std::array::from_fn(|c| (b[c] / gains[c]).min(1.0))
        });
        assert!(reconstruct_rgb(&mut img, gains) > 0);
        // x = 40: k = 1.4, green clipped, red and blue not.
        let (g, truth) = (img.get(40, 4)[1], scene(40)[1]);
        assert!((g / truth - 1.0).abs() < 0.03, "{g} vs {truth}");
    }

    #[test]
    fn values_never_go_down() {
        let cfa = Cfa::bayer("BGGR").unwrap();
        let img = Image::from_fn(50, 50, |x, y| [((x * 7 + y * 3) % 11) as f32 / 10.0, ((x + y * 5) % 13) as f32 / 12.0, 1.0]);
        let mut m = mosaic_of(&img, &cfa);
        let before = m.clone();
        reconstruct_mosaic(&mut m, 50, 50, &cfa, [1.8, 1.0, 1.4]);
        assert!(m.iter().zip(&before).all(|(a, b)| a >= b));
        // Unclipped samples are untouched.
        assert!(m.iter().zip(&before).all(|(a, b)| *b >= CLIP || a == b));
    }
}
