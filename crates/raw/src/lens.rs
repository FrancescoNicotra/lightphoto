//! Lens corrections stored in the file: distortion, lateral chromatic
//! aberration and vignetting.
//!
//! Everything is expressed with the two DNG opcodes that carry them
//! (DNG specification 1.4, as implemented by the DNG SDK):
//!
//! - **WarpRectilinear** (opcode 1): each output pixel is read from
//!   `centre + R·(n·ratio(r²) + tangential)`, with `n` its offset from the
//!   optical centre in units of `R`, the distance from the centre to the
//!   farthest corner, `ratio(r²) = k₀ + k₁r² + k₂r⁴ + k₃r⁶`, and one set of
//!   coefficients per colour plane (three planes also correct lateral
//!   chromatic aberration).
//! - **FixVignetteRadial** (opcode 3): pixels are multiplied by
//!   `1 + k₀r² + k₁r⁴ + … + k₄r¹⁰`.
//!
//! DNG files carry them in OpcodeList3 (after demosaicing). Panasonic RW2
//! files carry a distortion polynomial in the maker notes (tag 0x0119),
//! which is converted into a WarpRectilinear by least squares (see
//! [`panasonic_warp`]).

use rayon::prelude::*;

use crate::Image;

/// One colour plane of a WarpRectilinear: radial `kr0..kr3`, tangential
/// `kt0, kt1`.
pub type WarpPlane = [f64; 6];

#[derive(Clone, Debug, PartialEq)]
pub enum Opcode {
    Warp(Warp),
    Vignette(Vignette),
}

/// A WarpRectilinear, in sensor pixel coordinates (pixel indices are
/// positions, as in the DNG SDK).
#[derive(Clone, Debug, PartialEq)]
pub struct Warp {
    pub centre: [f64; 2],
    /// Normalisation radius in pixels.
    pub radius: f64,
    /// One plane for all colours, or one per colour.
    pub planes: Vec<WarpPlane>,
}

/// A FixVignetteRadial, in sensor pixel coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Vignette {
    pub centre: [f64; 2],
    pub radius: f64,
    pub k: [f64; 5],
}

impl Warp {
    /// Source position (sensor pixels) of output position `p`, for `plane`.
    pub fn source(&self, plane: usize, p: [f64; 2]) -> [f64; 2] {
        let k = self.planes[plane.min(self.planes.len() - 1)];
        let n = [(p[0] - self.centre[0]) / self.radius, (p[1] - self.centre[1]) / self.radius];
        let r2 = (n[0] * n[0] + n[1] * n[1]).min(1.0);
        let ratio = k[0] + r2 * (k[1] + r2 * (k[2] + r2 * k[3]));
        let (kt0, kt1) = (k[4], k[5]);
        let cross = 2.0 * n[0] * n[1];
        let th = kt1 * (r2 + 2.0 * n[0] * n[0]) + kt0 * cross;
        let tv = kt0 * (r2 + 2.0 * n[1] * n[1]) + kt1 * cross;
        [self.centre[0] + self.radius * (n[0] * ratio + th), self.centre[1] + self.radius * (n[1] * ratio + tv)]
    }
}

impl Vignette {
    pub fn gain(&self, p: [f64; 2]) -> f64 {
        let u = (p[0] + 0.5 - self.centre[0]) / self.radius;
        let v = (p[1] + 0.5 - self.centre[1]) / self.radius;
        let x = (u * u + v * v).min(1.0);
        let mut g = 1.0;
        let mut xp = x;
        for k in self.k {
            g += k * xp;
            xp *= x;
        }
        g
    }
}

/// Distance from `c` to the farthest corner of the rectangle [l, t, r, b).
fn farthest_corner(c: [f64; 2], [l, t, r, b]: [f64; 4]) -> f64 {
    let dx = (c[0] - l).abs().max((c[0] - r).abs());
    let dy = (c[1] - t).abs().max((c[1] - b).abs());
    (dx * dx + dy * dy).sqrt()
}

/// Parses a DNG opcode list (big-endian), keeping the opcodes understood
/// here. `bounds` is the image the list applies to (x, y, width, height in
/// sensor pixels: the active area for OpcodeList3).
pub fn parse_opcode_list(data: &[u8], bounds: [usize; 4]) -> Vec<Opcode> {
    let mut r = Reader { data, pos: 0 };
    let mut out = Vec::new();
    let Some(count) = r.u32() else { return out };
    let [bx, by, bw, bh] = bounds.map(|v| v as f64);
    let rect = [bx, by, bx + bw, by + bh];
    for _ in 0..count {
        let (Some(id), Some(_version), Some(_flags), Some(len)) = (r.u32(), r.u32(), r.u32(), r.u32()) else { break };
        let end = r.pos + len as usize;
        if end > data.len() {
            break;
        }
        let mut p = Reader { data: &data[..end], pos: r.pos };
        let centre = |cx: f64, cy: f64| [bx + cx * bw, by + cy * bh];
        match id {
            1 => {
                let parsed = (|| {
                    let n = p.u32()? as usize;
                    if !(1..=4).contains(&n) {
                        return None;
                    }
                    let planes = (0..n).map(|_| Some(std::array::from_fn::<_, 6, _>(|_| p.f64().unwrap_or(f64::NAN)))).collect::<Option<Vec<WarpPlane>>>()?;
                    let c = centre(p.f64()?, p.f64()?);
                    planes.iter().flatten().all(|v| v.is_finite()).then(|| Warp { centre: c, radius: farthest_corner(c, rect), planes })
                })();
                out.extend(parsed.map(Opcode::Warp));
            }
            3 => {
                let parsed = (|| {
                    let k = [p.f64()?, p.f64()?, p.f64()?, p.f64()?, p.f64()?];
                    let c = centre(p.f64()?, p.f64()?);
                    Some(Vignette { centre: c, radius: farthest_corner(c, rect), k })
                })();
                out.extend(parsed.map(Opcode::Vignette));
            }
            _ => {}
        }
        r.pos = end;
    }
    out
}

/// Writes `opcodes` as a DNG opcode list for an image whose bounds on the
/// sensor are `bounds` (x, y, width, height): the inverse of
/// [`parse_opcode_list`]. Warps are renormalised to the farthest corner of
/// those bounds, as the specification requires.
pub fn opcode_list(opcodes: &[Opcode], bounds: [usize; 4]) -> Vec<u8> {
    let [bx, by, bw, bh] = bounds.map(|v| v as f64);
    let rect = [bx, by, bx + bw, by + bh];
    let relative = |c: [f64; 2]| [(c[0] - bx) / bw, (c[1] - by) / bh];
    let mut out = (opcodes.len() as u32).to_be_bytes().to_vec();
    for op in opcodes {
        let (id, params): (u32, Vec<f64>) = match op {
            Opcode::Warp(w) => {
                let w = w.renormalised(farthest_corner(w.centre, rect));
                let mut v: Vec<f64> = w.planes.iter().flatten().copied().collect();
                v.extend(relative(w.centre));
                (1, v)
            }
            Opcode::Vignette(v) => {
                let m = farthest_corner(v.centre, rect) / v.radius;
                let mut p: Vec<f64> = v.k.iter().enumerate().map(|(i, k)| k * m.powi(2 * (i as i32 + 1))).collect();
                p.extend(relative(v.centre));
                (3, p)
            }
        };
        let mut body = Vec::new();
        if let Opcode::Warp(w) = op {
            body.extend((w.planes.len() as u32).to_be_bytes());
        }
        body.extend(params.iter().flat_map(|x| x.to_be_bytes()));
        // DNG 1.3 opcodes, not optional, applied to previews too.
        for x in [id, 0x0103_0000, 0, body.len() as u32] {
            out.extend(x.to_be_bytes());
        }
        out.extend(body);
    }
    out
}

impl Warp {
    /// The same warp with radii in units of `radius` pixels: the radial
    /// coefficients scale by (radius / self.radius)^(2i), the tangential
    /// ones by radius / self.radius.
    pub fn renormalised(&self, radius: f64) -> Warp {
        let m = radius / self.radius;
        let planes = self
            .planes
            .iter()
            .map(|k| [k[0], k[1] * m.powi(2), k[2] * m.powi(4), k[3] * m.powi(6), k[4] * m, k[5] * m])
            .collect();
        Warp { centre: self.centre, radius, planes }
    }

    /// Moved to coordinates whose origin is sensor position `origin`.
    pub fn shifted(&self, origin: [f64; 2]) -> Warp {
        Warp { centre: [self.centre[0] - origin[0], self.centre[1] - origin[1]], ..self.clone() }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let b = self.data.get(self.pos..self.pos + N)?.try_into().ok()?;
        self.pos += N;
        Some(b)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_be_bytes)
    }
    fn f64(&mut self) -> Option<f64> {
        self.take().map(f64::from_be_bytes)
    }
}

/// Panasonic's distortion model (tag 0x0119 of RW2 files, as decoded by
/// darktable): with radii in units of the half diagonal of the image,
/// a source radius `rd` lands at `ru = rd·(1 + s·(a·rd² + b·rd⁴ + c·rd⁶))`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanasonicDistortion {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub scale: f64,
}

impl PanasonicDistortion {
    /// From the 32 bytes of the tag; `None` when the camera marked the
    /// correction off.
    pub fn from_tag(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 32 {
            return None;
        }
        let v: Vec<f64> = bytes.chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f64).collect();
        if (v[7] as i16) & 0x0f != 1 {
            return None;
        }
        Some(PanasonicDistortion { a: v[8] / 32768.0, b: v[4] / 32768.0, c: v[11] / 32768.0, scale: 1.0 / (1.0 + v[5] / 32768.0) })
    }

    fn forward(&self, rd: f64) -> f64 {
        let r2 = rd * rd;
        rd * (1.0 + self.scale * r2 * (self.a + r2 * (self.b + r2 * self.c)))
    }

    /// The source radius of output radius `ru` (Newton's method).
    fn inverse(&self, ru: f64) -> f64 {
        let mut rd = ru;
        for _ in 0..20 {
            let h = 1e-6;
            let d = (self.forward(rd + h) - self.forward(rd - h)) / (2.0 * h);
            let step = (self.forward(rd) - ru) / d.max(1e-3);
            rd -= step;
            if step.abs() < 1e-12 {
                break;
            }
        }
        rd
    }
}

/// Panasonic's distortion over an image of `size` pixels whose top-left
/// photosite is at `origin` on the sensor, as a WarpRectilinear: the ratio
/// `rd/ru` sampled over the whole radius and fitted with the cubic in r²
/// of the opcode. The worst error is returned with it, in pixels.
pub fn panasonic_warp(d: &PanasonicDistortion, origin: [usize; 2], size: [usize; 2]) -> (Warp, f64) {
    let centre = [origin[0] as f64 + (size[0] as f64 - 1.0) / 2.0, origin[1] as f64 + (size[1] as f64 - 1.0) / 2.0];
    let radius = ((size[0] * size[0] + size[1] * size[1]) as f64).sqrt() / 2.0;
    // Least squares on ratio(t) = k0 + k1 t + k2 t² + k3 t³, t = r².
    let samples: Vec<(f64, f64)> = (0..=400)
        .map(|i| {
            let r = i as f64 / 400.0;
            // rd/ru tends to 1 at the centre.
            let ratio = if r == 0.0 { 1.0 } else { d.inverse(r) / r };
            (r * r, ratio)
        })
        .collect();
    let mut ata = [[0.0f64; 4]; 4];
    let mut atb = [0.0f64; 4];
    for &(t, y) in &samples {
        let row = [1.0, t, t * t, t * t * t];
        for i in 0..4 {
            atb[i] += row[i] * y;
            for j in 0..4 {
                ata[i][j] += row[i] * row[j];
            }
        }
    }
    let k = solve4(ata, atb);
    let worst = samples
        .iter()
        .map(|&(t, y)| {
            let fit = k[0] + t * (k[1] + t * (k[2] + t * k[3]));
            (fit - y).abs() * t.sqrt() * radius
        })
        .fold(0.0, f64::max);
    (Warp { centre, radius, planes: vec![[k[0], k[1], k[2], k[3], 0.0, 0.0]] }, worst)
}

/// Gaussian elimination with partial pivoting.
fn solve4(mut a: [[f64; 4]; 4], mut b: [f64; 4]) -> [f64; 4] {
    for col in 0..4 {
        let pivot = (col..4).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap();
        a.swap(col, pivot);
        b.swap(col, pivot);
        let pivot_row = a[col];
        for row in col + 1..4 {
            let f = a[row][col] / pivot_row[col];
            for (v, p) in a[row].iter_mut().zip(pivot_row).skip(col) {
                *v -= f * p;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = [0.0; 4];
    for row in (0..4).rev() {
        let s: f64 = (row + 1..4).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    x
}

/// Where an image sits on the sensor: sensor position = `scale · pixel +
/// offset` (scale 2 and a half-photosite offset for half-size images).
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub scale: f64,
    pub offset: [f64; 2],
}

/// Applies `opcodes` in order to `img`, laid on the sensor by `at`.
pub fn apply(img: Image, opcodes: &[Opcode], at: Placement) -> Image {
    opcodes.iter().fold(img, |img, op| match op {
        Opcode::Vignette(v) => vignette(img, v, at),
        Opcode::Warp(w) => warp(&img, w, at),
    })
}

fn vignette(mut img: Image, v: &Vignette, at: Placement) -> Image {
    let w = img.width;
    img.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            // The SDK samples pixel centres; here a pixel covers `scale`
            // photosites from its sensor position.
            let s = [at.scale * x as f64 + at.offset[0] + (at.scale - 1.0) / 2.0, at.scale * y as f64 + at.offset[1] + (at.scale - 1.0) / 2.0];
            let g = v.gain(s) as f32;
            *p = p.map(|c| c * g);
        }
    });
    img
}

fn warp(img: &Image, wp: &Warp, at: Placement) -> Image {
    let (w, h) = (img.width, img.height);
    let planes = wp.planes.len().min(3);
    Image::from_fn(w, h, |x, y| {
        let s = [at.scale * x as f64 + at.offset[0], at.scale * y as f64 + at.offset[1]];
        let to_pixel = |q: [f64; 2]| [((q[0] - at.offset[0]) / at.scale) as f32, ((q[1] - at.offset[1]) / at.scale) as f32];
        if planes <= 1 {
            catmull_rom(img, to_pixel(wp.source(0, s)), None)
        } else {
            std::array::from_fn(|c| catmull_rom(img, to_pixel(wp.source(c, s)), Some(c))[c])
        }
    })
}

/// Catmull-Rom interpolation at `p` (edges replicated), of one channel or
/// all of them. Not below zero.
fn catmull_rom(img: &Image, p: [f32; 2], only: Option<usize>) -> [f32; 3] {
    let weights = |t: f32| -> [f32; 4] {
        let (t2, t3) = (t * t, t * t * t);
        [(-t3 + 2.0 * t2 - t) / 2.0, (3.0 * t3 - 5.0 * t2 + 2.0) / 2.0, (-3.0 * t3 + 4.0 * t2 + t) / 2.0, (t3 - t2) / 2.0]
    };
    let (fx, fy) = (p[0].floor(), p[1].floor());
    let (wx, wy) = (weights(p[0] - fx), weights(p[1] - fy));
    let (x0, y0) = (fx as isize - 1, fy as isize - 1);
    let clamp = |v: isize, n: usize| v.clamp(0, n as isize - 1) as usize;
    let mut out = [0.0f32; 3];
    for (j, wyj) in wy.iter().enumerate() {
        let yy = clamp(y0 + j as isize, img.height);
        for (i, wxi) in wx.iter().enumerate() {
            let q = img.pixels[yy * img.width + clamp(x0 + i as isize, img.width)];
            let k = wxi * wyj;
            match only {
                Some(c) => out[c] += k * q[c],
                None => (0..3).for_each(|c| out[c] += k * q[c]),
            }
        }
    }
    out.map(|v| v.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opcode(id: u32, params: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [id, 0x0103_0000, 1, params.len() as u32] {
            v.extend(x.to_be_bytes());
        }
        v.extend(params);
        v
    }

    fn doubles(v: &[f64]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    #[test]
    fn parses_warp_and_vignette() {
        let mut list = 2u32.to_be_bytes().to_vec();
        let mut warp = 1u32.to_be_bytes().to_vec();
        warp.extend(doubles(&[0.98, 0.01, 0.002, 0.0, 0.0, 0.0, 0.5, 0.5]));
        list.extend(opcode(1, &warp));
        list.extend(opcode(3, &doubles(&[0.3, 0.1, 0.0, 0.0, 0.0, 0.5, 0.5])));
        list.extend(opcode(99, &[1, 2, 3])); // unknown: skipped, not counted
        let ops = parse_opcode_list(&list, [10, 20, 400, 300]);
        assert_eq!(ops.len(), 2);
        let Opcode::Warp(w) = &ops[0] else { panic!() };
        assert_eq!(w.centre, [210.0, 170.0]);
        assert_eq!(w.radius, 250.0);
        assert_eq!(w.planes, vec![[0.98, 0.01, 0.002, 0.0, 0.0, 0.0]]);
        let Opcode::Vignette(v) = &ops[1] else { panic!() };
        assert!((v.gain([209.5, 169.5]) - 1.0).abs() < 1e-12);
        // At a corner: r² = 1.
        assert!((v.gain([409.5, 319.5]) - 1.4).abs() < 1e-12);
    }

    #[test]
    fn warp_maps_like_the_specification() {
        let w = Warp { centre: [100.0, 50.0], radius: 200.0, planes: vec![[0.9, 0.1, 0.0, 0.0, 0.0, 0.0]] };
        assert_eq!(w.source(0, [100.0, 50.0]), [100.0, 50.0]);
        // n = (0.5, 0), r² = 0.25, ratio = 0.925.
        let s = w.source(0, [200.0, 50.0]);
        assert!((s[0] - (100.0 + 200.0 * 0.5 * 0.925)).abs() < 1e-9 && (s[1] - 50.0).abs() < 1e-12, "{s:?}");
    }

    /// The fitted opcode reproduces Panasonic's polynomial to a fraction of
    /// a pixel over a 16 MP frame (the GX80 file's coefficients).
    #[test]
    fn panasonic_polynomial_becomes_a_warp() {
        let mut tag = [0u8; 32];
        for (i, v) in [(4usize, 222i16), (5, 0), (7, 1), (8, 3561), (11, -738)] {
            tag[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        let d = PanasonicDistortion::from_tag(&tag).unwrap();
        let (w, worst) = panasonic_warp(&d, [8, 8], [4592, 3448]);
        assert!(worst < 0.05, "{worst} px");
        // A point at 0.9 of the half diagonal comes from rd with ru = 0.9.
        let ru = 0.9;
        let p = [w.centre[0] + ru * w.radius * 0.8, w.centre[1] + ru * w.radius * 0.6];
        let s = w.source(0, p);
        let rd = ((s[0] - w.centre[0]).powi(2) + (s[1] - w.centre[1]).powi(2)).sqrt() / w.radius;
        assert!((d.forward(rd) - ru).abs() * w.radius < 0.05, "{rd}");
        // Switched off: no correction.
        tag[14] = 0;
        assert!(PanasonicDistortion::from_tag(&tag).is_none());
    }

    #[test]
    fn opcode_lists_round_trip() {
        let warp = Warp { centre: [600.0, 400.0], radius: 2870.0, planes: vec![[1.0, -0.1, 0.01, 0.0, 0.0, 0.0]] };
        let vignette = Vignette { centre: [600.0, 400.0], radius: 900.0, k: [0.2, 0.05, 0.0, 0.0, 0.0] };
        let ops = vec![Opcode::Warp(warp.clone()), Opcode::Vignette(vignette.clone())];
        let bounds = [0, 0, 1024, 768];
        let back = parse_opcode_list(&opcode_list(&ops, bounds), bounds);
        let (Opcode::Warp(w), Opcode::Vignette(v)) = (&back[0], &back[1]) else { panic!("{back:?}") };
        // Renormalised, but the same mapping and the same gains.
        assert!((w.radius - 600f64.hypot(400.0)).abs() < 1e-9);
        for p in [[0.0, 0.0], [1023.0, 767.0], [300.0, 700.0]] {
            let (a, b) = (warp.source(0, p), w.source(0, p));
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} {b:?}");
            assert!((vignette.gain(p) - v.gain(p)).abs() < 1e-12);
        }
    }

    #[test]
    fn identity_warp_keeps_the_image() {
        let img = Image::from_fn(30, 20, |x, y| [x as f32, y as f32, (x * y) as f32]);
        let w = Warp { centre: [15.0, 10.0], radius: 18.0, planes: vec![[1.0, 0.0, 0.0, 0.0, 0.0, 0.0]] };
        let out = apply(img.clone(), &[Opcode::Warp(w)], Placement { scale: 1.0, offset: [0.0, 0.0] });
        assert!(out.pixels.iter().zip(&img.pixels).all(|(a, b)| (0..3).all(|c| (a[c] - b[c]).abs() < 1e-4)));
    }
}
