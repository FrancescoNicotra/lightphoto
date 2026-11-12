//! Automatic crop: the largest rectangle with the image's proportions inside
//! the area every aligned shot covers. Written as `crs:Crop*`, so it stays
//! editable; the pixels outside are kept.

use crate::output::CropRect;

/// Long side of the grid the search runs on.
const GRID: usize = 512;

/// The crop for a `width × height` coverage mask (`true` = every shot covers
/// the pixel), or `None` when everything is covered.
pub fn auto_crop(covered: &[bool], width: usize, height: usize) -> Option<CropRect> {
    if covered.iter().all(|&c| c) || width == 0 || height == 0 {
        return None;
    }
    // Conservative reduction: a cell counts as covered only if all of it is.
    let f = width.max(height).div_ceil(GRID).max(1);
    let (gw, gh) = (width.div_ceil(f), height.div_ceil(f));
    let mut bad = vec![false; gw * gh];
    for y in 0..height {
        for x in 0..width {
            if !covered[y * width + x] {
                bad[(y / f) * gw + x / f] = true;
            }
        }
    }
    // Summed-area table of the uncovered cells.
    let mut sat = vec![0u32; (gw + 1) * (gh + 1)];
    for y in 0..gh {
        for x in 0..gw {
            sat[(y + 1) * (gw + 1) + x + 1] =
                u32::from(bad[y * gw + x]) + sat[y * (gw + 1) + x + 1] + sat[(y + 1) * (gw + 1) + x] - sat[y * (gw + 1) + x];
        }
    }
    let empty = |x: usize, y: usize, w: usize, h: usize| {
        let s = |x: usize, y: usize| sat[y * (gw + 1) + x];
        s(x + w, y + h) + s(x, y) == s(x + w, y) + s(x, y + h)
    };
    let aspect = width as f64 / height as f64;
    // Best position for a rectangle `rh` cells high: free, nearest the centre.
    let place = |rh: usize| -> Option<(usize, usize, usize, usize)> {
        let rw = ((rh as f64 * aspect).floor() as usize).min(gw);
        if rw == 0 || rh == 0 || rh > gh {
            return None;
        }
        let (cx, cy) = ((gw - rw) as f64 / 2.0, (gh - rh) as f64 / 2.0);
        (0..=gh - rh)
            .flat_map(|y| (0..=gw - rw).map(move |x| (x, y)))
            .filter(|&(x, y)| empty(x, y, rw, rh))
            .min_by(|a, b| {
                let d = |p: &(usize, usize)| (p.0 as f64 - cx).powi(2) + (p.1 as f64 - cy).powi(2);
                d(a).total_cmp(&d(b))
            })
            .map(|(x, y)| (x, y, rw, rh))
    };
    // Feasibility is monotonic in the size: binary search on the height.
    let (mut lo, mut hi) = (0usize, gh);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if place(mid).is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let (x, y, rw, rh) = place(lo)?;
    // Back to pixels, staying inside the image.
    let px = |v: usize, n: usize| (v * f).min(n) as f32;
    Some(CropRect {
        left: px(x, width) / width as f32,
        top: px(y, height) / height as f32,
        right: px(x + rw, width) / width as f32,
        bottom: px(y + rh, height) / height as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_inside_the_covered_area() {
        let (w, h) = (300, 200);
        // Uncovered: a 12-px band on the left and a 6-px band at the bottom.
        let cov: Vec<bool> = (0..w * h).map(|i| i % w >= 12 && i / w < h - 6).collect();
        let c = auto_crop(&cov, w, h).unwrap();
        assert!(c.left * w as f32 >= 12.0 - 1e-3, "{c:?}");
        assert!(c.bottom * h as f32 <= (h - 6) as f32 + 1e-3, "{c:?}");
        let (cw, ch) = ((c.right - c.left) * w as f32, (c.bottom - c.top) * h as f32);
        assert!((cw / ch - 1.5).abs() < 0.02, "{cw} × {ch}");
        assert!(ch >= 190.0, "{ch}");
        assert_eq!(auto_crop(&vec![true; w * h], w, h), None);
    }
}
