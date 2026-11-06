//! Single-channel planes, the Gaussian pyramid and image gradients.

use rayon::prelude::*;

/// A single-channel image with an optional per-pixel weight in `0..=1`
/// (0 = ignore the pixel, e.g. clipped or below the noise floor).
#[derive(Clone, Debug)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    /// Row-major, `width × height` values.
    pub data: Vec<f32>,
    /// Row-major, `width × height` weights; `None` = all pixels weigh 1.
    pub weight: Option<Vec<f32>>,
}

impl Plane {
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Self {
        Plane {
            width,
            height,
            data,
            weight: None,
        }
    }

    pub fn with_weight(mut self, weight: Vec<f32>) -> Self {
        self.weight = Some(weight);
        self
    }

    /// Builds a plane by evaluating `f(x, y)` at every pixel index.
    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize, usize) -> f32 + Sync) -> Self {
        let mut data = vec![0.0; width * height];
        data.par_chunks_mut(width.max(1))
            .enumerate()
            .for_each(|(y, row)| {
                for (x, v) in row.iter_mut().enumerate() {
                    *v = f(x, y);
                }
            });
        Plane::new(width, height, data)
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.data.len() == self.width * self.height
            && self
                .weight
                .as_ref()
                .is_none_or(|w| w.len() == self.data.len())
    }
}

/// The pyramid stops once the long side is at most this many pixels, so the
/// coarsest level is ~100–190 px.
const COARSEST_LONG_SIDE: usize = 192;
/// ... or before the short side drops below this.
const MIN_SHORT_SIDE: usize = 24;

/// Pyramid levels, finest (index 0, same size as the input) first. Each level
/// halves the previous one, so coordinates scale exactly by 2 between levels.
pub(crate) fn pyramid(plane: &Plane) -> Vec<Plane> {
    let mut levels = vec![finest_level(plane)];
    loop {
        let last = levels.last().unwrap();
        let (w, h) = (last.width, last.height);
        if w.max(h) <= COARSEST_LONG_SIDE || w.min(h) / 2 < MIN_SHORT_SIDE {
            break;
        }
        let next = downsample(last);
        levels.push(next);
    }
    levels
}

/// Level 0: a light [1 2 1] blur against noise (both images get the same) and
/// the weight eroded by one pixel, because blur and gradients read neighbours.
fn finest_level(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let left = |x: usize| x.saturating_sub(1);
    let right = |x: usize| (x + 1).min(w - 1);
    let up = |y: usize| y.saturating_sub(1) * w;
    let down = |y: usize| (y + 1).min(h - 1) * w;
    let horiz = map_pixels(w, h, |x, y| {
        let r = &p.data[y * w..(y + 1) * w];
        0.25 * r[left(x)] + 0.5 * r[x] + 0.25 * r[right(x)]
    });
    let data = map_pixels(w, h, |x, y| {
        0.25 * horiz[up(y) + x] + 0.5 * horiz[y * w + x] + 0.25 * horiz[down(y) + x]
    });
    let weight = p.weight.as_ref().map(|wt| {
        let horiz = map_pixels(w, h, |x, y| {
            let r = &wt[y * w..(y + 1) * w];
            r[left(x)].min(r[x]).min(r[right(x)])
        });
        map_pixels(w, h, |x, y| {
            horiz[up(y) + x]
                .min(horiz[y * w + x])
                .min(horiz[down(y) + x])
        })
    });
    Plane {
        width: w,
        height: h,
        data,
        weight,
    }
}

fn map_pixels(w: usize, h: usize, f: impl Fn(usize, usize) -> f32 + Sync) -> Vec<f32> {
    Plane::from_fn(w, h, f).data
}

/// Halves a level with the symmetric [1 3 3 1]/8 kernel centred between input
/// pixels `2i` and `2i + 1`, i.e. on the centre of output pixel `i`. The
/// weight is filtered the same way, so a pixel blurred with masked
/// neighbours counts proportionally less.
fn downsample(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let (nw, nh) = (w / 2, h / 2);
    let idx = |i: isize, n: usize| i.clamp(0, n as isize - 1) as usize;
    const K: [f32; 4] = [0.125, 0.375, 0.375, 0.125];

    let run = |src: &[f32]| -> Vec<f32> {
        // Horizontal: w×h → nw×h.
        let mut tmp = vec![0.0f32; nw * h];
        tmp.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
            let s = &src[y * w..(y + 1) * w];
            for (x, v) in row.iter_mut().enumerate() {
                let x0 = 2 * x as isize - 1;
                *v = (0..4).map(|k| K[k as usize] * s[idx(x0 + k, w)]).sum();
            }
        });
        // Vertical: nw×h → nw×nh.
        let mut out = vec![0.0f32; nw * nh];
        out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
            let y0 = 2 * y as isize - 1;
            let rows: [&[f32]; 4] = std::array::from_fn(|k| {
                let r = idx(y0 + k as isize, h);
                &tmp[r * nw..(r + 1) * nw]
            });
            for (x, v) in row.iter_mut().enumerate() {
                *v = rows.iter().zip(K).map(|(r, k)| k * r[x]).sum();
            }
        });
        out
    };

    Plane {
        width: nw,
        height: nh,
        data: run(&p.data),
        weight: p.weight.as_ref().map(|wt| run(wt)),
    }
}

/// Central-difference gradients (per pixel); zero on the outer border.
pub(crate) fn gradients(p: &Plane) -> (Vec<f32>, Vec<f32>) {
    let (w, h) = (p.width, p.height);
    let mut gx = vec![0.0f32; w * h];
    let mut gy = vec![0.0f32; w * h];
    gx.par_chunks_mut(w)
        .zip(gy.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (rx, ry))| {
            if y == 0 || y + 1 >= h {
                return;
            }
            let d = &p.data;
            for x in 1..w.saturating_sub(1) {
                let i = y * w + x;
                rx[x] = 0.5 * (d[i + 1] - d[i - 1]);
                ry[x] = 0.5 * (d[i + w] - d[i - w]);
            }
        });
    (gx, gy)
}
