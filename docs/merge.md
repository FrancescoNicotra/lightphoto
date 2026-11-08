# HDR merge

The [`merge`](../crates/merge) crate merges 2–9 bracketed RAW files into one floating point DNG, like Lightroom's *Photo Merge → HDR*. The merge happens in **linear camera RGB**, after the demosaic and before white balance and colour matrices, so the result is a `LinearRaw` DNG that develops like the original RAW files: white balance, Adobe profiles, presets and masks all apply.

```rust
let sources = paths.into_iter().map(merge::Source::File).collect();
let (merged, written) = merge::merge_hdr(sources, None, &MergeOptions::default(), &Control::default())?;
// written.path == "<folder>/<first>-HDR.dng"
```

```text
lumen-cli hdr a.RW2 b.RW2 c.RW2 [-o out.dng] [--no-align] [--deghost none|low|medium|high]
              [--auto-crop] [--uncompressed] [--timings] [--ghost-mask m.png] [--no-adobe]
```

## Two passes

| Pass | What | Memory |
|---|---|---|
| 1, half size, all shots | decode (`raw::load_capture`, half size, with the clipping mask), validate, EXIF exposures, align, warp, exposure ratios from the pixels, ghost masks, BaselineNoise, auto crop | the half-size shots, 13 bytes/pixel each |
| 2, full size, one shot at a time | decode → warp into the reference frame in bands of 256 rows → accumulate `Σ w·v/e` (3 × f32) and `Σ w` (f32), plus the ghost sources `G`, `M` over the rectangle the ghost masks reach | accumulators 16 bytes/pixel (+16 in the ghost rectangle) + one decoded shot |
| output | scale + `BaselineExposure`, preview, XMP, `raw::write_linear_dng` | the merged image |

[`prepare`](../crates/merge/src/lib.rs) runs pass 1 and returns a `Prepared` (frames, reference, warnings, crop, ghosts, the half-size shots for a preview: `Prepared::preview()`); `Prepared::merge` runs pass 2 and `Merged::write_dng` writes the file. `merge_hdr` does all three. A `Control` carries a progress callback (`Stage`, fraction) and a cancellation flag, checked between shots and between bands; a cancelled merge writes nothing.

`Source` is a file, or a full-size `Capture` already in memory (tests; its half-size copy is a 2×2 binning).

## Checks ([`bracket.rs`](../crates/merge/src/bracket.rs))

| Case | Result |
|---|---|
| < 2 or > 9 shots | `MergeError::FrameCount` |
| JPEG/PNG/TIFF, or a floating point DNG (already a merge) | `NotRaw` with the file names |
| different Make/Model | `DifferentCameras` |
| different sensor size, default crop or CFA | `DifferentSensors` |
| different orientation | `DifferentOrientations` |
| focal length differs by more than 2% | warning |
| ISO varies by more than 2 stops | warning |

## Exposures ([`exposure.rs`](../crates/merge/src/exposure.rs))

A shot's value is `v ≈ X · e` until it clips, with `e = t · ISO / N²` from EXIF (ISO and aperture are left out when some shot lacks them). The **reference** is the shot with the median exposure (the darker of two); exposures are relative to it.

EXIF values are nominal (1/125 s is really 1/128), so they are refined on the aligned half-size shots: between shots adjacent in exposure, over the pixels valid in both (maximum channel in 0.03–0.85, not clipped, covered by both), `r = exp(median(ln Sⱼ − ln Sᵢ))` with `S` the channel sum; the ratios are chained to the reference.

- at least 0.5% of the pixels usable → the measured ratio; if it differs from EXIF by 0.3 EV or more, a warning;
- otherwise EXIF; with neither, `NoExposure` (leave that shot out);
- no exposure time in EXIF → a warning, the shots are ordered by brightness and measured;
- all ratios under 0.15 EV → a "not a bracket" warning: the merge becomes a noise-reducing average.

## Weights ([`weights.rs`](../crates/merge/src/weights.rs))

`X = Σ wᵢ · vᵢ/eᵢ / Σ wᵢ` with `wᵢ = eᵢ · h(mᵢ)`:

- `h(m) = 1 − smoothstep(0.80, 0.95, m)`, `m` the maximum channel, and `h = 0` on the shot's clipping mask. One weight per pixel, not per channel, so a single clipped channel cannot shift the hue.
- `eᵢ`: with photon noise the variance of `vᵢ/eᵢ` is about `X/eᵢ`, so inverse-variance weighting is proportional to the exposure. The brightest unclipped shot dominates and hands over **continuously** to darker ones as it nears clipping: no seams, and no ramp towards black is needed.
- Where every shot is clipped, the darkest one is used, moved onto the camera neutral (`n · maxᶜ(vᶜ/nᶜ)`) so blown highlights stay white after white balance. It is implemented as a floor weight of 10⁻⁴ on the darkest shot (10⁻⁸ on the reference, for the borders the darkest one does not cover).
- Small negative values (below black) are kept until the output, so shadows do not rise.

## Alignment ([`alignment.rs`](../crates/merge/src/alignment.rs))

With `align` (default), each shot is aligned on the half-size copies to its **neighbour in exposure** towards the reference, and the transforms are composed: neighbours share more valid pixels than the extremes of a bracket (risk 6 of the plan). Planes for [`align`](align.md): log₂ of the mean channel, weight 0 where `h < 0.5`, clipped, or below 0.01.

**Model.** Each pair is aligned with a **similarity** and with a **homography**; the homography is used only when it lowers the residual `1 − ρ` by at least 20%. Shots 2 EV apart share only a band of valid pixels, where the homography's perspective terms are poorly constrained: on the synthetic ±2 EV bracket the homography fitted a pure translation with 0.40 px corner error at full size, the similarity with 0.04 px. A real perspective change (corners moving 8 px, ±1 EV) still selects the homography (0.12 px).

**Moving objects.** A large object that moves between the shots lowers the correlation at the right place, and can win instead: the coarse search lines it up and ECC converges there (on the synthetic moving disk, a 64 px "translation" with ρ 0.875 passed `align`'s 0.85 check, the gap noted in the F3 hand-over; at 24 MP even ECC started from the identity slid onto the object). So when the plain result is missing or has ρ < 0.98, a **robust pass** runs:

1. a similarity on planes with **normalised local contrast**, `(L − μ)/√(σ² + 0.05²)` over a Gaussian of 12 px, softly limited to ±3: an object then counts by its outline, not its area, so the texture leads;
2. residuals of the plain planes under that transform; pixels beyond `max(5σ_MAD, 0.3 EV)`, grown by 2 px, are dropped (at least half the usable pixels must remain);
3. the similarity/homography selection again on the plain planes without them, from the identity.

The robust result wins if it converges with a higher ρ. The normalised planes are not used for the final estimate: they amplify noise (0.29 px instead of 0.06 px on the static bracket, and the perspective case no longer selected the homography).

A link that fails (not converged) is retried directly against the reference; if that fails too, the shot stays unaligned with an `AlignmentFailed` warning, and ghost removal is raised to at least Medium.

Half size → full size: `T_full = A · T_half · A⁻¹`, with `A` = scale 2 plus, for Bayer files whose crop starts on an odd photosite, a one-pixel offset (half-size cells are whole 2×2 cells starting at the first even photosite). The shots are warped with `align::warp_rgb` (Catmull-Rom limited to the 4 nearest) and the clipping mask with `Interp::Max`; outside its coverage a shot has weight 0. The reference covers everything.

**Auto crop** (`auto_crop`): the largest rectangle with the image's proportions, nearest the centre, inside the area every shot covers (with a half-size pixel of margin), searched on a ≤ 512 px grid. It is written as `crs:HasCrop`/`crs:Crop*` in the DNG's XMP, so it stays editable; no pixel is cut.

## Ghost removal ([`ghost.rs`](../crates/merge/src/ghost.rs))

On the aligned half-size shots, with `deghost` Low, Medium or High (default None, as in Lightroom):

1. **Noise model** fitted on the shots themselves: for each pair adjacent in exposure, in 40 bins of radiance, the robust variance `(1.4826 · MAD)²` of the difference of the normalised luminances `Y/e`; then a least-squares fit (relative errors) of `σ²(v) = a·v + b` for the luminance of a half-size pixel. On the synthetic shots it recovers the simulated photon noise within a few percent: `a` = 2.9e-6 for the RGB captures (the mean of 12 samples of variance v/30000) and 9.5e-6 for the Bayer half size (R, the mean of two G, B).
2. **Inconsistency**: for each shot `i` and each pixel where it is valid (`h ≥ 0.5`, covered), against the reference, or, where the reference is clipped or dark (< 0.002), against the valid shot nearest the reference in exposure: `z = d / √(σᵢ² + σ_c²)`, the variances taken at the level of the brighter (less noisy) of the two. `d` is measured against the **range of the other shot over the pixel and its 4 neighbours**, both ways (`d = min(dist(Xᵢ, range X_c), dist(X_c, range Xᵢ))`): sub-pixel misregistration on sharp edges (warp interpolation, Bayer sampling) is not motion. Without it, the hand-held DNG bracket produced 18 false regions along the colour edges.
3. **Motion mask**: `max z > τ`, opening and closing (square, radius `open`), dilation, regions below the minimum area dropped, 8-connected components.
4. **One source per region**: the reference if its mean `h` over the region is ≥ 0.5, else the shot with the best mean `e·h`.
5. **Blend**: each source's region mask is grown by half the feather and blurred (Gaussian ≈ three box passes, σ = feather/4), so a region is entirely its source and fades out around it; overlapping feathers of different sources are normalised to sum ≤ 1. At full resolution the masks are sampled bilinearly and `out = X · (1 − Σ mᵢ) + Σ mᵢ · vᵢ/eᵢ` (`G` and `M` accumulators, only over the rectangle the masks reach).

| Level | τ (z) | opening | dilation | minimum area | feather |
|---|---|---|---|---|---|
| None | — | — | — | — | — |
| Low | 6 | 0.2% | 0.3% | 0.05% | 0.5% |
| Medium | 4 | 0.2% | 0.6% | 0.02% | 1% |
| High | 2.5 | 0.1% | 1.2% | 0.01% | 1.5% |

Sizes are fractions of the long side (areas of the image area), at least one pixel. `Prepared::ghosts` / `Merged::ghosts` give the noise model, the motion mask, the regions with their source and the overlay `Σ mᵢ`; `lumen-cli hdr --ghost-mask m.png` writes it (white = motion, grey = feather), half size, oriented.

Parallax of near objects (which a homography cannot model) shows up as motion and is fixed the same way, at the price of noisier single-source regions.

## Output ([`output.rs`](../crates/merge/src/output.rs))

- **Scale**: `s = 1 / percentile₉₉.₉₉(maxᶜ X)`; the data written is `min(X · s, 1)` (the brightest 0.01% are speculars; readers disagree on values above white).
- **`BaselineExposure`** = the reference's + `log₂(1/s)`: at default settings the DNG looks like the reference, and the highlights above it are recoverable with Exposure/Highlights.
- **`BaselineNoise`** = median over the pixels of `min(1, 1/√Σ eᵢ·hᵢ)`: the merge's noise relative to the reference alone, under photon noise.
- **XMP**: `xmp:CreatorTool="Lumen x.y"` plus the auto crop.
- **Preview**: the written data developed by `pipeline` with default settings (and the crop), Adobe profiles if installed, ≤ 1024 px, sensor orientation.
- Colour tags, EXIF, orientation and camera names come from the reference shot (`LinearDng::from_capture`); see [linear-dng.md](linear-dng.md) for the file format.
- Name: `<first file>-HDR.dng` in the first file's folder, then `-HDR-2.dng`…; written to `.tmp` and renamed, so a failure or cancellation leaves no partial file.

## Tests

`crates/merge/tests/` builds brackets from a continuous synthetic scene (`tests/common/scene.rs`): a 14-stop gradient (2⁻⁸…2⁶), texture, colour cells with sharp edges, a flat shadow patch and a light source clipped in every shot; each shot samples the scene through a known transform, adds photon and read noise and quantises to 14 bits. Two paths: captures built in memory, and Bayer DNGs with an EXIF IFD (exposure time, f-number, ISO) on a sensor whose crop starts at (1, 1), through rawler and the bilinear demosaic.

| Test | Metric | Threshold | Measured |
|---|---|---|---|
| radiance, 3 shots ±2 EV (memory) | mean \|log₂(out/R)\| over 8×8 blocks where the darkest shot is valid | < 0.02 EV | 0.0014 EV |
| radiance, 5 and 9 shots ±1 EV, shuffled order | same | < 0.02 EV | 0.0012 / 0.0007 EV |
| radiance, DNG files, hand-held | same, 16 px margin | < 0.02 EV | 0.0048 EV |
| dynamic range | pixels clipped in the reference recovered within 0.05 EV | > 99% | 100% |
| noise | SNR gain in the shadow patch vs the reference alone | ≥ 0.8·√(e_max/e_ref) = 1.6 | 2.33 |
| exposures without EXIF | EV error | < 0.03 EV | 0.0001 EV |
| nominal EXIF (1/125, 1/30, 1/8 vs 1/128, 1/32, 1/8) | EV error | < 0.02 EV | 0.0001 EV |
| alignment, memory (shift 3.3/−7.7; rotation 0.4° + scale 1.002) | corner RMS at full size | < 0.25 px | 0.044 / 0.057 px |
| alignment, DNG files (odd crop) | same | < 0.25 px | 0.084 / 0.101 px |
| perspective (corners move 8 px) | same, homography selected | < 0.25 px | 0.115 px |
| DNG round trip | `write` → `Photo::load`, relative error | ≤ 1e-3 | 4.9e-4 |
| tags | BaselineExposure, neutral, ColorMatrix, orientation, XMP crop, preview | identical | ✓ |
| ghosts, disk moving across 3 shots (Medium) | IoU of the motion mask with the union of the disk positions | > 0.8 | 0.844 (tripod), 0.843 (hand-held), 0.850 (DNG, hand-held) |
| same | dark blobs in the output | 1, at the reference's disk | 1 |
| same | pixels where only the other shots had the disk, darker than 85% of the background | — | 0% (50% without ghost removal) |
| same, hand-held | alignment despite the moving disk, corner RMS | < 0.25 px | 0.014–0.155 px |
| no false ghosts, static scene (High) | motion mask area | < 0.5% | 0.000% (tripod and hand-held) |
| levels on the moving disk | mask area Low ≤ Medium ≤ High | ordered | 4.15 / 4.71 / 5.58% |

Also covered: errors of the table above, cancellation (no file left), progress stages, `-HDR-2` naming, a merged DNG refused as input, the all-clipped light source staying neutral, an unrelated shot failing alignment with a warning and forcing ghost removal to Medium.

## Speed and memory

`tests/timing.rs` writes a synthetic hand-held 3 × 24 MP (6000 × 4000) bracket of Bayer DNGs with a large dark disk (radius 350 px) walking across it (`LUMEN_BRACKET_DIR=… cargo test --release -p merge --test timing -- --ignored --nocapture`), then `lumen-cli hdr … --timings` (release, Apple Silicon, 10 cores; median of 3 runs, ms):

| Step | static scene, no ghost removal | moving disk, Medium |
|---|---|---|
| decode, half size (3 shots) | 116–144 | 116 |
| align + warp, half size | 221–249 | 906 (robust pass for the shot with the disk) |
| exposures | 8 | 8 |
| ghosts | — | 162 |
| decode, full size | 226–232 | 226 |
| warp + accumulate | 215–244 | 216 |
| scale + preview | 23 | 23 |
| write (half float, Deflate) | 250 | 259 |
| **total** | **1.1–1.3 s** | **1.9 s** |

Plan targets: alignment < 2 s, total < 8 s. Memory: the peak of **live heap** is 1.24 GB without ghosts and 1.32 GB with the moving disk at High (pass 1: 1.16 GB, pass 2: 1.32 GB, write: 0.43 GB; `peak_heap_of_a_24mp_merge` counts it with a counting allocator), under the 1.5 GB target. The process footprint that `/usr/bin/time -l` reports is higher (2.0–2.4 GB), because macOS's allocator keeps freed large blocks mapped. Most of pass 2's peak is the full-size decode in `raw` (≈ 850 MB for 24 MP: the rawler mosaic, its f32 copy, the normalised mosaic, the demosaiced image and its crop) on top of the 0.4 GB of accumulators; trimming that decode is the next lever if 45–60 MP sensors need it.

## Not yet done

- Auto tone on the merge (needs local Highlights/Shadows, F0).
- The app: menu, dialog with preview, background merge (F5).
- Verification on real brackets and comparison with Lightroom's merge (F6): how Adobe picks the scale, `BaselineExposure`, `LinearResponseLimit` and noise tags; tuning the thresholds.
