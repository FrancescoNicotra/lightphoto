//! Deterministic synthetic scenes for the alignment tests.
//!
//! The scene is a *continuous* function of the plane, so a shot under any
//! transform is sampled exactly (no interpolation error in the ground truth).

use align::{Plane, Transform};

/// 64-bit mix (splitmix64 finaliser).
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn hash(seed: u64, a: i64, b: i64, c: u64) -> u64 {
    mix(seed ^ mix(a as u64 ^ mix(b as u64 ^ mix(c))))
}

/// Uniform in [0, 1).
fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

pub struct Scene {
    seed: u64,
    /// (kx, ky, phase, amplitude)
    waves: Vec<(f64, f64, f64, f64)>,
    /// (cx, cy, radius, amplitude)
    disks: Vec<(f64, f64, f64, f64)>,
    /// (cell size, amplitude)
    octaves: Vec<(f64, f64)>,
    size: (f64, f64),
}

impl Scene {
    /// A textured scene roughly covering `width × height` (it extends
    /// smoothly beyond).
    pub fn new(seed: u64, width: usize, height: usize) -> Self {
        let (w, h) = (width as f64, height as f64);
        let mut k = 0u64;
        let mut rnd = || {
            k += 1;
            unit(hash(seed, 7, 11, k))
        };
        let waves = (0..6)
            .map(|_| {
                let period = 9.0 + 60.0 * rnd();
                let dir = std::f64::consts::TAU * rnd();
                let f = std::f64::consts::TAU / period;
                (
                    f * dir.cos(),
                    f * dir.sin(),
                    std::f64::consts::TAU * rnd(),
                    0.06 + 0.08 * rnd(),
                )
            })
            .collect();
        let disks = (0..12)
            .map(|_| {
                (
                    w * rnd(),
                    h * rnd(),
                    8.0 + 0.08 * w.min(h) * rnd(),
                    0.6 * rnd() - 0.3,
                )
            })
            .collect();
        Scene {
            seed,
            waves,
            disks,
            octaves: vec![(40.0, 0.5), (12.0, 0.3), (5.0, 0.15)],
            size: (w, h),
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

    /// Scene value at a continuous point (pixel edges at integers).
    pub fn eval(&self, x: f64, y: f64) -> f64 {
        let mut v = 0.2 * (x / self.size.0) - 0.1 * (y / self.size.1);
        for (o, &(cell, amp)) in self.octaves.iter().enumerate() {
            v += amp * self.value_noise(x, y, cell, o as u64);
        }
        for &(kx, ky, ph, amp) in &self.waves {
            v += amp * (kx * x + ky * y + ph).sin();
        }
        for &(cx, cy, r, amp) in &self.disks {
            let d = (x - cx).hypot(y - cy);
            v += amp * 0.5 * (1.0 + ((r - d) / 1.2).tanh());
        }
        v
    }

    /// A `width × height` shot of the scene: pixel centre `q` shows the scene
    /// point `t⁻¹(q)` (`t` maps the reference to this shot), then
    /// `gain · v + offset` plus Gaussian noise of standard deviation `noise`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        width: usize,
        height: usize,
        t: &Transform,
        gain: f64,
        offset: f64,
        noise: f64,
        noise_seed: u64,
    ) -> Plane {
        let inv = t.inverse().unwrap();
        Plane::from_fn(width, height, |x, y| {
            let (u, v) = inv.apply(x as f64 + 0.5, y as f64 + 0.5);
            let n = if noise > 0.0 {
                let h1 = unit(hash(noise_seed, x as i64, y as i64, 1)).max(1e-12);
                let h2 = unit(hash(noise_seed, x as i64, y as i64, 2));
                (-2.0 * h1.ln()).sqrt() * (std::f64::consts::TAU * h2).cos()
            } else {
                0.0
            };
            (gain * self.eval(u, v) + offset + noise * n) as f32
        })
    }
}

/// 2×2 box binning, like a half-size demosaic.
pub fn bin2(p: &Plane) -> Plane {
    let (w, h) = (p.width / 2, p.height / 2);
    Plane::from_fn(w, h, |x, y| {
        let i = 2 * y * p.width + 2 * x;
        0.25 * (p.data[i] + p.data[i + 1] + p.data[i + p.width] + p.data[i + p.width + 1])
    })
}
