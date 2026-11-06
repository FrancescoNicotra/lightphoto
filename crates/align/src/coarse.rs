//! Coarse initialisation: exhaustive integer translation search by weighted
//! normalised cross-correlation (NCC) on the coarsest pyramid level.

use rayon::prelude::*;

use crate::Plane;

/// Shifts whose overlap carries less than this fraction of the weight of a
/// full overlap are not considered (their NCC is too noisy to trust).
const MIN_OVERLAP: f64 = 0.4;

/// Best integer shift `d` (in pixels of this level) such that
/// `moving(x + d) ≈ reference(x)`, searched within `±radius` of zero on each
/// axis. Returns `None` if no shift has enough overlap or texture.
pub(crate) fn translation(r: &Plane, m: &Plane, radius: usize) -> Option<(isize, isize, f64)> {
    let (w, h) = (r.width, r.height);
    let rx = radius.min(w / 2) as isize;
    let ry = radius.min(h / 2) as isize;
    // Weight expected from a full overlap: Σ w_ref · mean(w_moving).
    let sum = |p: &Plane| {
        p.weight
            .as_ref()
            .map_or((w * h) as f64, |v| v.iter().map(|&x| x as f64).sum())
    };
    let total = sum(r) * sum(m) / (w * h) as f64;
    if total <= 0.0 {
        return None;
    }
    let shifts: Vec<(isize, isize)> = (-ry..=ry)
        .flat_map(|dy| (-rx..=rx).map(move |dx| (dx, dy)))
        .collect();
    shifts
        .par_iter()
        .filter_map(|&(dx, dy)| {
            let c = ncc(r, m, dx, dy, total)?;
            Some((dx, dy, c))
        })
        // Prefer the higher NCC; on ties the smaller shift, for determinism.
        .reduce_with(|a, b| {
            let key = |t: &(isize, isize, f64)| (t.2, -(t.0.abs() + t.1.abs()));
            if key(&b) > key(&a) {
                b
            } else {
                a
            }
        })
}

fn ncc(r: &Plane, m: &Plane, dx: isize, dy: isize, total: f64) -> Option<f64> {
    let (w, h) = (r.width as isize, r.height as isize);
    let (x0, x1) = (0.max(-dx), w.min(w - dx));
    let (y0, y1) = (0.max(-dy), h.min(h - dy));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (mut sw, mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0f64, 0.0, 0.0, 0.0, 0.0, 0.0);
    let ones = vec![1.0f32; (x1 - x0) as usize];
    for y in y0..y1 {
        let (xa, xb) = ((y * w + x0) as usize, ((y + dy) * w + x0 + dx) as usize);
        let n = (x1 - x0) as usize;
        let (ra, rb) = (&r.data[xa..xa + n], &m.data[xb..xb + n]);
        let wa = r.weight.as_ref().map_or(&ones[..], |v| &v[xa..xa + n]);
        let wb = m.weight.as_ref().map_or(&ones[..], |v| &v[xb..xb + n]);
        let (mut w_, mut a_, mut b_, mut aa, mut bb, mut ab) = (0.0f32, 0.0f32, 0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let wt = wa[i] * wb[i];
            let (a, b) = (ra[i], rb[i]);
            w_ += wt;
            a_ += wt * a;
            b_ += wt * b;
            aa += wt * a * a;
            bb += wt * b * b;
            ab += wt * a * b;
        }
        sw += w_ as f64;
        sa += a_ as f64;
        sb += b_ as f64;
        saa += aa as f64;
        sbb += bb as f64;
        sab += ab as f64;
    }
    if sw < MIN_OVERLAP * total {
        return None;
    }
    let caa = saa - sa * sa / sw;
    let cbb = sbb - sb * sb / sw;
    let cab = sab - sa * sb / sw;
    let den = (caa * cbb).sqrt();
    (den > 1e-12 * sw).then(|| cab / den)
}
