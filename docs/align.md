# Alignment

The [`align`](../crates/align) crate estimates the geometric transform between two shots of the same scene and resamples one onto the other. It is written for hand-held exposure brackets (HDR merge) and meant to be reused by focus stacking and panoramas, so it depends only on `rayon`: no RAW decoding, no colour.

```rust
let a = align::align(&reference, &moving, &AlignParams::default())?;   // Plane = values + optional weight
if a.converged {
    let t = a.transform.half_to_full();                                // estimated at half size
    let (rgb, coverage) = align::warp_rgb(&shot, w, h, &t, w, h);
}
```

## Conventions

- **`Transform`** is a 3×3 projective matrix from **reference** pixel coordinates to **moving** pixel coordinates: the reference point `p` shows the same scene point as the moving image at `T(p)`.
- Coordinates are continuous, with the origin on the outer corner of the top-left pixel: **pixel centres are at `+0.5`**. Then a change of resolution is a plain scaling, `T_full = S · T_half · S⁻¹` with `S = diag(2, 2, 1)` (`Transform::scaled`, `half_to_full`): half-size pixel `i`, the average of full pixels `2i` and `2i + 1`, is centred on their shared edge `2i + 1 = 2 · (i + 0.5)`. Scaling index coordinates (centres at integers) instead is a classic half-pixel slip; `tests/scale.rs` covers it.
- **Weights** (`Plane::weight`, 0..1) mark pixels to ignore in either image: clipped or below the noise floor for HDR.

## Method

| Step | What | Where |
|---|---|---|
| 1 | Gaussian pyramid: level 0 is the input with a light [1 2 1] blur (weights eroded by one pixel); each further level halves the previous with a [1 3 3 1]/8 kernel centred on the new pixel centre, so coordinates scale exactly by 2. It stops once the long side is ≤ 192 px (~100–190 px) | [`plane.rs`](../crates/align/src/plane.rs) |
| 2 | Coarse start on the smallest level: exhaustive integer translation search by weighted NCC within ±`coarse_search` (12%) of the long side | [`coarse.rs`](../crates/align/src/coarse.rs) |
| 3 | **ECC** (Enhanced Correlation Coefficient, Evangelidis & Psarakis 2008) in the inverse compositional form: the reference's gradients, the Jacobian and the zero-mean statistics are fixed per level, each iteration samples the moving image at `T(x)` and solves the closed-form ECC step, which is invariant to gain and offset between the shots. Weighted by both masks. Coarse to fine with a growing model: translation, Euclidean on the third finest level, the requested model on the two finest. Stops when an update moves the corners less than `tolerance` (1e-3 px) or after `max_iterations` (30) | [`ecc.rs`](../crates/align/src/ecc.rs) |
| 4 | Levels larger than `max_pixels` (1 Mpx) use only the most textured 128×128 tiles, the best few in each cell of a 4×4 grid so the perspective terms stay constrained | [`lib.rs`](../crates/align/src/lib.rs) |
| 5 | Quality: the result is accepted when ECC converged on the finest level, the final correlation ρ ≥ `min_correlation` (0.85) and no corner moves, relative to the image centre, by more than `max_corner_shift` (10%) of the long side. Otherwise the next simpler model is tried (homography → similarity → translation). If none passes, the best attempt is returned with `converged = false` | [`lib.rs`](../crates/align/src/lib.rs) |

Bilinear sampling of the moving image on the coarse levels, Catmull-Rom on the finest (bilinear's position-dependent blur biases the sub-pixel estimate).

The corner check measures the *deformation* (rotation, scale, perspective), not the plain shift, because the shift is already bounded by the coarse search: a valid 11% shift would otherwise be rejected and replaced by a worse model.

## Warp

`warp_rgb` and `warp_plane` map every output pixel centre through the transform and sample the source there (inverse mapping). They return the values and the **coverage**: `false` where the point falls outside the source, so a merge gives that shot zero weight there. Interpolation:

| `Interp` | Use |
|---|---|
| `CatmullRom` (and `warp_rgb`) | images: bicubic 4×4, limited to the min/max of the 4 nearest pixels, so no ringing near clipped areas |
| `Bilinear` | cheap previews |
| `Max` | masks (e.g. clipping): the maximum of the 4 nearest pixels, so a flagged pixel is never interpolated away |

The identity and integer translations are exact.

## Accuracy and speed

Synthetic tests (`crates/align/tests/`) sample a continuous textured scene under known transforms, with noise and a different gain and offset for the moving shot, at 900×600:

| Case | Corner RMS error |
|---|---|
| translation 3.3 / −7.7 px | 0.006 px |
| rotation 0.4° | 0.003 px |
| rotation 0.4° + scale 1.002 | 0.004 px |
| mild homography (corners move > 4 px) | 0.005 px |
| 25% of the moving shot clipped, weight 0 | 0.005 px |
| half size → full resolution (2×2 binning, 1200×800) | 0.002 px |

Threshold in the tests: 0.1 px (0.05 px for half → full). Two unrelated images give ρ ≈ 0.08 and `converged = false`.

Timings with `cargo test --release -p align --test timing -- --ignored --nocapture` (Apple Silicon, homography, including the pyramid): **1500 × 1000 px ≈ 30 ms**, **3000 × 2000 px ≈ 50 ms**; `warp_rgb` of the same frame 5 ms / 20 ms.

## Not yet done

- `features.rs` (FAST + ORB + RANSAC) to initialise panoramas, whose overlap is too small for the coarse NCC.
- Automatic crop to the shots' common area (belongs to the merge).
