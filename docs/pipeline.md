# Develop pipeline

Implemented in the [`pipeline`](../crates/pipeline) crate. Every Camera Raw panel is translated into an operation of our own. Adobe's PV2012+ algorithms are not public, so the sliders move in the same direction with a similar strength, but the results are not identical. `tools/compare` measures the difference.

## Order

| # | Stage | Space | Notes |
|---|---|---|---|
| 1 | **White balance** | camera → linear ProPhoto | `As Shot`, `Custom` (absolute Temperature/Tint through the DNG matrices), named presets (Daylight, Cloudy…), `Auto` (grey world). On JPEG/TIFF: `IncrementalTemperature/Tint` (±100 mired). |
| 1a | **Straighten / crop** | linear ProPhoto | `CropAngle` turns the oriented photo about its centre (bilinear), then `CropTop/Left/Bottom/Right` cut the frame ([`geometry.rs`](../crates/pipeline/src/geometry.rs)). Done first, so the post-crop vignette and every spatial stage see the final frame |
| 1b | **Camera profile HueSatMap** | linear ProPhoto | only with an Adobe DCP (see below), interpolated between the two illuminants like the matrices |
| 2 | **Calibration** | linear 3×3 matrix | each Rec.709 primary rotated in hue (±100 ≈ ±25°) and scaled in saturation; white stays white |
| 3 | **Exposure** | linear | `Exposure2012` + the DNG `BaselineExposure` |
| 4 | **Shoulder** (raw only) | linear | identity up to 0.7, then `tanh`: highlights above clipping (reconstructed at decoding, see below) stay recoverable |
| 4b | **LookTable** | linear ProPhoto | the Look's (e.g. Adobe Color), otherwise the DCP's |
| 5 | **Profile curve** | linear / Melissa* | raw only: the DCP `ProfileToneCurve` or, when missing (as in Adobe Standard, or without a DCP), the DNG SDK default **ACR3** curve ([`acr3.rs`](../crates/pipeline/src/acr3.rs)); applied in linear with the SDK's hue-preserving "RGB tone" method. Then the Look's curve (Adobe Color: a gentle S) |
| 6 | **Profile RGB table** | declared primaries and gamma | `crs:RGBTable` of creative profiles, with Amount |
| 7 | **Basic** | Melissa | Contrast (power curve around mid grey), Highlights/Shadows (bells centred at 0.75 and 0.25), Whites/Blacks (ends). **Global**, not local as in Adobe |
| 8 | **Curves** | Melissa | parametric (4 regions with splits) → master points → R/G/B, monotone cubic spline (Fritsch–Carlson) |
| 9 | **ShadowTint** | Melissa | green/magenta in the shadows |
| 9b | **Local corrections (point-wise)** | Melissa | for each mask, through its weight map: Exposure and Temperature/Tint as gains in linear, Contrast/Highlights/Shadows/Whites/Blacks through the Basic tone curve, Saturation around the luma ([`masks.rs`](../crates/pipeline/src/masks.rs)) |
| 10 | **Colour NR** | Melissa | chroma blur, radius ∝ Amount |
| 11 | **Texture / Clarity** | Melissa luma | difference from a blur (0.15% / 1.2% of the long edge), weighted towards the midtones |
| 12 | **Dehaze** | Melissa | global veil model |
| 13 | **Sharpening** | Melissa luma | unsharp mask: Radius, Detail (halo limit), Masking (edges only) |
| 13b | **Local corrections (spatial)** | Melissa | Texture/Clarity, Dehaze and Sharpness of each mask: the global operation on a copy, blended in through the weights |
| 14 | **Post-crop vignette** | Melissa | Amount, Midpoint, Roundness, Feather |
| 15 | **HSL** | encoded Rec.709 → (Y, a, b) | 8 overlapping bands whose weights sum to 1. Hue ±100 = ±40°, Saturation ±100 = ±13% chroma. Luminance leaves greys alone |
| 16 | **Vibrance / Saturation** | (Y, a, b) | vibrance acts more on dull colours and protects skin tones |
| 17 | **Colour grading** | (Y, a, b) | luminance bells for shadows/midtones/highlights (Blending sets their width, Balance their centre), plus the global wheel |
| 18 | **.cube LUT** (`--lut`) | encoded sRGB | last, with an amount |
| 19 | **Grain** | after resizing | two-octave value noise (Size, Frequency = Roughness), fixed seed |

\* *Melissa* = ProPhoto primaries with the sRGB transfer curve, the space of Lightroom's curves and RGB readouts.

## Demosaicing

Full-size Bayer decodes use **RCD** (Ratio Corrected Demosaicing, Luis Sanz Rodríguez, v2.3, as in RawTherapee and darktable; [`crates/raw/src/rcd.rs`](../crates/raw/src/rcd.rs)); X-Trans and other patterns, and the few photosites at the sensor border, use bilinear interpolation; half-size previews bin 2×2 cells.

- RCD runs on data scaled by the as-shot white balance and scaled back afterwards: it assumes small differences between channels, which raw camera values do not have (without it the photo's mean colour moved by about 5%).
- Its ratio estimates are not mean-preserving in fine texture (about 1% per channel in foliage): interpolated values are rescaled block by block (64 px, blended) to the mean of the real photosites of their colour.
- Output is clamped to the largest photosite in its 5×5 neighbourhood: next to blown highlights RCD would otherwise overshoot past the sensor's clip and those pixels would be treated as clipped.
- Measured by mosaicing the camera's JPEG and rebuilding it (`crates/raw/tests/demosaic_quality.rs`): bilinear 22.4 dB, Malvar–He–Cutler 25.0 dB, RCD 25.9 dB PSNR; channel means within 0.3% of the truth. Cost at 16 MP: about 170 ms more decoding (tiles of 256 px, one band at a time, about 40 MB extra memory).

## Highlight reconstruction

Before the demosaic, photosites at or above 0.98 of the white level are estimated from the colours that did not clip ([`crates/raw/src/highlights.rs`](../crates/raw/src/highlights.rs)), on data scaled by the as-shot white balance. The idea is darktable's "inpaint opposed"; two of its choices made bright skies magenta and are changed:

- A clipped colour is estimated from each **other colour whose 3×3 neighbourhood has no clipped sample**, times a ratio between the two colours; two estimates are combined on cube roots. darktable averages both other colours even when one of them clipped too, so where green and blue clip together the clipped blue drags green below red and blue.
- The ratios are **local**: summed per 16 px cell next to the clipped areas (over unclipped samples above 20% of the clip point), and spread into the clipped areas from coarser levels (push-pull, a cell's own ratio weighed against its surroundings' by 256 samples). darktable keeps one offset per colour for the whole image; an offset also falls short as a colour gets brighter, a ratio does not.
- With no usable colour around, a sample is made neutral at the brightest clipped sample near it; afterwards `Photo::to_working` makes pixels where every channel clipped neutral again with the chosen white balance. Left at their clip levels, a green next to a red that clips higher (after white balance) draws a magenta or yellow outline around blown areas.
- A sample only goes up (a clipped value is a lower bound). Values above 1.0 go through the raw shoulder of the tone stage: at default settings the result looks as before (blown areas are a cleaner white), and negative Exposure or Highlights now shows the reconstructed sky instead of flat grey.
- Not for HDR merges (they handle clipped shots themselves, and their floating point DNGs clip a pixel to white as before) nor for merge inputs.

Measured (`crates/raw/tests/highlights_quality.rs`), by overexposing the camera JPEG and clipping it as a sensor would: mean error on the clipped samples 0.24 EV instead of 0.39 at +1 EV, 0.33 instead of 0.55 at +2 EV (darktable's global offset: 0.26 and 0.42). What is left is mostly foliage texture, where the pixels that clipped are those greener than their neighbours. On the GX80 RW2's sky, 1–4% of the partly clipped pixels lean magenta, against 70% of the raw values. Cost: about 20–25 ms per decode at 16 MP.

## Lens corrections

Corrections stored in the file are applied at decoding, after the demosaic and before orientation, as Lightroom does for built-in lens profiles ([`crates/raw/src/lens.rs`](../crates/raw/src/lens.rs)); `DecodeOptions::no_lens_correction` leaves them out, and `RawInfo::lens_correction` lists what was applied.

- Everything is expressed with the two DNG opcodes for it, from the DNG SDK's definitions: **WarpRectilinear** (each output pixel read from `centre + R·(n·(k₀ + k₁r² + k₂r⁴ + k₃r⁶) + tangential)`, `R` the distance from the optical centre to the farthest corner, one set of coefficients per colour plane, which also corrects lateral chromatic aberration) and **FixVignetteRadial** (a gain `1 + k₀r² + … + k₄r¹⁰`). Resampling is Catmull-Rom.
- **DNG**: OpcodeList3, as written by Adobe's DNG Converter and by phones, with the active area as the image the opcodes refer to. Other opcodes (GainMap in OpcodeList2, for instance) are ignored.
- **Panasonic RW2** (and Leica files of the same kind): the distortion polynomial in the maker notes (tag 0x0119; layout as decoded by darktable), with radii in units of the half diagonal of the image: a source radius `rd` lands at `rd·(1 + s·(a·rd² + b·rd⁴ + c·rd⁶))`. It is inverted (Newton) and fitted with the WarpRectilinear cubic in r² by least squares: worst error 0.05 px at 16 MP.
- Checked against the camera, which applies the same correction to its JPEG (`crates/raw/tests/lens_framing.rs`): matching 108 patches of our develop to the GX80's JPEG and fitting a scale and shift, the residual is 0.40 px rms (1.3 px worst) at 1920 px, against 6.4 px rms (28 px worst) uncorrected, at scale 0.9995: the same framing. The versioned crop in `samples/ci` carries the correction as an OpcodeList3, moved onto the crop, so this runs in CI too.
- Cost: 60–90 ms per 16 MP decode, 20–30 ms at half size.
- Not for HDR merge inputs: the merged DNG does not carry the correction yet.

## Speed and memory

- **Stage cache** (`pipeline::stages`): `Pipeline::render_cached` keeps four stages (working image, tone, spatial filters, colour), each with the settings it was computed from, and redoes only the stages whose settings changed. The photo is recognised by its pixel buffer, size, a sample of its pixels and its colour data, so a new photo or a swapped camera profile is never served from the cache. A cached render is bit-identical to an uncached one (tested).
- **Transfer functions** in the per-pixel passes use tables instead of `powf` (`pipeline::gamma`, error below 2·10⁻⁵), and colour grading is tabulated on luma: the colour pass went from 55 to 26 ms at 2048 px.
- **Memory**: the demosaic writes only the default crop, the decoder reads the RAW samples in place instead of converting them all to f32, colour noise reduction works one channel at a time, and exports use `Pipeline::render_owned` and an owning `export::finish`, which reuse the photo's pixels instead of copying them. Peak memory for a 16 MP export went from 1.05 GB to 0.53 GB (decode alone from 634 to 368 MB). Scaling linearly, a 45 MP file needs about 1.5 GB; not measured, as there is no such sample yet. Processing in strips would lower it further and is not done.

## Masks

A mask is a group of components (linear gradient, radial gradient, brush) combined in order: add (maximum), subtract (× 1 − component), intersect (×). Each component can be inverted and has an opacity; the group has an Amount. The weights are computed at the resolution of the render (drafts while dragging, preview, export), so the same mask gives the same result at every size:

- **Linear**: smoothstep from the `Zero` point to the `Full` point, in pixel space.
- **Radial**: the ellipse in its bounding box, turned by `Angle`; full inside, a smooth falloff over the `Feather` part of the radius.
- **Brush**: strokes painted in order, each dab moving the weight towards 1 (or 0 for the eraser) by its flow, full strength inside the centre weight and fading to the radius.

Geometry is in fractions of the uncropped photo in its stored orientation: a `Frame` maps each developed pixel back through the crop, the straighten angle and the EXIF orientation, so masks stay on the picture when the crop changes. Masks Lumen cannot render (AI, range) are kept in the settings but not applied, with a note.

The local sliders are approximations in the spirit of the global ones, not Adobe's maths: negative local Sharpness (Adobe's blur) is not applied.

## Installed Adobe profiles

Presets name profiles without including them: "Adobe Color" is `Stubbed`, and a DNG only carries the hash of its LookTable. When Lightroom, Lightroom Classic or Camera Raw is installed, `pipeline::adobe` uses it:

- **Camera profile**: `CameraProfiles/Adobe Standard/<make> <model> Adobe Standard.dcp` (or the one the Look asks for in `crs:CameraProfile`). When no name matches, it looks among the same maker's DCPs for one with the same ColorMatrix: this is how the GX80 finds the "Panasonic DMC-GX85" profile. The DCP replaces the file's matrices and adds a HueSatMap, a LookTable and possibly a curve.
- **Look**: the `.xmp` with the same UUID under `Settings/…/Profiles/`, for instance `Adobe Raw/Adobe Color.xmp`, which holds the LookTable (36×16×16) and the profile curve.

Folders searched: those in `LUMEN_ADOBE_DIR`; on macOS the Lightroom and Lightroom Classic bundles and `/Library/Application Support/Adobe/CameraRaw`; on Windows `C:\ProgramData\Adobe\CameraRaw` and `%APPDATA%\Adobe\CameraRaw`. The files are Adobe's: they are read from the local install and never copied into the repository. `--no-adobe` turns them off.

## Missing or approximate

- **Without an Adobe install** the Adobe Color LookTable is not available: only its curve is applied, and the CLI says so.
- **Highlights/Shadows, Texture, Clarity, Dehaze** are global or simple. In Adobe they are local, edge-aware filters (a guided filter would get closer).
- **Luminance noise reduction** is not implemented (the CLI says so).
- **Lens correction**: only what the file carries (see [Lens corrections](#lens-corrections)); no lens database (lensfun), no correction of lateral chromatic aberration or vignetting for Panasonic RW2s (not in their metadata), no Optics panel in the app.
- **Input ICC profiles** (JPEG/TIFF): sRGB is assumed.

## `.cube` export

`lumen-cli cube preset.xmp -o out.cube` (or `xmp-dump preset.xmp --cube out.cube`) samples stages 2, 5–9 and 15–17 in "already rendered image" mode, with sRGB in and out. White balance, spatial operations and grain cannot be part of a LUT.

## Calibration against Lightroom

Slider strengths were fitted to a real Lightroom render: the GX80 camera JPEG (`samples/pixls/P1010041-camera.jpg`) with "Dreamy days in spring" applied by Lightroom 12 for iOS, by a coordinate search minimising the mean ΔE2000 at 480 px (where the random grain averages out):

| Constant | Before | Fitted |
|---|---|---|
| Contrast ±100 → curve exponent | ±0.6 | ±0.25 |
| Highlights ±100 → maximum shift | ±0.15 | ±0.02 |
| Shadows ±100 | ±0.15 | ±0.15 (unchanged) |
| HSL hue ±100 | ±30° | ±40° |
| HSL saturation ±100 → chroma | ±100% | ±13% |
| HSL luminance ±100 → luma | ±40% | ±80% |
| Colour grading saturation 100 → added chroma | 0.12 | 0.36 |

Result: mean ΔE from 7.1 to **2.8** (test `matches_lightroom_on_the_camera_jpeg`). Limits: one image with one preset, so some constants may compensate for each other (the very weak HSL saturation, for example, could hide a colour grading or vibrance that differs from Adobe's). A robust calibration needs Lightroom exports with **one slider at a time**, which can be made for free from JPEGs. The bench for that is `tools/calibrate`: a manifest of references, one per slider, measured at 480 px ([calibration.md](calibration.md), in Italian, lists the exports to make). Raw files still need checking: Adobe's Highlights/Shadows are local and behave differently on high dynamic range files.

## Checking

```bash
cargo run --release -p lumen-cli -- develop photo.dng --preset preset.xmp -o ours.jpg
cargo run --release -p lumen-cli -- preview photo.dng -o lightroom.jpg   # or an export made in Lightroom
cargo run --release -p compare -- ours.jpg lightroom.jpg --diff diff.png
```

`compare` prints the mean, median and 95th-percentile ΔE2000, the mean Lab of both images and a difference map (black = 0, white ≥ 10). The reference that matters is a render by Lightroom itself with the same preset, such as the preview inside a "mobile" DNG. For the raw.pixls.us RW2 the only reference is the camera JPEG, which has another look and framing: there only the averages mean something (base rendering with ACR3 and Adobe Standard: L* 39.2 against 40.9, ΔE of the means 3.6; the Panasonic JPEG is more saturated by design).
