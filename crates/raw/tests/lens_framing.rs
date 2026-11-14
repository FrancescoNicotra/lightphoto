//! Lens correction against the camera: the GX80's JPEG is corrected in the
//! camera with the same parameters the RW2 carries, so once corrected our
//! develop must land on it pixel for pixel. Offsets are measured patch by
//! patch with normalised cross-correlation on a grid over the frame.

use raw::{DecodeOptions, Image, Photo};
use test_samples::Set;

/// Square root of the luminance: the two renders differ in tone curve, the
/// correlation does not mind an affine change and the root evens it out.
fn plane(img: &Image) -> Vec<f32> {
    img.pixels.iter().map(|p| (0.3 * p[0] + 0.6 * p[1] + 0.1 * p[2]).max(0.0).sqrt()).collect()
}

struct Patch<'a> {
    img: &'a [f32],
    w: usize,
    at: (usize, usize),
}

impl Patch<'_> {
    /// The patch of radius `half` centred at `at` + (dx, dy), normalised to
    /// zero mean and unit norm.
    fn values(&self, half: usize, dx: isize, dy: isize) -> Vec<f32> {
        let (cx, cy) = (self.at.0 as isize + dx, self.at.1 as isize + dy);
        let mut v = Vec::with_capacity((2 * half + 1).pow(2));
        for yy in cy - half as isize..=cy + half as isize {
            for xx in cx - half as isize..=cx + half as isize {
                v.push(self.img[yy as usize * self.w + xx as usize]);
            }
        }
        let m = v.iter().sum::<f32>() / v.len() as f32;
        let s = v.iter().map(|q| (q - m).powi(2)).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|q| (q - m) / s).collect()
    }
}

/// Where `reference` best matches in `search`, within ±`range` pixels of
/// its position, refined with a parabola; `None` if weak or at the edge of
/// the range.
fn offset(search: &Patch, reference: &Patch, half: usize, range: isize) -> Option<(f32, f32)> {
    let r = reference.values(half, 0, 0);
    let n = (2 * range + 1) as usize;
    let mut scores = vec![f32::MIN; n * n];
    for dy in -range..=range {
        for dx in -range..=range {
            let p = search.values(half, dx, dy);
            scores[(dy + range) as usize * n + (dx + range) as usize] = p.iter().zip(&r).map(|(p, q)| p * q).sum();
        }
    }
    let (best, &score) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let (bx, by) = (best % n, best / n);
    if score < 0.6 || bx == 0 || by == 0 || bx == n - 1 || by == n - 1 {
        return None;
    }
    let sub = |l: f32, c: f32, r: f32| 0.5 * (l - r) / (l - 2.0 * c + r).min(-1e-6);
    let fx = sub(scores[by * n + bx - 1], score, scores[by * n + bx + 1]);
    let fy = sub(scores[(by - 1) * n + bx], score, scores[(by + 1) * n + bx]);
    Some((bx as f32 - range as f32 + fx, by as f32 - range as f32 + fy))
}

/// Matched positions on a grid: (our position, its position in the
/// camera image), with `origin` where our (0, 0) roughly falls in it.
fn matches(ours: &Image, camera: &Image, origin: [usize; 2]) -> Vec<Match> {
    let (a, b) = (plane(ours), plane(camera));
    let (half, range) = (12usize, 24isize);
    let margin = half + range as usize + 1;
    let (w, h) = (ours.width, ours.height);
    let mut out = Vec::new();
    for gy in 0..9 {
        for gx in 0..12 {
            let x = margin + gx * (w - 2 * margin) / 11;
            let y = margin + gy * (h - 2 * margin) / 8;
            let (cx, cy) = (x + origin[0], y + origin[1]);
            if cx + margin >= camera.width || cy + margin >= camera.height {
                continue;
            }
            let ours_patch = Patch { img: &a, w, at: (x, y) };
            let camera_patch = Patch { img: &b, w: camera.width, at: (cx, cy) };
            if let Some((dx, dy)) = offset(&camera_patch, &ours_patch, half, range) {
                out.push(([x as f32, y as f32], [cx as f32 + dx, cy as f32 + dy]));
            }
        }
    }
    out
}

/// Our position and the matching one in the camera image.
type Match = ([f32; 2], [f32; 2]);

/// Least-squares `q = s·p + t`; returns s and the residuals.
fn fit_similarity(m: &[Match]) -> (f32, Vec<[f32; 2]>) {
    let n = m.len() as f32;
    let mean = |f: &dyn Fn(&Match) -> f32| m.iter().map(f).sum::<f32>() / n;
    let (px, py, qx, qy) = (mean(&|v| v.0[0]), mean(&|v| v.0[1]), mean(&|v| v.1[0]), mean(&|v| v.1[1]));
    let num: f32 = m.iter().map(|(p, q)| (p[0] - px) * (q[0] - qx) + (p[1] - py) * (q[1] - qy)).sum();
    let den: f32 = m.iter().map(|(p, _)| (p[0] - px).powi(2) + (p[1] - py).powi(2)).sum();
    let s = num / den;
    let res = m.iter().map(|(p, q)| [q[0] - qx - s * (p[0] - px), q[1] - qy - s * (p[1] - py)]).collect();
    (s, res)
}

/// The full RW2 (4592 × 3448 from photosite (8, 8)) against the whole
/// 1920 × 1440 camera JPEG, or the versioned crop (1024 × 768 photosites
/// from (1696, 1328), with the RW2's correction as an OpcodeList3) against
/// the crop of that JPEG from (560, 400).
#[test]
fn corrected_raw_lands_on_the_camera_jpeg() {
    let sample = test_samples::gx80();
    let camera = Photo::load(&sample.camera_jpeg, &DecodeOptions::default()).unwrap().to_working(None);
    // Camera JPEG pixels per photosite, and where the RAW's (0, 0) falls.
    let k = 1920.0 / 4592.0;
    // Limits: |scale − 1|, residual rms and worst (camera JPEG pixels), how
    // much larger the uncorrected residual must be. Measured: full frame
    // 0.9995, 0.40, 1.33 px against 6.4 px uncorrected; crop (near the
    // centre, where the distortion is mostly a change of scale: 1.0025
    // uncorrected) 1.0001, 0.13, 0.37 px against 0.24 px.
    let (origin, limits) = match sample.set {
        Set::Full => ([0, 0], (0.002, 0.6, 2.0, 4.0)),
        Set::Ci => ([((1696.0 - 8.0) * k) as usize - 560, ((1328.0 - 8.0) * k) as usize - 400], (0.001, 0.25, 0.8, 1.5)),
    };
    let mut uncorrected_rms = 0.0;
    for no_lens_correction in [true, false] {
        let photo = Photo::load(&sample.raw, &DecodeOptions { no_lens_correction, ..Default::default() }).unwrap();
        assert_eq!(photo.info.raw.as_ref().unwrap().lens_correction.is_empty(), no_lens_correction);
        let ours = photo.to_working(None);
        let ours = ours.downscale((ours.width.max(ours.height) as f64 * k).round() as usize);
        let m = matches(&ours, &camera, origin);
        let (s, res) = fit_similarity(&m);
        let rms = (res.iter().map(|d| d[0] * d[0] + d[1] * d[1]).sum::<f32>() / res.len() as f32).sqrt();
        let worst = res.iter().map(|d| d[0].hypot(d[1])).fold(0.0, f32::max);
        eprintln!("lens correction {}: {} patches, scale {s:.4}, residual rms {rms:.2} px, worst {worst:.2} px", !no_lens_correction, m.len());
        if no_lens_correction {
            uncorrected_rms = rms;
            continue;
        }
        let (max_scale, max_rms, max_worst, ratio) = limits;
        assert!(m.len() > 50, "{}", m.len());
        assert!((s - 1.0).abs() < max_scale, "scale {s}");
        assert!(rms < max_rms && worst < max_worst, "rms {rms}, worst {worst}");
        assert!(uncorrected_rms > ratio * rms, "uncorrected {uncorrected_rms}, corrected {rms}");
    }
}
