# Linear captures and floating point DNGs

The foundations of HDR merging (and later focus stacking and panoramas), in the [`raw`](../crates/raw) crate:

- [`capture.rs`](../crates/raw/src/capture.rs): a RAW file as linear camera RGB **in sensor orientation**, with a mask of clipped pixels;
- [`dng_write.rs`](../crates/raw/src/dng_write.rs): linear camera RGB written as a **floating point `LinearRaw` DNG**, the format Lightroom uses for its HDR merges.

## `Capture`

`raw::load_capture(path, &DecodeOptions)` decodes like `Photo::load` and stops before orientation:

| Field | Content |
|---|---|
| `data` | camera RGB, 0..1 with 1 = clip point of each channel (black subtracted, so a few values are slightly negative), demosaiced, cropped to the default crop, **not** white balanced, **not** oriented |
| `clipped` | one byte per pixel: 1 if a photosite at or above `CLIP_MASK` (0.98) feeds the pixel through the demosaic |
| `info` | as for a `Photo`, but `info.orientation` is not applied to `data` |
| `profile`, `as_shot_neutral` | the file's colour matrices and as-shot white |
| `camera` | Make/Model as written in the file and the DNG UniqueCameraModel (or "make model"), to copy into a derived DNG |
| `metadata` | rawler's EXIF (exposure, lens, GPS, dates), to copy into a derived DNG |

`Photo::load` is `Capture` plus orientation (`Capture::into_photo`); it skips the mask, so the app pays nothing for it.

**Clipping mask.** It is computed on the mosaic, before the demosaic mixes neighbours: a pixel is clipped if any photosite the bilinear demosaic reads for it is. That is its 3×3 neighbourhood for Bayer and X-Trans alike (every 3×3 window of X-Trans holds all three colours; `demosaic::support_radius` gives 2, a 5×5 window, only for patterns that lack a colour in some 3×3 window), and the 2×2 cell at half size. For LinearRaw files the mask is per pixel: any channel ≥ 0.98. The threshold sits below the 0.995 at which `Photo::to_working` clips highlights to white: merging should stop trusting a frame a little before the hard clip.

## Writing: `write_linear_dng`

```rust
let capture = raw::load_capture(path, &DecodeOptions::default())?;
let dng = LinearDng { xmp: Some(packet), preview: Some(&preview), ..LinearDng::from_capture(&capture) };
raw::write_linear_dng(out, &dng, DngEncoding::HalfDeflate)?;
```

rawler's `DngWriter` lays out the file and writes IFD0, EXIF, XMP and the JPEG preview; it can only store floats as uncompressed 32 bit strips, so the raw tiles are encoded by Lumen and handed to its TIFF writer (`DngWriter::dng.write_data`).

```text
IFD0        thumbnail (RGB, ≤ 240×120) and the tags below, EXIF IFD, XMP (tag 700)
├ SubIFD 0  LinearRaw, 3 × 16 bit half float, 256×256 tiles, Deflate + predictor 34894
└ SubIFD 1  JPEG preview, ≤ 1024 px
```

| Where | Tags |
|---|---|
| IFD0 | `DNGVersion` 1.6, `DNGBackwardVersion` 1.4 (the first with floating point data), Make, Model, `UniqueCameraModel`, Software, Orientation, `CalibrationIlluminant1/2`, `ColorMatrix1/2`, `ForwardMatrix1/2` (when both exist), `CameraCalibration1/2` (when not identity), `AnalogBalance` (when not identity), `AsShotNeutral`, `BaselineExposure`, `BaselineNoise`, `BaselineSharpness` 1, `LinearResponseLimit` 1, XMP, EXIF |
| SubIFD 0 | `NewSubFileType` 0, `PhotometricInterpretation` 34892, `SamplesPerPixel` 3, `BitsPerSample` 16 ×3, `SampleFormat` 3 ×3, `Compression` 8, `Predictor` 34894, `TileWidth/Length` 256, `TileOffsets/ByteCounts`, `PlanarConfiguration` 1, `BlackLevel` 0 (repeat 1×1), `WhiteLevel` 1, `ActiveArea` and `DefaultCrop*` = the whole image, `DefaultScale` 1, `BestQualityScale` 1 |
| SubIFD 1 | `NewSubFileType` 1, JPEG, sRGB |

Details:

- **Values**: 1.0 is white, the clip point of the reference exposure. Values above 1 are kept (that is the point of an HDR merge); negative values and NaN are written as 0, as DNG raw data cannot be negative. On a real RAW this changes the 0.03% of pixels that read below black, the dark noise.
- **Half floats**: 10 bits of mantissa, a relative error ≤ 2⁻¹¹ ≈ 4.9·10⁻⁴ over 30 stops; rounding is to nearest, ties to even (`f32_to_f16`, no extra dependency).
- **Tile encoding** (predictor 34894, "floating point ×2"): for each row of the tile, samples in pixel order (RGBRGB…) are converted to half floats, their bytes split into a plane of high bytes followed by a plane of low bytes, then each byte is replaced by its difference with the byte 2 × 3 = 6 positions earlier. Tiles at the right and bottom edges are padded by repeating the last column and row. Each tile is compressed with zlib at level 3 (on a 16 MP RAW: 2% larger than level 6, five times faster) and encoded in parallel.
- **Colour illuminants**: `ColorProfile` keeps temperatures, so they are written back as the EXIF LightSource code with that temperature (2850 K → Standard A, 6500 K → D65…).
- **`CameraCalibration`** is copied when not identity so the DNG renders in Lumen as its source did. Without a `CameraCalibrationSignature`, Adobe applies it only with profiles that have no `ProfileCalibrationSignature` either.
- **Previews**: in sensor orientation like the raw data (the DNG convention: `Orientation` applies to every image in the file). The caller passes a rendering; a larger one is resized with Lanczos, so rawler's nearest-neighbour resize never runs. Without one, the writer makes a plain render (white balance, matrix, `BaselineExposure`, no tone curve).
- **Safe writes**: the file is written as `name.dng.tmp` next to the target and renamed at the end; on error the temporary file is removed.
- `DngEncoding::F32Uncompressed` writes 32 bit floats in plain strips (12 bytes/pixel) to tell encoder problems from others.

Size on the GX80 sample (16 MP): 63 MB, 4.0 bytes/pixel, written in about 0.25 s (release build, 10 cores); 190 MB uncompressed.

## Reading floating point DNGs

rawler already decodes them (strips or Deflate tiles with predictor 3, 34894 or 34895; 16, 24 or 32 bit). Lumen forces the white level to 1.0 for floating point data, whatever `WhiteLevel` says: without the tag rawler assumes 2^bits − 1 and the photo would be black. `RawInfo::float` tells the app that the photo is such a DNG.

## Checking

- `cargo test -p raw --test dng_write`: round trip through rawler's decoder for both encodings (relative error ≤ 10⁻³ for half floats, exact for f32; matrices, neutral, BaselineExposure, orientation, EXIF, XMP and preview preserved); the clipping mask; a synthetic Bayer DNG rewritten as a linear DNG develops like the original (ΔE2000 < 0.5 at every pixel); floating point DNGs without `WhiteLevel` or with an integer one.
- `lumen-cli dng photo.RW2 -o photo-linear.dng` rewrites a RAW as the DNG an HDR merge of that single frame would produce, with a preview rendered at default settings. Developed with `lumen-cli develop`, the GX80 sample matches the RW2 with a mean ΔE2000 of 0.01 (p95 0.01); the few larger differences are the negative values written as 0.
- Lightroom/Camera Raw compatibility is checked by opening such a DNG there.

## Not done yet

- The app reads develop settings only from the `.xmp` sidecar, not from the XMP embedded in a DNG (`app/src/state.rs`): needed so the initial settings of a merge show up in Lumen as they do in Lightroom.
- The app does not apply `Orientation` to the embedded preview of RAW files (filmstrip thumbnails): DNG previews of portrait shots appear sideways until it does.
- For DNG files rawler does not resolve the lens, so `PhotoInfo::lens` is empty (EXIF `LensModel` is still copied).
