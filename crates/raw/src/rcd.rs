//! RCD demosaicing (Ratio Corrected Demosaicing, Luis Sanz Rodríguez,
//! v2.3, as in RawTherapee and darktable), for Bayer sensors.
//!
//! 1. Vertical/horizontal discrimination from a squared high-pass filter on
//!    colour differences.
//! 2. A low-pass filter of the mosaic at the red and blue sites.
//! 3. Green at red/blue sites: north/south/west/east estimates scaled by the
//!    ratio of the low-pass values (the "ratio correction"), weighted by
//!    gradients, blended by the discrimination.
//! 4. Red at blue sites and blue at red sites along the diagonals, then red
//!    and blue at green sites, both on colour differences with green.
//!
//! Written from the published algorithm and checked by mosaicing real
//! photos and measuring the reconstruction (see the tests).
//!
//! The image is processed in tiles with their own small buffers, so memory
//! stays a few MB per thread instead of a dozen full-size planes.
//!
//! RCD assumes colour differences between channels are small, which holds
//! for white-balanced data, not for raw camera values (red is often less
//! than half of green): the mosaic is scaled by white balance `gains` first
//! and the result scaled back, so it stays camera RGB.
//!
//! RCD's ratio estimates are not mean-preserving: in fine texture (foliage)
//! they shift a channel's average by about 1%, a faint colour cast. After
//! demosaicing, in every 64×64 block the interpolated values of each colour
//! are scaled so their mean matches that of the real photosites of that
//! colour, the factors blended smoothly between blocks. On a real photo this
//! brings the channel means back to the truth and slightly raises PSNR.
//!
//! Every output value is clamped between 0 and the largest photosite in its
//! 5×5 neighbourhood. RCD's estimates can overshoot next to clipped areas
//! (blown skies), where no real value exceeds the sensor's; left alone,
//! those pixels would then be treated as clipped and turned white.

use rayon::prelude::*;

use crate::demosaic::{self, Cfa};
use crate::image::Image;

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

/// Output tile size and the margin read around it. The algorithm reaches
/// about six pixels; output closer than `VALID` to a tile edge is not used.
const TILE: usize = 256;
const MARGIN: usize = 10;
const VALID: usize = 8;

/// Radius of the clipping mask around clipped photosites. A pixel's value
/// comes from photosites within about two of it; farther ones only enter
/// the direction weights, where one clipped value matters little. A wider
/// mask throws away good data near highlights in HDR merges (in the
/// moving-disk merge test, radius 6 already changes which shots fill the
/// disk's edges).
pub const SUPPORT_RADIUS: usize = 2;

#[inline]
fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}

#[inline]
fn sqr(x: f32) -> f32 {
    x * x
}

/// RCD demosaic of the region `[x, y, w, h]` of a `width`×`height` Bayer
/// mosaic, with white balance `gains` (R, G, B multipliers; `[1.0; 3]` for
/// balanced data). Pixels within a few photosites of the mosaic border,
/// which RCD cannot reach, are filled by the bilinear method. `None` if
/// `cfa` is not Bayer.
pub fn rcd_region(mosaic: &[f32], width: usize, height: usize, cfa: &Cfa, region: [usize; 4], gains: [f32; 3]) -> Option<Image> {
    let mut out = demosaic_tiles(mosaic, width, height, cfa, region, gains)?;
    let [x0, y0] = [region[0].min(width), region[1].min(height)];
    restore_means(&mut out, |x, y| cfa.color_at(x0 + x, y0 + y));
    Some(out)
}

/// RCD without the mean correction: the same pixels whatever the region or
/// the tiling.
fn demosaic_tiles(mosaic: &[f32], width: usize, height: usize, cfa: &Cfa, region: [usize; 4], gains: [f32; 3]) -> Option<Image> {
    if !cfa.is_bayer() {
        return None;
    }
    assert_eq!(mosaic.len(), width * height);
    let [x0, y0] = [region[0].min(width), region[1].min(height)];
    let (rw, rh) = (region[2].min(width - x0), region[3].min(height - y0));
    // One band of tiles at a time: the tiles of a band run in parallel and
    // are copied into the output before the next band, so only a band's
    // worth of tiles is held besides the output.
    let mut out = Image::new(rw, rh);
    for ty in 0..rh.div_ceil(TILE) {
        let band: Vec<(usize, Image)> = (0..rw.div_ceil(TILE))
            .into_par_iter()
            .map(|tx| {
                // Output rectangle of this tile, in mosaic coordinates.
                let (ox, oy) = (x0 + tx * TILE, y0 + ty * TILE);
                let (ow, oh) = (TILE.min(x0 + rw - ox), TILE.min(y0 + rh - oy));
                // Window read around it.
                let (wx, wy) = (ox.saturating_sub(MARGIN), oy.saturating_sub(MARGIN));
                let (ww, wh) = ((ox + ow + MARGIN).min(width) - wx, (oy + oh + MARGIN).min(height) - wy);
                let rgb = rcd_window(mosaic, width, cfa, [wx, wy, ww, wh], gains);
                let tile = Image::from_fn(ow, oh, |x, y| {
                    let (mx, my) = (ox + x, oy + y);
                    let (lx, ly) = (mx - wx, my - wy);
                    let inside = lx >= VALID && ly >= VALID && lx + VALID < ww && ly + VALID < wh;
                    if inside {
                        let i = ly * ww + lx;
                        // In the balanced domain RCD works in.
                        let top = (my - 2..=my + 2)
                            .flat_map(|yy| (mx - 2..=mx + 2).map(move |xx| (xx, yy)))
                            .map(|(xx, yy)| mosaic[yy * width + xx] * gains[cfa.color_at(xx, yy)])
                            .fold(0.0f32, f32::max);
                        std::array::from_fn(|c| rgb[c][i].clamp(0.0, top) / gains[c])
                    } else {
                        // Only at the mosaic border (tiles inside have margin).
                        demosaic::bilinear_region(mosaic, width, height, cfa, [mx, my, 1, 1]).pixels[0]
                    }
                });
                (ox - x0, tile)
            })
            .collect();
        for (ox, tile) in band {
            let oy = ty * TILE;
            for y in 0..tile.height {
                let dst = (oy + y) * rw + ox;
                out.pixels[dst..dst + tile.width].copy_from_slice(&tile.pixels[y * tile.width..(y + 1) * tile.width]);
            }
        }
    }
    Some(out)
}

const BLOCK: usize = 64;

/// Scales interpolated values so that, block by block, each colour's mean
/// matches the mean of its real photosites (see the module notes).
fn restore_means(img: &mut Image, colour: impl Fn(usize, usize) -> usize + Sync) {
    let (w, h) = (img.width, img.height);
    let (bw, bh) = (w.div_ceil(BLOCK), h.div_ceil(BLOCK));
    let factors: Vec<[f32; 3]> = (0..bw * bh)
        .into_par_iter()
        .map(|b| {
            let (bx, by) = (b % bw, b / bw);
            let mut sum = [[0.0f64; 2]; 3];
            let mut n = [[0usize; 2]; 3];
            for y in by * BLOCK..((by + 1) * BLOCK).min(h) {
                for x in bx * BLOCK..((bx + 1) * BLOCK).min(w) {
                    let own = colour(x, y);
                    let p = img.pixels[y * w + x];
                    for c in 0..3 {
                        let k = usize::from(c != own);
                        sum[c][k] += p[c] as f64;
                        n[c][k] += 1;
                    }
                }
            }
            std::array::from_fn(|c| {
                if n[c][0] == 0 || n[c][1] == 0 || sum[c][1] <= 1e-9 {
                    return 1.0;
                }
                ((sum[c][0] / n[c][0] as f64) / (sum[c][1] / n[c][1] as f64)).clamp(0.8, 1.25) as f32
            })
        })
        .collect();
    img.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let gy = ((y as f32 + 0.5) / BLOCK as f32 - 0.5).clamp(0.0, (bh - 1) as f32);
        let (y0, fy) = (gy as usize, gy.fract());
        let y1 = (y0 + 1).min(bh - 1);
        for (x, p) in row.iter_mut().enumerate() {
            let gx = ((x as f32 + 0.5) / BLOCK as f32 - 0.5).clamp(0.0, (bw - 1) as f32);
            let (x0, fx) = (gx as usize, gx.fract());
            let x1 = (x0 + 1).min(bw - 1);
            let own = colour(x, y);
            for c in 0..3 {
                if c != own {
                    let f = |xx: usize, yy: usize| factors[yy * bw + xx][c];
                    let k = (f(x0, y0) * (1.0 - fx) + f(x1, y0) * fx) * (1.0 - fy) + (f(x0, y1) * (1.0 - fx) + f(x1, y1) * fx) * fy;
                    p[c] *= k;
                }
            }
        }
    });
}

/// Runs RCD on one window of the mosaic; returns the three colour planes.
fn rcd_window(mosaic: &[f32], width: usize, cfa: &Cfa, [wx, wy, w, h]: [usize; 4], gains: [f32; 3]) -> [Vec<f32>; 3] {
    let n = w * h;
    let fc = |x: usize, y: usize| cfa.color_at(wx + x, wy + y);
    let cfa_w: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .map(|(x, y)| mosaic[(wy + y) * width + wx + x].max(0.0) * gains[fc(x, y)])
        .collect();
    let c = &cfa_w;
    let (w1, w2, w3, w4) = (w, 2 * w, 3 * w, 4 * w);

    let mut rgb = [vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]];
    for y in 0..h {
        for x in 0..w {
            rgb[fc(x, y)][y * w + x] = c[y * w + x];
        }
    }
    if w < 2 * MARGIN || h < 2 * MARGIN {
        return rgb;
    }

    // Step 1: vertical / horizontal discrimination.
    let mut buf_v = vec![0.0f32; n];
    let mut buf_h = vec![0.0f32; n];
    for y in 3..h - 3 {
        for x in 3..w - 3 {
            let i = y * w + x;
            buf_v[i] = sqr((c[i - w3] - c[i - w1] - c[i + w1] + c[i + w3]) - 3.0 * (c[i - w2] + c[i + w2]) + 6.0 * c[i]);
            buf_h[i] = sqr((c[i - 3] - c[i - 1] - c[i + 1] + c[i + 3]) - 3.0 * (c[i - 2] + c[i + 2]) + 6.0 * c[i]);
        }
    }
    let mut vh_dir = vec![0.5f32; n];
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            let i = y * w + x;
            let v = (buf_v[i - w1] + buf_v[i] + buf_v[i + w1]).max(EPSSQ);
            let hh = (buf_h[i - 1] + buf_h[i] + buf_h[i + 1]).max(EPSSQ);
            vh_dir[i] = v / (v + hh);
        }
    }
    drop((buf_v, buf_h));

    // Step 2: low-pass filter at the red and blue sites.
    let mut lpf = vec![0.0f32; n];
    for y in 2..h - 2 {
        for x in 2..w - 2 {
            if fc(x, y) == 1 {
                continue;
            }
            let i = y * w + x;
            lpf[i] = c[i] + 0.5 * (c[i - w1] + c[i + w1] + c[i - 1] + c[i + 1]) + 0.25 * (c[i - w1 - 1] + c[i - w1 + 1] + c[i + w1 - 1] + c[i + w1 + 1]);
        }
    }

    // The "refined" discrimination: the centre's, unless its neighbours are
    // more decided.
    let refine = |dir: &[f32], i: usize| {
        let centre = dir[i];
        let around = 0.25 * (dir[i - w1 - 1] + dir[i - w1 + 1] + dir[i + w1 - 1] + dir[i + w1 + 1]);
        if (0.5 - centre).abs() < (0.5 - around).abs() {
            around
        } else {
            centre
        }
    };

    // Step 3: green at red and blue sites.
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            if fc(x, y) == 1 {
                continue;
            }
            let i = y * w + x;
            let disc = refine(&vh_dir, i);
            let n_grad = EPS + (c[i - w1] - c[i + w1]).abs() + (c[i] - c[i - w2]).abs() + (c[i - w1] - c[i - w3]).abs() + (c[i - w2] - c[i - w4]).abs();
            let s_grad = EPS + (c[i - w1] - c[i + w1]).abs() + (c[i] - c[i + w2]).abs() + (c[i + w1] - c[i + w3]).abs() + (c[i + w2] - c[i + w4]).abs();
            let w_grad = EPS + (c[i - 1] - c[i + 1]).abs() + (c[i] - c[i - 2]).abs() + (c[i - 1] - c[i - 3]).abs() + (c[i - 2] - c[i - 4]).abs();
            let e_grad = EPS + (c[i - 1] - c[i + 1]).abs() + (c[i] - c[i + 2]).abs() + (c[i + 1] - c[i + 3]).abs() + (c[i + 2] - c[i + 4]).abs();
            let l = lpf[i];
            let n_est = c[i - w1] * (l + l) / (EPS + l + lpf[i - w2]);
            let s_est = c[i + w1] * (l + l) / (EPS + l + lpf[i + w2]);
            let w_est = c[i - 1] * (l + l) / (EPS + l + lpf[i - 2]);
            let e_est = c[i + 1] * (l + l) / (EPS + l + lpf[i + 2]);
            let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
            let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
            rgb[1][i] = intp(disc, h_est, v_est);
        }
    }
    drop(lpf);

    // Step 4.0–4.1: diagonal discrimination at red and blue sites.
    let mut p_hpf = vec![0.0f32; n];
    let mut q_hpf = vec![0.0f32; n];
    for y in 3..h - 3 {
        for x in 3..w - 3 {
            if fc(x, y) == 1 {
                continue;
            }
            let i = y * w + x;
            p_hpf[i] = sqr((c[i - w3 - 3] - c[i - w1 - 1] - c[i + w1 + 1] + c[i + w3 + 3]) - 3.0 * (c[i - w2 - 2] + c[i + w2 + 2]) + 6.0 * c[i]);
            q_hpf[i] = sqr((c[i - w3 + 3] - c[i - w1 + 1] - c[i + w1 - 1] + c[i + w3 - 3]) - 3.0 * (c[i - w2 + 2] + c[i + w2 - 2]) + 6.0 * c[i]);
        }
    }
    let mut pq_dir = vec![0.5f32; n];
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            if fc(x, y) == 1 {
                continue;
            }
            let i = y * w + x;
            let p = (p_hpf[i - w1 - 1] + p_hpf[i] + p_hpf[i + w1 + 1]).max(EPSSQ);
            let q = (q_hpf[i - w1 + 1] + q_hpf[i] + q_hpf[i + w1 - 1]).max(EPSSQ);
            pq_dir[i] = p / (p + q);
        }
    }
    drop((p_hpf, q_hpf));

    // Step 4.2: red at blue sites and blue at red sites.
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            let native = fc(x, y);
            if native == 1 {
                continue;
            }
            let i = y * w + x;
            let k = 2 - native;
            let disc = refine(&pq_dir, i);
            let (rc, g) = (&rgb[k], &rgb[1]);
            let nw_grad = EPS + (rc[i - w1 - 1] - rc[i + w1 + 1]).abs() + (rc[i - w1 - 1] - rc[i - w3 - 3]).abs() + (g[i] - g[i - w2 - 2]).abs();
            let ne_grad = EPS + (rc[i - w1 + 1] - rc[i + w1 - 1]).abs() + (rc[i - w1 + 1] - rc[i - w3 + 3]).abs() + (g[i] - g[i - w2 + 2]).abs();
            let sw_grad = EPS + (rc[i + w1 - 1] - rc[i - w1 + 1]).abs() + (rc[i + w1 - 1] - rc[i + w3 - 3]).abs() + (g[i] - g[i + w2 - 2]).abs();
            let se_grad = EPS + (rc[i + w1 + 1] - rc[i - w1 - 1]).abs() + (rc[i + w1 + 1] - rc[i + w3 + 3]).abs() + (g[i] - g[i + w2 + 2]).abs();
            let nw_est = rc[i - w1 - 1] - g[i - w1 - 1];
            let ne_est = rc[i - w1 + 1] - g[i - w1 + 1];
            let sw_est = rc[i + w1 - 1] - g[i + w1 - 1];
            let se_est = rc[i + w1 + 1] - g[i + w1 + 1];
            let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
            let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);
            let v = g[i] + intp(disc, q_est, p_est);
            rgb[k][i] = v;
        }
    }

    // Step 4.3: red and blue at green sites.
    for y in 4..h - 4 {
        for x in 4..w - 4 {
            if fc(x, y) != 1 {
                continue;
            }
            let i = y * w + x;
            let disc = refine(&vh_dir, i);
            let g = &rgb[1];
            let g0 = g[i];
            let n1 = EPS + (g0 - g[i - w2]).abs();
            let s1 = EPS + (g0 - g[i + w2]).abs();
            let w1g = EPS + (g0 - g[i - 2]).abs();
            let e1 = EPS + (g0 - g[i + 2]).abs();
            let (gn, gs, gw, ge) = (g[i - w1], g[i + w1], g[i - 1], g[i + 1]);
            let mut out = [0.0f32; 2];
            for (slot, k) in [0usize, 2].into_iter().enumerate() {
                let rc = &rgb[k];
                let sn = (rc[i - w1] - rc[i + w1]).abs();
                let ew = (rc[i - 1] - rc[i + 1]).abs();
                let n_grad = n1 + sn + (rc[i - w1] - rc[i - w3]).abs();
                let s_grad = s1 + sn + (rc[i + w1] - rc[i + w3]).abs();
                let w_grad = w1g + ew + (rc[i - 1] - rc[i - 3]).abs();
                let e_grad = e1 + ew + (rc[i + 1] - rc[i + 3]).abs();
                let n_est = rc[i - w1] - gn;
                let s_est = rc[i + w1] - gs;
                let w_est = rc[i - 1] - gw;
                let e_est = rc[i + 1] - ge;
                let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                out[slot] = g0 + intp(disc, h_est, v_est);
            }
            rgb[0][i] = out[0];
            rgb[2][i] = out[1];
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mosaic_of(w: usize, h: usize, cfa: &Cfa, f: impl Fn(usize, usize) -> [f32; 3]) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)[cfa.color_at(x, y)]).collect()
    }

    #[test]
    fn flat_colour_is_exact_for_every_bayer_phase() {
        let colour = [0.6, 0.3, 0.1];
        for name in ["RGGB", "GRBG", "GBRG", "BGGR"] {
            let cfa = Cfa::bayer(name).unwrap();
            let (w, h) = (300, 270);
            let m = mosaic_of(w, h, &cfa, |_, _| colour);
            let img = rcd_region(&m, w, h, &cfa, [0, 0, w, h], [1.0; 3]).unwrap();
            for p in &img.pixels {
                assert!(p.iter().zip(colour).all(|(a, b)| (a - b).abs() < 1e-5), "{name} {p:?}");
            }
        }
    }

    #[test]
    fn unbalanced_flat_colour_is_exact_with_its_gains() {
        // A grey under daylight as a camera sees it: red and blue low.
        let raw = [0.20, 0.50, 0.30];
        let gains = [2.5, 1.0, 5.0 / 3.0];
        let cfa = Cfa::bayer("BGGR").unwrap();
        let (w, h) = (280, 260);
        let m = mosaic_of(w, h, &cfa, |_, _| raw);
        let img = rcd_region(&m, w, h, &cfa, [0, 0, w, h], gains).unwrap();
        for p in &img.pixels {
            assert!(p.iter().zip(raw).all(|(a, b)| (a - b).abs() < 1e-5), "{p:?}");
        }
    }

    #[test]
    fn regions_and_tiles_agree() {
        let cfa = Cfa::bayer("GRBG").unwrap();
        let (w, h) = (600, 530);
        let m = mosaic_of(w, h, &cfa, |x, y| {
            let t = ((x * 7 + y * 3) % 23) as f32 / 23.0;
            [0.2 + 0.5 * t, 0.3 + 0.4 * ((x / 9 + y / 11) % 2) as f32, 0.6 - 0.3 * t]
        });
        let full = demosaic_tiles(&m, w, h, &cfa, [0, 0, w, h], [1.0; 3]).unwrap();
        for region in [[13, 7, 400, 300], [256, 256, 300, 200], [1, 0, 599, 530]] {
            let r = demosaic_tiles(&m, w, h, &cfa, region, [1.0; 3]).unwrap();
            assert_eq!(r, full.crop(region[0], region[1], region[2], region[3]), "{region:?}");
        }
    }

    #[test]
    fn not_for_x_trans() {
        let rows = ["GGRGGB", "GGBGGR", "BRGRBG", "GGBGGR", "GGRGGB", "RBGBRG"];
        let colors = rows.concat().chars().map(|c| "RGB".find(c).unwrap() as u8).collect();
        let cfa = Cfa { width: 6, height: 6, colors };
        assert!(rcd_region(&[0.0; 36], 6, 6, &cfa, [0, 0, 6, 6], [1.0; 3]).is_none());
    }
}
