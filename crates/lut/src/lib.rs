//! 3D LUTs: .cube (Resolve/Adobe) and .3dl (Lustre) I/O plus tetrahedral
//! interpolation, the method Adobe uses for its own tables.

use std::fmt::Write as _;
use std::path::Path;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Parse { line: usize, msg: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Parse { line, msg } => write!(f, "line {line}: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

fn parse_err(line: usize, msg: impl Into<String>) -> Error {
    Error::Parse { line, msg: msg.into() }
}

/// Three per-channel curves (also used as a shaper in front of a 3D LUT).
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1D {
    pub data: Vec<[f32; 3]>,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
}

impl Lut1D {
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let last = (self.data.len() - 1) as f32;
        std::array::from_fn(|c| {
            let t = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * last;
            let i = (t as usize).min(self.data.len() - 2);
            let f = t - i as f32;
            self.data[i][c] + (self.data[i + 1][c] - self.data[i][c]) * f
        })
    }
}

/// A 3D LUT; `data` is stored red-fastest, as in .cube files:
/// index = r + g * size + b * size².
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3D {
    pub size: usize,
    pub data: Vec<[f32; 3]>,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
}

impl Lut3D {
    pub fn identity(size: usize) -> Self {
        Self::from_fn(size, |rgb| rgb)
    }

    /// Samples `f` on the grid; inputs are the normalized 0..1 coordinates.
    pub fn from_fn(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> Self {
        assert!(size >= 2, "a 3D LUT needs at least 2 points per axis");
        let s = (size - 1) as f32;
        let mut data = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    data.push(f([r as f32 / s, g as f32 / s, b as f32 / s]));
                }
            }
        }
        Lut3D { size, data, domain_min: [0.0; 3], domain_max: [1.0; 3] }
    }

    #[inline]
    pub fn get(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.data[r + self.size * (g + self.size * b)]
    }

    /// Tetrahedral interpolation (input clamped to the domain).
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        let s = (n - 1) as f32;
        let mut i0 = [0usize; 3];
        let mut f = [0f32; 3];
        for c in 0..3 {
            let t = ((rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c])).clamp(0.0, 1.0) * s;
            i0[c] = (t as usize).min(n - 2);
            f[c] = t - i0[c] as f32;
        }
        let [r, g, b] = i0;
        let [fr, fg, fb] = f;
        let c000 = self.get(r, g, b);
        let c111 = self.get(r + 1, g + 1, b + 1);

        // Walk from c000 to c111 along the edges of the tetrahedron that
        // contains the point, ordered by the largest fractional part.
        let (c_a, c_b, w) = if fr > fg {
            if fg > fb {
                (self.get(r + 1, g, b), self.get(r + 1, g + 1, b), [fr, fg, fb])
            } else if fr > fb {
                (self.get(r + 1, g, b), self.get(r + 1, g, b + 1), [fr, fb, fg])
            } else {
                (self.get(r, g, b + 1), self.get(r + 1, g, b + 1), [fb, fr, fg])
            }
        } else if fb > fg {
            (self.get(r, g, b + 1), self.get(r, g + 1, b + 1), [fb, fg, fr])
        } else if fb > fr {
            (self.get(r, g + 1, b), self.get(r, g + 1, b + 1), [fg, fb, fr])
        } else {
            (self.get(r, g + 1, b), self.get(r + 1, g + 1, b), [fg, fr, fb])
        };
        std::array::from_fn(|c| {
            c000[c] + (c_a[c] - c000[c]) * w[0] + (c_b[c] - c_a[c]) * w[1] + (c111[c] - c_b[c]) * w[2]
        })
    }

    /// Blends between the input and the LUT result; Camera Raw's profile
    /// Amount slider goes from 0 to 2.
    pub fn apply_amount(&self, rgb: [f32; 3], amount: f32) -> [f32; 3] {
        let out = self.apply(rgb);
        std::array::from_fn(|c| rgb[c] + (out[c] - rgb[c]) * amount)
    }

    pub fn to_cube_string(&self, title: Option<&str>) -> String {
        let mut s = String::with_capacity(self.data.len() * 28 + 128);
        if let Some(t) = title {
            let _ = writeln!(s, "TITLE \"{}\"", t.replace('"', "'"));
        }
        let _ = writeln!(s, "LUT_3D_SIZE {}", self.size);
        if self.domain_min != [0.0; 3] || self.domain_max != [1.0; 3] {
            let [a, b, c] = self.domain_min;
            let _ = writeln!(s, "DOMAIN_MIN {a} {b} {c}");
            let [a, b, c] = self.domain_max;
            let _ = writeln!(s, "DOMAIN_MAX {a} {b} {c}");
        }
        s.push('\n');
        for [r, g, b] in &self.data {
            let _ = writeln!(s, "{r:.6} {g:.6} {b:.6}");
        }
        s
    }

    pub fn write_cube(&self, path: impl AsRef<Path>, title: Option<&str>) -> Result<(), Error> {
        Ok(std::fs::write(path, self.to_cube_string(title))?)
    }
}

/// Contents of a .cube file: a 3D LUT, optionally preceded by a 1D shaper.
#[derive(Clone, Debug, PartialEq)]
pub struct CubeFile {
    pub title: Option<String>,
    pub shaper: Option<Lut1D>,
    pub lut: Option<Lut3D>,
}

impl CubeFile {
    pub fn apply(&self, mut rgb: [f32; 3]) -> [f32; 3] {
        if let Some(s) = &self.shaper {
            rgb = s.apply(rgb);
        }
        match &self.lut {
            Some(l) => l.apply(rgb),
            None => rgb,
        }
    }
}

pub fn parse_cube(text: &str) -> Result<CubeFile, Error> {
    let mut title = None;
    let (mut size1, mut size3) = (0usize, 0usize);
    let (mut dmin, mut dmax) = ([0.0f32; 3], [1.0f32; 3]);
    let (mut dmin1, mut dmax1) = (None, None);
    let mut rows: Vec<[f32; 3]> = Vec::new();

    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let head = parts.next().unwrap();
        let rest: Vec<&str> = parts.collect();
        let triple = |rest: &[&str]| -> Result<[f32; 3], Error> {
            if rest.len() != 3 {
                return Err(parse_err(n, "expected 3 values"));
            }
            let mut v = [0.0; 3];
            for (i, s) in rest.iter().enumerate() {
                v[i] = s.parse().map_err(|_| parse_err(n, format!("bad number {s:?}")))?;
            }
            Ok(v)
        };
        let int = |rest: &[&str]| -> Result<usize, Error> {
            rest.first().and_then(|s| s.parse().ok()).ok_or_else(|| parse_err(n, "expected a size"))
        };
        match head {
            "TITLE" => title = Some(rest.join(" ").trim_matches('"').to_string()),
            "LUT_3D_SIZE" => size3 = int(&rest)?,
            "LUT_1D_SIZE" => size1 = int(&rest)?,
            "DOMAIN_MIN" => dmin = triple(&rest)?,
            "DOMAIN_MAX" => dmax = triple(&rest)?,
            "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                let range: Vec<f32> = rest.iter().filter_map(|s| s.parse().ok()).collect();
                let [lo, hi] = range[..] else { return Err(parse_err(n, "expected min max")) };
                if head.starts_with("LUT_1D") {
                    (dmin1, dmax1) = (Some([lo; 3]), Some([hi; 3]));
                } else {
                    (dmin, dmax) = ([lo; 3], [hi; 3]);
                }
            }
            h if h.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.') => {
                let mut all = vec![head];
                all.extend(&rest);
                rows.push(triple(&all)?);
            }
            // Unknown keywords (LUT_IN_VIDEO_RANGE, vendor tags) are ignored.
            _ => {}
        }
    }

    let need = size1 + size3.pow(3);
    if size1 == 0 && size3 == 0 {
        return Err(parse_err(0, "missing LUT_1D_SIZE / LUT_3D_SIZE"));
    }
    if rows.len() != need {
        return Err(parse_err(0, format!("expected {need} rows, found {}", rows.len())));
    }
    let shaper = (size1 > 0).then(|| Lut1D {
        data: rows[..size1].to_vec(),
        domain_min: dmin1.unwrap_or(dmin),
        domain_max: dmax1.unwrap_or(dmax),
    });
    let lut = (size3 > 0).then(|| Lut3D {
        size: size3,
        data: rows[size1..].to_vec(),
        domain_min: if size1 > 0 { [0.0; 3] } else { dmin },
        domain_max: if size1 > 0 { [1.0; 3] } else { dmax },
    });
    Ok(CubeFile { title, shaper, lut })
}

/// Lustre/Autodesk .3dl: optional shaper line, then integer RGB rows
/// with blue varying fastest. Bit depth is inferred from the max value.
pub fn parse_3dl(text: &str) -> Result<Lut3D, Error> {
    let mut rows: Vec<[u32; 3]> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        let nums: Vec<u32> = line.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        match nums.len() {
            0 => continue,
            3 => rows.push([nums[0], nums[1], nums[2]]),
            // The shaper line lists the input grid positions; we assume an
            // evenly spaced grid, which is what every writer produces.
            _ if rows.is_empty() => continue,
            _ => return Err(parse_err(n + 1, "expected 3 integers")),
        }
    }
    let is_cube = |n: usize| (n as f64).cbrt().round().powi(3) as usize == n;
    // A 3-point shaper line looks like a data row; it is the odd one out.
    if !is_cube(rows.len()) && is_cube(rows.len() - 1) {
        rows.remove(0);
    }
    let size = (rows.len() as f64).cbrt().round() as usize;
    if size < 2 || size.pow(3) != rows.len() {
        return Err(parse_err(0, format!("{} rows is not a cube", rows.len())));
    }
    let max = rows.iter().flatten().copied().max().unwrap_or(1).max(1);
    let scale = (max + 1).next_power_of_two() as f32 - 1.0;
    let mut data = vec![[0.0; 3]; rows.len()];
    for (i, row) in rows.iter().enumerate() {
        let (r, g, b) = (i / (size * size), (i / size) % size, i % size);
        data[r + size * (g + size * b)] = row.map(|v| v as f32 / scale);
    }
    Ok(Lut3D { size, data, domain_min: [0.0; 3], domain_max: [1.0; 3] })
}

/// Loads a .cube or .3dl file by extension.
pub fn load(path: impl AsRef<Path>) -> Result<CubeFile, Error> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)?;
    let is_3dl = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("3dl"));
    if is_3dl {
        Ok(CubeFile { title: None, shaper: None, lut: Some(parse_3dl(&text)?) })
    } else {
        parse_cube(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn identity_is_exact_everywhere() {
        let lut = Lut3D::identity(17);
        for &p in &[[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.3, 0.71, 0.05], [0.999, 0.5, 0.123]] {
            assert!(close(lut.apply(p), p), "{p:?}");
        }
    }

    #[test]
    fn tetrahedral_reproduces_linear_functions() {
        // Any affine map is reproduced exactly, even on a coarse grid.
        let f = |[r, g, b]: [f32; 3]| [0.2 + 0.5 * r - 0.1 * b, 1.0 - g, 0.3 * r + 0.3 * g + 0.4 * b];
        let lut = Lut3D::from_fn(5, f);
        for &p in &[[0.13, 0.87, 0.42], [0.9, 0.1, 0.5], [0.5, 0.5, 0.5], [0.26, 0.24, 0.25]] {
            assert!(close(lut.apply(p), f(p)), "{p:?}");
        }
    }

    #[test]
    fn cube_round_trip_and_axis_order() {
        let invert = Lut3D::from_fn(3, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        let text = invert.to_cube_string(Some("invert"));
        let back = parse_cube(&text).unwrap();
        assert_eq!(back.title.as_deref(), Some("invert"));
        let lut = back.lut.unwrap();
        assert!(close(lut.apply([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0]));
        // Second data row of a .cube is r=1,g=0,b=0 (red fastest).
        let second = text.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit())).nth(1).unwrap();
        assert_eq!(second, "0.500000 1.000000 1.000000");
    }

    #[test]
    fn amount_blends() {
        let invert = Lut3D::from_fn(2, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b]);
        assert!(close(invert.apply_amount([1.0, 0.0, 0.5], 0.5), [0.5, 0.5, 0.5]));
    }

    #[test]
    fn parses_3dl_blue_fastest() {
        let mut text = String::from("0 1023\n");
        for r in 0..2 {
            for g in 0..2 {
                for b in 0..2 {
                    text += &format!("{} {} {}\n", r * 1023, g * 1023, b * 1023);
                }
            }
        }
        let lut = parse_3dl(&text).unwrap();
        assert!(close(lut.apply([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
        assert!(close(lut.apply([0.0, 0.0, 1.0]), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn rejects_wrong_row_count() {
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
    }
}
