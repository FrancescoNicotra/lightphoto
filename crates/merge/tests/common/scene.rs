//! A deterministic scene of continuous radiance (camera RGB, reference
//! pixel coordinates with centres at +0.5) and shots of it under known
//! exposures, transforms and noise.

use align::Transform;

pub const NEUTRAL: [f64; 3] = [0.55, 1.0, 0.72];
/// Photon noise: variance of a normalised value v is v / FULL_WELL.
pub const FULL_WELL: f64 = 30000.0;
/// Read noise, normalised.
pub const READ_NOISE: f64 = 3.0 / 16383.0;
/// Radiance of the light source: clipped in every shot.
pub const LIGHT: f64 = 1024.0;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

pub fn hash(seed: u64, a: i64, b: i64, c: u64) -> u64 {
    mix(seed ^ mix(a as u64 ^ mix(b as u64 ^ mix(c))))
}

pub fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Standard normal from two hashes.
pub fn gauss(seed: u64, x: i64, y: i64, c: u64) -> f64 {
    let h1 = unit(hash(seed, x, y, 2 * c)).max(1e-12);
    let h2 = unit(hash(seed, x, y, 2 * c + 1));
    (-2.0 * h1.ln()).sqrt() * (std::f64::consts::TAU * h2).cos()
}

/// A dark disk that moves between shots.
#[derive(Clone, Copy, Debug)]
pub struct Mover {
    pub radius: f64,
    /// Radiance (times the neutral).
    pub level: f64,
}

pub struct Scene {
    pub width: f64,
    pub height: f64,
    seed: u64,
    waves: Vec<(f64, f64, f64, f64)>,
    /// Flat neutral patch for the noise test: x0, y0, x1, y1, radiance.
    pub patch: (f64, f64, f64, f64, f64),
    /// Light source: cx, cy, radius.
    pub light: (f64, f64, f64),
    pub mover: Option<Mover>,
}

impl Scene {
    pub fn new(seed: u64, width: usize, height: usize) -> Self {
        let (w, h) = (width as f64, height as f64);
        let mut k = 0u64;
        let mut rnd = || {
            k += 1;
            unit(hash(seed, 3, 5, k))
        };
        let waves = (0..5)
            .map(|_| {
                let period = 14.0 + 50.0 * rnd();
                let dir = std::f64::consts::TAU * rnd();
                let f = std::f64::consts::TAU / period;
                (f * dir.cos(), f * dir.sin(), std::f64::consts::TAU * rnd(), 0.08 + 0.08 * rnd())
            })
            .collect();
        Scene {
            width: w,
            height: h,
            seed,
            waves,
            patch: (0.04 * w, 0.62 * h, 0.2 * w, 0.92 * h, 1.0 / 64.0),
            light: (0.9 * w, 0.2 * h, 0.03 * w),
            mover: None,
        }
    }

    fn value_noise(&self, x: f64, y: f64, cell: f64, octave: u64) -> f64 {
        let (u, v) = (x / cell, y / cell);
        let (i, j) = (u.floor(), v.floor());
        let (fx, fy) = (u - i, v - j);
        let fade = |t: f64| t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
        let (sx, sy) = (fade(fx), fade(fy));
        let g = |a: f64, b: f64| 2.0 * unit(hash(self.seed, a as i64, b as i64, octave)) - 1.0;
        let top = g(i, j) + sx * (g(i + 1.0, j) - g(i, j));
        let bot = g(i, j + 1.0) + sx * (g(i + 1.0, j + 1.0) - g(i, j + 1.0));
        top + sy * (bot - top)
    }

    /// Texture in EV, about ±0.7.
    fn texture(&self, x: f64, y: f64) -> f64 {
        let mut v = 0.4 * self.value_noise(x, y, 40.0, 0) + 0.25 * self.value_noise(x, y, 13.0, 1);
        for &(kx, ky, ph, amp) in &self.waves {
            v += amp * (kx * x + ky * y + ph).sin();
        }
        v
    }

    /// Static radiance at a point of the reference frame: a 14-stop
    /// horizontal gradient (2⁻⁸ … 2⁶) × texture × colour cells with sharp
    /// edges, a flat grey patch and a light source.
    pub fn radiance(&self, x: f64, y: f64) -> [f64; 3] {
        let (lx, ly, lr) = self.light;
        if (x - lx).hypot(y - ly) < lr {
            return NEUTRAL.map(|n| n * LIGHT);
        }
        let (px0, py0, px1, py1, level) = self.patch;
        if (px0..px1).contains(&x) && (py0..py1).contains(&y) {
            return NEUTRAL.map(|n| n * level);
        }
        let base = (-8.0 + 14.0 * (x / self.width).clamp(0.0, 1.0)).exp2();
        let tex = self.texture(x, y).exp2();
        // 8 × 6 colour cells.
        let (ci, cj) = ((x / self.width * 8.0).floor() as i64, (y / self.height * 6.0).floor() as i64);
        let tint: [f64; 3] = std::array::from_fn(|c| 0.45 + 0.55 * unit(hash(self.seed, ci, cj, 10 + c as u64)));
        std::array::from_fn(|c| base * tex * tint[c] * NEUTRAL[c])
    }

    /// Radiance with the moving disk at `centre` (reference coordinates).
    pub fn radiance_with(&self, x: f64, y: f64, centre: Option<(f64, f64)>) -> [f64; 3] {
        if let (Some(m), Some((cx, cy))) = (self.mover, centre) {
            if (x - cx).hypot(y - cy) < m.radius {
                return NEUTRAL.map(|n| n * m.level);
            }
        }
        self.radiance(x, y)
    }
}

/// One shot of the bracket.
#[derive(Clone, Debug)]
pub struct ShotSpec {
    /// True exposure relative to the scene (the reference is usually 1).
    pub exposure: f64,
    /// EXIF exposure time, if any (seconds); ISO 100, f/8.
    pub exif_time: Option<f64>,
    /// Reference → this shot.
    pub transform: Transform,
    /// Centre of the moving disk in this shot (reference coordinates).
    pub mover: Option<(f64, f64)>,
    pub noise_seed: u64,
    pub noise: bool,
}

impl ShotSpec {
    pub fn new(exposure: f64, seed: u64) -> Self {
        ShotSpec { exposure, exif_time: Some(exposure / 100.0), transform: Transform::IDENTITY, mover: None, noise_seed: seed, noise: true }
    }

    /// The normalised sensor value of channel `c` at shot pixel centre (x, y)
    /// (`sx`, `sy` index the noise): clipped, noisy, quantised to 14 bits.
    #[allow(clippy::too_many_arguments)]
    pub fn sample(&self, scene: &Scene, inv: &Transform, x: f64, y: f64, c: usize, sx: i64, sy: i64) -> f64 {
        let (u, v) = inv.apply(x, y);
        let r = scene.radiance_with(u, v, self.mover)[c] * self.exposure;
        let n = if self.noise {
            (r.max(0.0) / FULL_WELL + READ_NOISE * READ_NOISE).sqrt() * gauss(self.noise_seed, sx, sy, c as u64)
        } else {
            0.0
        };
        (((r + n).min(1.0) * 16383.0).round() / 16383.0).max(-0.01)
    }
}
