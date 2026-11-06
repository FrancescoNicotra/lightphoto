//! Projective transforms between pixel coordinate systems.
//!
//! Coordinates are continuous with the origin at the top-left corner of the
//! top-left pixel, so the centre of pixel `(i, j)` is at `(i + 0.5, j + 0.5)`.
//! With this convention a change of resolution by a factor `f` is the plain
//! scaling `x' = f·x` (no half-pixel offset), see [`Transform::scaled`].

/// A 3×3 projective transform from **reference** coordinates to **moving**
/// (shot) coordinates: the reference point `p` shows the same scene point as
/// the moving image at `T(p)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform(pub [[f64; 3]; 3]);

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);

    pub fn translation(tx: f64, ty: f64) -> Self {
        Transform([[1.0, 0.0, tx], [0.0, 1.0, ty], [0.0, 0.0, 1.0]])
    }

    pub fn scaling(sx: f64, sy: f64) -> Self {
        Transform([[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]])
    }

    /// Rotation by `angle` radians (counter-clockwise in a y-down image is
    /// clockwise on screen) followed by uniform `scale`, both about `(cx, cy)`,
    /// then a translation by `(tx, ty)`.
    pub fn similarity(scale: f64, angle: f64, cx: f64, cy: f64, tx: f64, ty: f64) -> Self {
        let (s, c) = angle.sin_cos();
        let (a, b) = (scale * c, scale * s);
        Transform([
            [a, -b, cx - a * cx + b * cy + tx],
            [b, a, cy - b * cx - a * cy + ty],
            [0.0, 0.0, 1.0],
        ])
    }

    /// Maps a point. Returns non-finite values for points sent to infinity.
    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let m = &self.0;
        let w = m[2][0] * x + m[2][1] * y + m[2][2];
        (
            (m[0][0] * x + m[0][1] * y + m[0][2]) / w,
            (m[1][0] * x + m[1][1] * y + m[1][2]) / w,
        )
    }

    /// `self ∘ other`: applies `other` first, then `self`.
    pub fn compose(&self, other: &Transform) -> Transform {
        let (a, b) = (&self.0, &other.0);
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
            }
        }
        Transform(r).normalized()
    }

    /// Inverse transform, or `None` if the matrix is singular.
    pub fn inverse(&self) -> Option<Transform> {
        let m = &self.0;
        let c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
        let c01 = m[1][2] * m[2][0] - m[1][0] * m[2][2];
        let c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];
        let det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02;
        let scale = m.iter().flatten().fold(0.0f64, |a, v| a.max(v.abs()));
        if !det.is_finite() || det.abs() <= 1e-14 * scale.powi(3) {
            return None;
        }
        let d = 1.0 / det;
        Some(
            Transform([
                [
                    c00 * d,
                    (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
                    (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
                ],
                [
                    c01 * d,
                    (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
                    (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
                ],
                [
                    c02 * d,
                    (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
                    (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
                ],
            ])
            .normalized(),
        )
    }

    /// The same transform expressed for images `factor` times larger:
    /// `S · T · S⁻¹` with `S = diag(factor, factor, 1)`. Because pixel centres
    /// sit at `+0.5`, no offset is needed: pixel `i` of a half-size image
    /// covers full-size pixels `2i` and `2i + 1`, whose shared edge `2i + 1`
    /// is exactly `2 · (i + 0.5)`.
    pub fn scaled(&self, factor: f64) -> Transform {
        let s = Transform::scaling(factor, factor);
        let s_inv = Transform::scaling(1.0 / factor, 1.0 / factor);
        s.compose(self).compose(&s_inv)
    }

    /// Half-size → full-resolution: [`Transform::scaled`] by 2.
    pub fn half_to_full(&self) -> Transform {
        self.scaled(2.0)
    }

    /// Scales the matrix so that `m[2][2] = 1` (when it is not ~0).
    pub fn normalized(mut self) -> Transform {
        let w = self.0[2][2];
        if w.abs() > 1e-300 && w != 1.0 {
            for v in self.0.iter_mut().flatten() {
                *v /= w;
            }
        }
        self
    }

    /// The corners of a `width × height` image (outer pixel edges).
    pub fn corners(width: usize, height: usize) -> [(f64, f64); 4] {
        let (w, h) = (width as f64, height as f64);
        [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]
    }

    /// Largest displacement, in pixels, of the four image corners.
    pub fn max_corner_shift(&self, width: usize, height: usize) -> f64 {
        Self::corners(width, height)
            .iter()
            .map(|&(x, y)| {
                let (u, v) = self.apply(x, y);
                (u - x).hypot(v - y)
            })
            .fold(0.0, |a, d| {
                if d.is_finite() {
                    a.max(d)
                } else {
                    f64::INFINITY
                }
            })
    }

    /// Largest displacement of the four corners *relative to the centre's*:
    /// how much the image is deformed (rotated, scaled, tilted) rather than
    /// shifted.
    pub fn max_corner_deformation(&self, width: usize, height: usize) -> f64 {
        let (cx, cy) = (0.5 * width as f64, 0.5 * height as f64);
        let (mx, my) = self.apply(cx, cy);
        let (dx, dy) = (mx - cx, my - cy);
        Self::corners(width, height)
            .iter()
            .map(|&(x, y)| {
                let (u, v) = self.apply(x, y);
                (u - x - dx).hypot(v - y - dy)
            })
            .fold(0.0, |a, d| {
                if d.is_finite() {
                    a.max(d)
                } else {
                    f64::INFINITY
                }
            })
    }

    /// RMS distance between where `self` and `other` send the four corners
    /// of a `width × height` image: the usual accuracy metric.
    pub fn corner_rms(&self, other: &Transform, width: usize, height: usize) -> f64 {
        let sum: f64 = Self::corners(width, height)
            .iter()
            .map(|&(x, y)| {
                let (a, b) = self.apply(x, y);
                let (c, d) = other.apply(x, y);
                (a - c).powi(2) + (b - d).powi(2)
            })
            .sum();
        (sum / 4.0).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inverse_and_compose() {
        let t = Transform([[1.01, 0.02, 3.0], [-0.015, 0.99, -2.0], [1e-5, -2e-5, 1.0]]);
        let i = t.inverse().unwrap();
        let id = t.compose(&i);
        for r in 0..3 {
            for c in 0..3 {
                let e = if r == c { 1.0 } else { 0.0 };
                assert!((id.0[r][c] - e).abs() < 1e-12);
            }
        }
        let (x, y) = t.apply(100.0, 50.0);
        let (u, v) = i.apply(x, y);
        assert!((u - 100.0).abs() < 1e-9 && (v - 50.0).abs() < 1e-9);
    }

    #[test]
    fn similarity_fixes_centre() {
        let t = Transform::similarity(1.1, 0.3, 40.0, 30.0, 0.0, 0.0);
        let (x, y) = t.apply(40.0, 30.0);
        assert!((x - 40.0).abs() < 1e-12 && (y - 30.0).abs() < 1e-12);
    }
}
