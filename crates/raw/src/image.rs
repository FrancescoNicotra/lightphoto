//! A floating point RGB image plus the geometric operations the decoder
//! needs: crop, EXIF orientation and area downscaling.

use rayon::prelude::*;

/// Interleaved RGB, row-major, linear light. After decoding the values are
/// linear ProPhoto RGB (D50); 1.0 is the sensor clip point at the as-shot
/// white balance, before any exposure.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[f32; 3]>,
}

/// EXIF / TIFF orientation tag values 1–8.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Normal = 1,
    FlipH = 2,
    Rotate180 = 3,
    FlipV = 4,
    Transpose = 5,
    Rotate90 = 6,
    Transverse = 7,
    Rotate270 = 8,
}

impl Orientation {
    pub fn from_exif(v: u16) -> Self {
        match v {
            2 => Self::FlipH,
            3 => Self::Rotate180,
            4 => Self::FlipV,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Normal,
        }
    }

    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270)
    }
}

impl Image {
    pub fn new(width: usize, height: usize) -> Self {
        Image { width, height, pixels: vec![[0.0; 3]; width * height] }
    }

    pub fn from_fn(width: usize, height: usize, f: impl Fn(usize, usize) -> [f32; 3] + Sync) -> Self {
        let mut img = Self::new(width, height);
        img.pixels.par_chunks_mut(width.max(1)).enumerate().for_each(|(y, row)| {
            for (x, p) in row.iter_mut().enumerate() {
                *p = f(x, y);
            }
        });
        img
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> [f32; 3] {
        self.pixels[y * self.width + x]
    }

    pub fn map(&mut self, f: impl Fn([f32; 3]) -> [f32; 3] + Sync) {
        self.pixels.par_iter_mut().for_each(|p| *p = f(*p));
    }

    /// The `w`×`h` region at (`x`, `y`), clamped to the image.
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Image {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);
        Image::from_fn(w, h, |cx, cy| self.get(x + cx, y + cy))
    }

    /// Applies an EXIF orientation, giving the image as it should be displayed.
    pub fn oriented(self, o: Orientation) -> Image {
        if o == Orientation::Normal {
            return self;
        }
        let (w, h) = (self.width, self.height);
        let (ow, oh) = if o.swaps_axes() { (h, w) } else { (w, h) };
        Image::from_fn(ow, oh, |x, y| {
            let (sx, sy) = match o {
                Orientation::Normal => (x, y),
                Orientation::FlipH => (w - 1 - x, y),
                Orientation::Rotate180 => (w - 1 - x, h - 1 - y),
                Orientation::FlipV => (x, h - 1 - y),
                Orientation::Transpose => (y, x),
                Orientation::Rotate90 => (y, h - 1 - x),
                Orientation::Transverse => (w - 1 - y, h - 1 - x),
                Orientation::Rotate270 => (w - 1 - y, x),
            };
            self.get(sx, sy)
        })
    }

    /// Box-filter downscale so the long edge is at most `long_edge` pixels.
    /// Every source pixel contributes by the area it covers, so the mean is
    /// preserved exactly. Never upscales.
    pub fn downscale(&self, long_edge: usize) -> Image {
        let long = self.width.max(self.height);
        if long <= long_edge || long_edge == 0 {
            return self.clone();
        }
        let s = long_edge as f64 / long as f64;
        let ow = ((self.width as f64 * s).round() as usize).max(1);
        let oh = ((self.height as f64 * s).round() as usize).max(1);
        self.resize_area(ow, oh)
    }

    fn resize_area(&self, ow: usize, oh: usize) -> Image {
        let xs = area_weights(self.width, ow);
        let ys = area_weights(self.height, oh);
        // Horizontal pass into a ow × height buffer, then vertical.
        let mut tmp = Image::new(ow, self.height);
        tmp.pixels.par_chunks_mut(ow).enumerate().for_each(|(y, row)| {
            let src = &self.pixels[y * self.width..(y + 1) * self.width];
            for (out, taps) in row.iter_mut().zip(&xs) {
                *out = weighted(taps.iter().map(|&(i, w)| (src[i], w)));
            }
        });
        Image::from_fn(ow, oh, |x, y| weighted(ys[y].iter().map(|&(i, w)| (tmp.pixels[i * ow + x], w))))
    }
}

fn weighted(taps: impl Iterator<Item = ([f32; 3], f32)>) -> [f32; 3] {
    let mut acc = [0.0f32; 3];
    for (p, w) in taps {
        for c in 0..3 {
            acc[c] += p[c] * w;
        }
    }
    acc
}

/// For each output sample, the source samples it covers and their weights
/// (summing to 1).
fn area_weights(src: usize, dst: usize) -> Vec<Vec<(usize, f32)>> {
    let ratio = src as f64 / dst as f64;
    (0..dst)
        .map(|o| {
            let (a, b) = (o as f64 * ratio, (o + 1) as f64 * ratio);
            let mut taps = Vec::new();
            let mut i = a.floor() as usize;
            while (i as f64) < b && i < src {
                let cover = (b.min(i as f64 + 1.0) - a.max(i as f64)) / ratio;
                if cover > 1e-9 {
                    taps.push((i, cover as f32));
                }
                i += 1;
            }
            taps
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(w: usize, h: usize) -> Image {
        Image::from_fn(w, h, |x, y| [(y * w + x) as f32, 0.0, 0.0])
    }

    fn values(img: &Image) -> Vec<f32> {
        img.pixels.iter().map(|p| p[0]).collect()
    }

    #[test]
    fn orientations_match_exif() {
        // 0 1 2
        // 3 4 5
        let img = numbered(3, 2);
        let cases = [
            (Orientation::FlipH, vec![2., 1., 0., 5., 4., 3.]),
            (Orientation::Rotate180, vec![5., 4., 3., 2., 1., 0.]),
            (Orientation::FlipV, vec![3., 4., 5., 0., 1., 2.]),
            (Orientation::Transpose, vec![0., 3., 1., 4., 2., 5.]),
            (Orientation::Rotate90, vec![3., 0., 4., 1., 5., 2.]), // 90° clockwise
            (Orientation::Transverse, vec![5., 2., 4., 1., 3., 0.]),
            (Orientation::Rotate270, vec![2., 5., 1., 4., 0., 3.]),
        ];
        for (o, expected) in cases {
            let out = img.clone().oriented(o);
            assert_eq!((out.width, out.height), if o.swaps_axes() { (2, 3) } else { (3, 2) });
            assert_eq!(values(&out), expected, "{o:?}");
        }
    }

    #[test]
    fn crop_is_clamped() {
        let c = numbered(4, 4).crop(1, 2, 10, 10);
        assert_eq!((c.width, c.height), (3, 2));
        assert_eq!(values(&c), vec![9., 10., 11., 13., 14., 15.]);
    }

    #[test]
    fn downscale_preserves_mean_and_aspect() {
        let img = Image::from_fn(301, 200, |x, y| [((x * 7 + y * 13) % 17) as f32, 1.0, 0.5]);
        let small = img.downscale(97);
        assert_eq!((small.width, small.height), (97, 64));
        let mean = |i: &Image| i.pixels.iter().map(|p| p[0] as f64).sum::<f64>() / i.pixels.len() as f64;
        // Equal only up to the rounding of the output size.
        assert!((mean(&img) - mean(&small)).abs() < 0.05, "{} vs {}", mean(&img), mean(&small));
        assert!(small.pixels.iter().all(|p| (p[1] - 1.0).abs() < 1e-5));
    }
}
