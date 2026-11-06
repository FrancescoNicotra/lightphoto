//! Crop and straighten (`crs:HasCrop`, `CropTop/Left/Bottom/Right`,
//! `CropAngle`).
//!
//! Model: the oriented photo is turned by `angle` about its centre on a
//! canvas of the same size, then the crop rectangle (fractions of that
//! canvas) is cut out. Camera Raw keeps the rectangle in the file's stored
//! orientation, before the EXIF rotation; [`to_oriented`] and
//! [`from_oriented`] convert. With `angle` = 0 this is exactly Camera Raw's
//! crop; with an angle the rectangle's meaning is our reading of the
//! format and may differ slightly from Lightroom's.

use rayon::prelude::*;

use raw::{Image, Orientation};
use xmp::Crop;

/// A rectangle in fractions of the image (0..1), left/top inclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub const FULL: Rect = Rect { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 };

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    pub fn center(&self) -> (f32, f32) {
        ((self.left + self.right) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

/// Maps a point in fractions of the stored (unrotated) image to the
/// oriented image, as `Image::oriented` moves pixels.
pub fn orient_point(o: Orientation, (u, v): (f32, f32)) -> (f32, f32) {
    match o {
        Orientation::Normal => (u, v),
        Orientation::FlipH => (1.0 - u, v),
        Orientation::Rotate180 => (1.0 - u, 1.0 - v),
        Orientation::FlipV => (u, 1.0 - v),
        Orientation::Transpose => (v, u),
        Orientation::Rotate90 => (1.0 - v, u),
        Orientation::Transverse => (1.0 - v, 1.0 - u),
        Orientation::Rotate270 => (v, 1.0 - u),
    }
}

/// The inverse of [`orient_point`].
pub fn unorient_point(o: Orientation, (x, y): (f32, f32)) -> (f32, f32) {
    match o {
        Orientation::Rotate90 => (y, 1.0 - x),
        Orientation::Rotate270 => (1.0 - y, x),
        // The others are their own inverse.
        _ => orient_point(o, (x, y)),
    }
}

/// Mirroring orientations turn rotations the other way.
fn mirrored(o: Orientation) -> bool {
    matches!(o, Orientation::FlipH | Orientation::FlipV | Orientation::Transpose | Orientation::Transverse)
}

/// The crop in the oriented image: rectangle and clockwise angle.
pub fn to_oriented(crop: &Crop, o: Orientation) -> (Rect, f32) {
    if !crop.has_crop {
        return (Rect::FULL, 0.0);
    }
    let a = orient_point(o, (crop.left, crop.top));
    let b = orient_point(o, (crop.right, crop.bottom));
    let rect = Rect { left: a.0.min(b.0), top: a.1.min(b.1), right: a.0.max(b.0), bottom: a.1.max(b.1) };
    let angle = if mirrored(o) { -crop.angle } else { crop.angle };
    (rect, angle)
}

/// A crop set on the oriented image, back in Camera Raw's terms.
pub fn from_oriented(rect: Rect, angle: f32, o: Orientation) -> Crop {
    let a = unorient_point(o, (rect.left, rect.top));
    let b = unorient_point(o, (rect.right, rect.bottom));
    let angle = if mirrored(o) { -angle } else { angle };
    let full = rect == Rect::FULL && angle == 0.0;
    Crop { has_crop: !full, left: a.0.min(b.0), top: a.1.min(b.1), right: a.0.max(b.0), bottom: a.1.max(b.1), angle }
}

/// Where a point of the straightened canvas (pixels) comes from in the
/// oriented image (pixels), for a `w`×`h` image turned by `angle`.
pub fn source_of(x: f32, y: f32, w: f32, h: f32, angle: f32) -> (f32, f32) {
    let (s, c) = (-angle).to_radians().sin_cos();
    let (dx, dy) = (x - w / 2.0, y - h / 2.0);
    (w / 2.0 + dx * c - dy * s, h / 2.0 + dx * s + dy * c)
}

/// Whether every corner of `rect` shows picture (not the empty corners a
/// rotation leaves).
pub fn inside(rect: Rect, angle: f32, w: f32, h: f32) -> bool {
    let eps = 1e-3;
    [(rect.left, rect.top), (rect.right, rect.top), (rect.left, rect.bottom), (rect.right, rect.bottom)].iter().all(|&(u, v)| {
        let (sx, sy) = source_of(u * w, v * h, w, h, angle);
        sx >= -eps * w && sy >= -eps * h && sx <= w * (1.0 + eps) && sy <= h * (1.0 + eps)
    })
}

/// `rect` shrunk about its centre, keeping its shape, until it holds
/// picture only.
pub fn fit_inside(rect: Rect, angle: f32, w: f32, h: f32) -> Rect {
    let rect = Rect { left: rect.left.max(0.0), top: rect.top.max(0.0), right: rect.right.min(1.0), bottom: rect.bottom.min(1.0) };
    if inside(rect, angle, w, h) {
        return rect;
    }
    let (cx, cy) = rect.center();
    let scaled = |k: f32| Rect {
        left: cx - rect.width() / 2.0 * k,
        right: cx + rect.width() / 2.0 * k,
        top: cy - rect.height() / 2.0 * k,
        bottom: cy + rect.height() / 2.0 * k,
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if inside(scaled(mid), angle, w, h) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    scaled(lo)
}

/// The photo turned by `angle` degrees (clockwise) about its centre, same
/// size, bilinear; the corners outside the picture are black.
pub fn straighten(img: &Image, angle: f32) -> Image {
    if angle == 0.0 {
        return img.clone();
    }
    let (w, h) = (img.width as f32, img.height as f32);
    let mut out = Image::new(img.width, img.height);
    out.pixels.par_chunks_mut(img.width.max(1)).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let (sx, sy) = source_of(x as f32 + 0.5, y as f32 + 0.5, w, h, angle);
            *p = bilinear(img, sx - 0.5, sy - 0.5);
        }
    });
    out
}

fn bilinear(img: &Image, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (img.width as i64, img.height as i64);
    if x < -0.5 || y < -0.5 || x > w as f32 - 0.5 || y > h as f32 - 0.5 {
        return [0.0; 3];
    }
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let at = |xi: i64, yi: i64| img.get(xi.clamp(0, w - 1) as usize, yi.clamp(0, h - 1) as usize);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

/// Cuts `rect` out of the image (at least one pixel).
pub fn crop(img: &Image, rect: Rect) -> Image {
    let (w, h) = (img.width as f32, img.height as f32);
    let x = (rect.left.clamp(0.0, 1.0) * w).round() as usize;
    let y = (rect.top.clamp(0.0, 1.0) * h).round() as usize;
    let cw = ((rect.width().clamp(0.0, 1.0) * w).round() as usize).max(1);
    let ch = ((rect.height().clamp(0.0, 1.0) * h).round() as usize).max(1);
    img.crop(x.min(img.width - 1), y.min(img.height - 1), cw, ch)
}

/// Straighten and crop an oriented image as `crop` says.
pub fn apply(img: Image, crop: &Crop, o: Orientation) -> Image {
    if !crop.is_active() {
        return img;
    }
    let (rect, angle) = to_oriented(crop, o);
    let turned = if angle != 0.0 { straighten(&img, angle) } else { img };
    if rect == Rect::FULL {
        turned
    } else {
        self::crop(&turned, rect)
    }
}

/// Size of the developed photo after cropping, for a `w`×`h` oriented one.
pub fn cropped_size(w: usize, h: usize, crop: &Crop, o: Orientation) -> (usize, usize) {
    if !crop.is_active() {
        return (w, h);
    }
    let (r, _) = to_oriented(crop, o);
    (((r.width() * w as f32).round() as usize).max(1), ((r.height() * h as f32).round() as usize).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Orientation; 8] = [
        Orientation::Normal,
        Orientation::FlipH,
        Orientation::Rotate180,
        Orientation::FlipV,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];

    #[test]
    fn orientation_points_match_the_image_transform() {
        // A 4×2 image with one marked pixel: where it lands must be where
        // orient_point sends its centre.
        for o in ALL {
            let img = Image::from_fn(4, 2, |x, y| if (x, y) == (3, 0) { [1.0; 3] } else { [0.0; 3] });
            let out = img.oriented(o);
            let (u, v) = orient_point(o, (3.5 / 4.0, 0.5 / 2.0));
            let (x, y) = ((u * out.width as f32) as usize, (v * out.height as f32) as usize);
            assert_eq!(out.get(x, y), [1.0; 3], "{o:?}");
            assert_eq!(unorient_point(o, (u, v)), (3.5 / 4.0, 0.5 / 2.0), "{o:?}");
        }
    }

    #[test]
    fn crops_convert_both_ways() {
        let crop = Crop { has_crop: true, top: 0.1, left: 0.2, bottom: 0.7, right: 0.9, angle: 3.0 };
        for o in ALL {
            let (rect, angle) = to_oriented(&crop, o);
            let back = from_oriented(rect, angle, o);
            let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
            assert!(close(back.top, crop.top) && close(back.left, crop.left) && close(back.bottom, crop.bottom) && close(back.right, crop.right), "{o:?}");
            assert_eq!(back.angle, crop.angle);
        }
        assert!(!from_oriented(Rect::FULL, 0.0, Orientation::Normal).has_crop);
    }

    #[test]
    fn crop_and_straighten() {
        let img = Image::from_fn(200, 100, |x, _| [x as f32 / 199.0; 3]);
        let crop = Crop { has_crop: true, top: 0.25, left: 0.5, bottom: 0.75, right: 1.0, angle: 0.0 };
        let out = apply(img.clone(), &crop, Orientation::Normal);
        assert_eq!((out.width, out.height), (100, 50));
        assert!(out.get(0, 0)[0] > 0.49 && out.get(0, 0)[0] < 0.51);
        assert_eq!(cropped_size(200, 100, &crop, Orientation::Normal), (100, 50));

        // A turned photo keeps its centre and gets black corners.
        let turned = straighten(&img, 10.0);
        assert!((turned.get(100, 50)[0] - img.get(100, 50)[0]).abs() < 0.02);
        assert_eq!(turned.get(0, 0), [0.0; 3]);
    }

    #[test]
    fn rectangles_are_kept_inside_the_picture() {
        let (w, h) = (300.0, 200.0);
        assert!(inside(Rect::FULL, 0.0, w, h));
        assert!(!inside(Rect::FULL, 5.0, w, h));
        let fitted = fit_inside(Rect::FULL, 5.0, w, h);
        assert!(inside(fitted, 5.0, w, h));
        assert!(fitted.width() > 0.8 && fitted.width() < 1.0);
        // The shape is kept.
        assert!((fitted.width() / fitted.height() - 1.0).abs() < 1e-3);
    }
}
