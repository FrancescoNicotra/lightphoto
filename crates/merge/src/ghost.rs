//! Ghost removal: where something moved between the shots, each moving
//! region takes a single source shot.

use align::Transform;

/// Where one shot is the single source, as a feathered weight `0..=1` at
/// half size, sampled bilinearly at full resolution.
#[derive(Clone, Debug)]
pub struct SourceMask {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    /// Full-resolution coordinates → half-size coordinates (pixel centres at +0.5).
    pub full_to_half: Transform,
}

impl SourceMask {
    #[inline]
    pub fn at_half(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    #[inline]
    pub fn at_full(&self, x: usize, y: usize) -> f32 {
        let m = &self.full_to_half.0;
        let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
        // An axis-aligned scale and offset.
        let u = (m[0][0] * fx + m[0][2] - 0.5).clamp(0.0, (self.width - 1) as f64);
        let v = (m[1][1] * fy + m[1][2] - 0.5).clamp(0.0, (self.height - 1) as f64);
        let (x0, y0) = (u.floor() as usize, v.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = ((u - x0 as f64) as f32, (v - y0 as f64) as f32);
        let d = |x: usize, y: usize| self.data[y * self.width + x];
        let top = d(x0, y0) + tx * (d(x1, y0) - d(x0, y0));
        let bot = d(x0, y1) + tx * (d(x1, y1) - d(x0, y1));
        top + ty * (bot - top)
    }
}
