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
| 4 | **Shoulder** (raw only) | linear | identity up to 0.7, then `tanh`: highlights above clipping stay recoverable |
| 4b | **LookTable** | linear ProPhoto | the Look's (e.g. Adobe Color), otherwise the DCP's |
| 5 | **Profile curve** | linear / Melissa* | raw only: the DCP `ProfileToneCurve` or, when missing (as in Adobe Standard, or without a DCP), the DNG SDK default **ACR3** curve ([`acr3.rs`](../crates/pipeline/src/acr3.rs)); applied in linear with the SDK's hue-preserving "RGB tone" method. Then the Look's curve (Adobe Color: a gentle S) |
| 6 | **Profile RGB table** | declared primaries and gamma | `crs:RGBTable` of creative profiles, with Amount |
| 7 | **Basic** | Melissa | Contrast (power curve around mid grey), Highlights/Shadows (bells centred at 0.75 and 0.25), Whites/Blacks (ends). **Global**, not local as in Adobe |
| 8 | **Curves** | Melissa | parametric (4 regions with splits) → master points → R/G/B, monotone cubic spline (Fritsch–Carlson) |
| 9 | **ShadowTint** | Melissa | green/magenta in the shadows |
| 10 | **Colour NR** | Melissa | chroma blur, radius ∝ Amount |
| 11 | **Texture / Clarity** | Melissa luma | difference from a blur (0.15% / 1.2% of the long edge), weighted towards the midtones |
| 12 | **Dehaze** | Melissa | global veil model |
| 13 | **Sharpening** | Melissa luma | unsharp mask: Radius, Detail (halo limit), Masking (edges only) |
| 14 | **Post-crop vignette** | Melissa | Amount, Midpoint, Roundness, Feather |
| 15 | **HSL** | encoded Rec.709 → (Y, a, b) | 8 overlapping bands whose weights sum to 1. Hue ±100 = ±40°, Saturation ±100 = ±13% chroma. Luminance leaves greys alone |
| 16 | **Vibrance / Saturation** | (Y, a, b) | vibrance acts more on dull colours and protects skin tones |
| 17 | **Colour grading** | (Y, a, b) | luminance bells for shadows/midtones/highlights (Blending sets their width, Balance their centre), plus the global wheel |
| 18 | **.cube LUT** (`--lut`) | encoded sRGB | last, with an amount |
| 19 | **Grain** | after resizing | two-octave value noise (Size, Frequency = Roughness), fixed seed |

\* *Melissa* = ProPhoto primaries with the sRGB transfer curve, the space of Lightroom's curves and RGB readouts.

## Installed Adobe profiles

Presets name profiles without including them: "Adobe Color" is `Stubbed`, and a DNG only carries the hash of its LookTable. When Lightroom, Lightroom Classic or Camera Raw is installed, `pipeline::adobe` uses it:

- **Camera profile**: `CameraProfiles/Adobe Standard/<make> <model> Adobe Standard.dcp` (or the one the Look asks for in `crs:CameraProfile`). When no name matches, it looks among the same maker's DCPs for one with the same ColorMatrix: this is how the GX80 finds the "Panasonic DMC-GX85" profile. The DCP replaces the file's matrices and adds a HueSatMap, a LookTable and possibly a curve.
- **Look**: the `.xmp` with the same UUID under `Settings/…/Profiles/`, for instance `Adobe Raw/Adobe Color.xmp`, which holds the LookTable (36×16×16) and the profile curve.

Folders searched: those in `LUMEN_ADOBE_DIR`; on macOS the Lightroom and Lightroom Classic bundles and `/Library/Application Support/Adobe/CameraRaw`; on Windows `C:\ProgramData\Adobe\CameraRaw` and `%APPDATA%\Adobe\CameraRaw`. The files are Adobe's: they are read from the local install and never copied into the repository. `--no-adobe` turns them off.

## Missing or approximate

- **Without an Adobe install** the Adobe Color LookTable is not available: only its curve is applied, and the CLI says so.
- **Highlights/Shadows, Texture, Clarity, Dehaze** are global or simple. In Adobe they are local, edge-aware filters (a guided filter would get closer).
- **Luminance noise reduction** is not implemented (the CLI says so).
- **Lens correction**: absent. The GX80's own JPEG, for instance, corrects distortion and so has a tighter framing.
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

Result: mean ΔE from 7.1 to **2.8** (test `matches_lightroom_on_the_camera_jpeg`). Limits: one image with one preset, so some constants may compensate for each other (the very weak HSL saturation, for example, could hide a colour grading or vibrance that differs from Adobe's). A robust calibration needs Lightroom exports with **one slider at a time**, which can be made for free from JPEGs. Raw files still need checking: Adobe's Highlights/Shadows are local and behave differently on high dynamic range files.

## Checking

```bash
cargo run --release -p lumen-cli -- develop photo.dng --preset preset.xmp -o ours.jpg
cargo run --release -p lumen-cli -- preview photo.dng -o lightroom.jpg   # or an export made in Lightroom
cargo run --release -p compare -- ours.jpg lightroom.jpg --diff diff.png
```

`compare` prints the mean, median and 95th-percentile ΔE2000, the mean Lab of both images and a difference map (black = 0, white ≥ 10). The reference that matters is a render by Lightroom itself with the same preset, such as the preview inside a "mobile" DNG. For the raw.pixls.us RW2 the only reference is the camera JPEG, which has another look and framing: there only the averages mean something (base rendering with ACR3 and Adobe Standard: L* 39.2 against 40.9, ΔE of the means 3.6; the Panasonic JPEG is more saturated by design).
