//! Demosaicing of colour filter array data. Bilinear for the MVP: it works for
//! any repeating RGB pattern (Bayer and X-Trans alike), and since it never
//! mixes channels, white balance can be applied before or after it.

use rayon::prelude::*;

use crate::image::Image;

/// A repeating colour filter pattern. Colours are 0 = R, 1 = G, 2 = B.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfa {
    pub width: usize,
    pub height: usize,
    /// Row-major, `width * height` entries; position (0, 0) of the pattern is
    /// pixel (0, 0) of the mosaic.
    pub colors: Vec<u8>,
}

impl Cfa {
    /// A 2×2 pattern from its name, e.g. "RGGB".
    pub fn bayer(name: &str) -> Option<Cfa> {
        let colors = name
            .chars()
            .map(|c| match c {
                'R' => Some(0),
                'G' => Some(1),
                'B' => Some(2),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()?;
        (colors.len() == 4).then_some(Cfa { width: 2, height: 2, colors })
    }

    #[inline]
    pub fn color_at(&self, x: usize, y: usize) -> usize {
        self.colors[(y % self.height) * self.width + x % self.width] as usize
    }

    pub fn is_bayer(&self) -> bool {
        self.width == 2 && self.height == 2 && {
            let mut c = self.colors.clone();
            c.sort();
            c == [0, 1, 1, 2]
        }
    }

    /// The same pattern seen from pixel (`dx`, `dy`), e.g. after a crop.
    pub fn shifted(&self, dx: usize, dy: usize) -> Cfa {
        let colors = (0..self.height)
            .flat_map(|y| (0..self.width).map(move |x| (x, y)))
            .map(|(x, y)| self.color_at(x + dx, y + dy) as u8)
            .collect();
        Cfa { width: self.width, height: self.height, colors }
    }

    pub fn name(&self) -> String {
        self.colors.iter().map(|&c| ['R', 'G', 'B'][c as usize]).collect()
    }
}

type Taps = [Vec<(isize, isize)>; 3];

/// For each position in the pattern and each colour, the neighbours to
/// average: the pixel itself for its own colour, otherwise the matching
/// pixels in the 3×3 window (5×5 if the 3×3 has none, which X-Trans needs).
fn neighbourhoods(cfa: &Cfa) -> Vec<Taps> {
    let mut out = Vec::with_capacity(cfa.width * cfa.height);
    for py in 0..cfa.height {
        for px in 0..cfa.width {
            let own = cfa.color_at(px, py);
            let at = |dx: isize, dy: isize| {
                let w = cfa.width as isize * 4;
                let h = cfa.height as isize * 4;
                cfa.color_at((px as isize + dx + w) as usize, (py as isize + dy + h) as usize)
            };
            out.push(std::array::from_fn(|c| {
                if c == own {
                    return vec![(0, 0)];
                }
                for r in 1..=2isize {
                    let taps: Vec<_> = (-r..=r)
                        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
                        .filter(|&(dx, dy)| at(dx, dy) == c)
                        .collect();
                    if !taps.is_empty() {
                        return taps;
                    }
                }
                Vec::new()
            }));
        }
    }
    out
}

/// Bilinear demosaic of a `width`×`height` mosaic.
pub fn bilinear(mosaic: &[f32], width: usize, height: usize, cfa: &Cfa) -> Image {
    assert_eq!(mosaic.len(), width * height);
    let taps = neighbourhoods(cfa);
    let mut img = Image::new(width, height);
    img.pixels.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, out) in row.iter_mut().enumerate() {
            let t = &taps[(y % cfa.height) * cfa.width + x % cfa.width];
            for c in 0..3 {
                let (mut sum, mut n) = (0.0f32, 0u32);
                for &(dx, dy) in &t[c] {
                    let (sx, sy) = (x as isize + dx, y as isize + dy);
                    if sx >= 0 && sy >= 0 && (sx as usize) < width && (sy as usize) < height {
                        sum += mosaic[sy as usize * width + sx as usize];
                        n += 1;
                    }
                }
                out[c] = if n > 0 { sum / n as f32 } else { border_fallback(mosaic, width, height, cfa, x, y, c) };
            }
        }
    });
    img
}

/// Mean of the nearest pixels of colour `c`, for image corners where every
/// regular neighbour is outside the image (only happens with X-Trans).
fn border_fallback(mosaic: &[f32], width: usize, height: usize, cfa: &Cfa, x: usize, y: usize, c: usize) -> f32 {
    for r in 1..=cfa.width.max(cfa.height) {
        let (mut sum, mut n) = (0.0f32, 0u32);
        for sy in y.saturating_sub(r)..(y + r + 1).min(height) {
            for sx in x.saturating_sub(r)..(x + r + 1).min(width) {
                if cfa.color_at(sx, sy) == c {
                    sum += mosaic[sy * width + sx];
                    n += 1;
                }
            }
        }
        if n > 0 {
            return sum / n as f32;
        }
    }
    0.0
}

/// Half-size "superpixel" demosaic for Bayer data: each 2×2 cell becomes one
/// pixel (R, mean of the two G, B). Four times fewer pixels and no
/// interpolation, ideal for previews. `None` if the pattern is not Bayer.
pub fn half_size(mosaic: &[f32], width: usize, height: usize, cfa: &Cfa) -> Option<Image> {
    if !cfa.is_bayer() {
        return None;
    }
    Some(Image::from_fn(width / 2, height / 2, |x, y| {
        let mut acc = [0.0f32; 3];
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (sx, sy) = (2 * x + dx, 2 * y + dy);
            acc[cfa.color_at(sx, sy)] += mosaic[sy * width + sx];
        }
        acc[1] *= 0.5;
        acc
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mosaic_of(w: usize, h: usize, cfa: &Cfa, f: impl Fn(usize, usize) -> [f32; 3]) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)[cfa.color_at(x, y)]).collect()
    }

    fn xtrans() -> Cfa {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let colors = rows.concat().chars().map(|c| "RGB".find(c).unwrap() as u8).collect();
        Cfa { width: 6, height: 6, colors }
    }

    #[test]
    fn flat_colour_is_reconstructed_exactly() {
        let colour = [0.6, 0.3, 0.1];
        for cfa in [Cfa::bayer("RGGB").unwrap(), Cfa::bayer("GBRG").unwrap(), xtrans()] {
            let m = mosaic_of(13, 11, &cfa, |_, _| colour);
            let img = bilinear(&m, 13, 11, &cfa);
            for p in &img.pixels {
                assert!(p.iter().zip(colour).all(|(a, b)| (a - b).abs() < 1e-6), "{} {p:?}", cfa.name());
            }
        }
    }

    #[test]
    fn horizontal_ramp_is_linear_inside() {
        let cfa = Cfa::bayer("RGGB").unwrap();
        let f = |x: usize, _| [x as f32, x as f32 * 0.5, 2.0 * x as f32];
        let m = mosaic_of(10, 10, &cfa, f);
        let img = bilinear(&m, 10, 10, &cfa);
        for y in 1..9 {
            for x in 1..9 {
                let p = img.get(x, y);
                let e = f(x, y);
                assert!(p.iter().zip(e).all(|(a, b)| (a - b).abs() < 1e-5), "({x},{y}) {p:?} vs {e:?}");
            }
        }
    }

    #[test]
    fn half_size_bins_cells() {
        let cfa = Cfa::bayer("GRBG").unwrap();
        let m = mosaic_of(4, 4, &cfa, |x, y| [1.0, (x + y) as f32, 3.0]);
        let img = half_size(&m, 4, 4, &cfa).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        // Cell (0,0): greens at (0,0) and (1,1) -> mean of 0 and 2.
        assert_eq!(img.get(0, 0), [1.0, 1.0, 3.0]);
        assert!(half_size(&m, 4, 4, &xtrans()).is_none());
    }

    #[test]
    fn shifted_pattern() {
        let cfa = Cfa::bayer("RGGB").unwrap();
        assert_eq!(cfa.shifted(1, 0).name(), "GRBG");
        assert_eq!(cfa.shifted(1, 1).name(), "BGGR");
    }
}
