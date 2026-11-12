# Camera Raw / Lightroom XMP format: reverse-engineering notes

These notes come from studying `samples/Dreamy days in spring.xmp` and its `... - mobile version.dng`. The code that implements them is in the [`xmp`](../crates/xmp) crate.

## 1. Packet structure

```
x:xmpmeta
 └ rdf:RDF
    └ rdf:Description  (one or more; they all describe the same resource)
        attributes  crs:Exposure2012="+0.50" ...      ← simple values
        elements    <crs:ToneCurvePV2012><rdf:Seq>…   ← arrays, lang-alt, structs
```

- Settings namespace: `crs` = `http://ns.adobe.com/camera-raw-settings/1.0/`.
- A simple value can be written as an **attribute** or as a **child element**. The parser accepts both.
- Numbers are text, often with an explicit sign: `"+50"`, `"+1.0"`, `"0.00"`.
- There are four kinds of complex value:
  - `rdf:Seq`: curves, `PointColors`.
  - `rdf:Alt`: `Name`, `Group`, `Description`, with `xml:lang="x-default"`.
  - Struct: `crs:Look` contains a nested `rdf:Description`.
  - Short struct: `<crs:LensBlur crs:Version="1" …/>`, attributes on an empty element.

## 2. Presets are partial

A preset only contains the keys it wants to set. It is applied as `current settings` + the keys present in the preset, which is what `DevelopSettings::apply` does.

## 3. Key map (PV2012, ProcessVersion 11+/15.4)

| Panel | XMP keys | Range / notes |
|---|---|---|
| WB | `WhiteBalance` (`As Shot`/`Auto`/`Custom`/name), `Temperature` (K), `Tint` | absolute, **raw only** |
| WB non-raw | `IncrementalTemperature`, `IncrementalTint` | −100..100 |
| Basic | `Exposure2012` (EV ±5), `Contrast2012`, `Highlights2012`, `Shadows2012`, `Whites2012`, `Blacks2012` | −100..100 |
| Presence | `Texture`, `Clarity2012`, `Dehaze`, `Vibrance`, `Saturation` | −100..100 |
| Parametric curve | `ParametricShadows/Darks/Lights/Highlights`, `Parametric{Shadow,Midtone,Highlight}Split` | splits 25/50/75 by default |
| Point curve | `ToneCurvePV2012`, `…Red`, `…Green`, `…Blue`, `ToneCurveName2012` | `rdf:Seq` of `"x, y"` in 0..255 (gamma space) |
| HSL | `{Hue,Saturation,Luminance}Adjustment{Red,Orange,Yellow,Green,Aqua,Blue,Purple,Magenta}` | −100..100 |
| Colour grading | shadows/highlights: `SplitToning{Shadow,Highlight}{Hue,Saturation}`, `SplitToningBalance`; midtones: `ColorGradeMidtone{Hue,Sat,Lum}`; `ColorGrade{Shadow,Highlight}Lum`, `ColorGradeGlobal{Hue,Sat,Lum}`, `ColorGradeBlending` | hue 0..359, sat 0..100 |
| Calibration | `ShadowTint`, `{Red,Green,Blue}{Hue,Saturation}` | −100..100 |
| Detail | `Sharpness`, `SharpenRadius`, `SharpenDetail`, `SharpenEdgeMasking`, `LuminanceSmoothing`, `ColorNoiseReduction*` | |
| Effects | `GrainAmount`, `GrainSize`, `GrainFrequency` (= Roughness), `GrainSeed`, `PostCropVignette*` | |
| Profile | `crs:Look` (struct), `CameraProfile`, `CameraProfileDigest` | see §4 |
| Crop | `HasCrop` (`True`/`False`), `CropTop`, `CropLeft`, `CropBottom`, `CropRight`, `CropAngle` | fractions 0..1 in the file's stored orientation (before the EXIF rotation); angle in degrees ±45 |

Colour grading (PV5) **reuses the split toning keys** for the hue and saturation of shadows and highlights. The `ColorGrade*` keys only exist for midtones, luminances, global and blending.

## 4. Profiles, Looks and LUTs

### In the preset `.xmp`
```xml
<crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1"
     crs:UUID="B952C231111CD8E0ECCF14B86BAA7077" crs:Stubbed="true"> …
```
`Stubbed="true"` means the profile is only named (name and UUID), because it is built into Camera Raw.

### In the "mobile version" `.dng` the same Look is complete
```xml
<crs:Parameters><rdf:Description crs:CameraProfile="Adobe Standard"
     crs:LookTable="E1095149FDB39D7A057BAB208837E2E1">
  <crs:ToneCurvePV2012> (0,0) (22,16) (40,35) (127,127) (224,230) (240,246) (255,255)
```
So **"Adobe Color" = the "Adobe Standard" camera profile + an Adobe-internal HSV table (`LookTable`, hash only) + a gentle S-curve**. The curve can be applied exactly. The LookTable is not in the file: it comes from Camera Raw's own files when installed.

### Creative profiles with an embedded LUT (made in Photoshop from a .cube)
```xml
crs:RGBTable="<md5>"   crs:Table_<md5>="<blob>"
```
They sit inside `crs:Look/crs:Parameters` in presets, or at the top level in presets of type `PresetType="Look"`.

Decoding the blob, implemented in `crates/xmp/src/rgb_table.rs` and checked against the DNG SDK layout (`dng_big_table`):

1. **Adobe base85**. Alphabet ``0-9 a-z A-Z . - : + = ^ ! / * ? ` ' | ( ) [ ] { } @ % $ #``. 5 characters make a little-endian `u32`, the first character least significant. A final group of n characters gives n−1 bytes.
2. A `u32 LE` with the uncompressed length, followed by a **zlib** stream.
3. Binary content, all little-endian:
   - `u32 type` (1 = RGB table), `u32 version` (1);
   - `u32 dimensions` (1 or 3), `u32 divisions` (e.g. 32);
   - `u16 ×3` samples as **deltas from the identity ramp** `nop[i] = (i·0xFFFF + d/2) / (d−1)`, with arithmetic modulo 2¹⁶. 3D order: r outermost, g, b innermost;
   - `u32 primaries` (0 sRGB, 1 AdobeRGB, 2 ProPhoto, 3 P3, 4 Rec2020), `u32 gamma` (0 linear, 1 sRGB, 2 1.8, 3 2.2, 4 Rec2020), `u32 gamut` (0 clip, 1 extend);
   - `f64 min amount`, `f64 max amount`, and an optional `u32 flags`.

The numeric codes for primaries and gamma still need confirming on a real profile exported by Camera Raw. `xmp-dump --cube` extracts the table to a `.cube`.

### LookTable of the Adobe Raw profiles
`Adobe Color.xmp` (in the Lightroom install) has `crs:LookTable="E1095149…"` and next to it `crs:Table_E1095149…="<blob>"`, with the same base85 + zlib wrapping as RGB tables. Content, little-endian, worked out from the sizes (they match to the byte):

- `u32 0`, `u32 1` (version), `u32` hue (36), saturation (16) and value (16) divisions;
- three `f32` per entry: hue shift in degrees, saturation scale, value scale. Order as the DNG `ProfileLookTableData`: value outermost, then hue, saturation innermost;
- a final `u32` value encoding (0 linear, 1 sRGB).

Implemented in `crates/xmp/src/look_table.rs` and applied with `raw::hue_sat::HueSatMap`.

## 5. Why an XMP preset does not become a LUT

| Operation | Point-wise (LUT ok) | Notes |
|---|---|---|
| Curves, HSL, colour grading, calibration, vibrance/saturation, profile | yes | |
| Contrast / Whites / Blacks | yes (approximation) | |
| Highlights / Shadows | partly | **local** in Adobe |
| Absolute WB `Temperature` | no | needs the raw data and the camera matrix |
| Texture, Clarity, Dehaze, Sharpening, NR, Grain, Vignette | no | spatial operations |

That is why presets are applied in the raw pipeline. The `.cube` export of a parametric preset only captures the point-wise part.

## 6. Keys seen but not mapped yet

`HDREditMode`, `CurveRefineSaturation`, `OverrideLookVignette`, `PointColors` (point colour, `-1` = empty), `LensBlur`, `AsShotTemperature/Tint`, `GrainSeed`, lens and perspective keys (`Perspective*`, `Defringe*`, `LensProfileEnable`), `CameraProfileDigest`. `xmp-dump` lists them under *Not mapped yet*.

## 7. Sidecars written by Lumen

- RAW files: `name.xmp` next to the photo, as Lightroom does. JPEG/PNG/TIFF: `name.jpg.xmp` (Lightroom writes their settings inside the file, which Lumen never modifies).
- Existing sidecars are updated in place: other namespaces and unknown keys are kept.
- The LUT applied in the app is stored in Lumen's own namespace, `https://github.com/FrancescoNicotra/lightphoto/ns/1.0/` (`lumen:LUT` with the path, `lumen:LUTAmount`); other programs ignore it.

## 8. Local corrections (masks)

Implemented in `crates/xmp/src/masks.rs`. Containers, all read and written:

| Key | Written by | Lumen |
|---|---|---|
| `crs:MaskGroupBasedCorrections` | Lightroom 11+ / Camera Raw 14+ | read; new masks are written here |
| `crs:GradientBasedCorrections` | older versions (linear gradient) | read, written back to the same key |
| `crs:CircularGradientBasedCorrections` | older versions (radial filter) | read, written back to the same key |
| `crs:PaintBasedCorrections` | older versions (brush) | read, written back to the same key |

Each container is an `rdf:Seq` of corrections:

```xml
<crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description
    crs:What="Correction" crs:CorrectionName="Mask 1" crs:CorrectionActive="true"
    crs:CorrectionAmount="1" crs:LocalExposure2012="-0.375" crs:LocalTemperature="-0.3" …>
  <crs:CorrectionMasks><rdf:Seq>
    <rdf:li><rdf:Description crs:What="Mask/Gradient" crs:MaskValue="1" crs:MaskBlendMode="0"
        crs:MaskInverted="false" crs:ZeroX="0.5" crs:ZeroY="0.55" crs:FullX="0.5" crs:FullY="0.25"/></rdf:li>
  </rdf:Seq></crs:CorrectionMasks>
</rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections>
```

What is **verified**, and against what:

- **Field names**, from ExifTool's Camera Raw tables (Correction and CorrectionMask structs): `What`, `CorrectionName`, `CorrectionActive`, `CorrectionAmount`, `CorrectionMasks`, `CorrectionSyncID`, `Local*` (`LocalExposure2012`, `LocalContrast2012`, `LocalHighlights2012`, `LocalShadows2012`, `LocalWhites2012`, `LocalBlacks2012`, `LocalTemperature`, `LocalTint`, `LocalSaturation`, `LocalTexture`, `LocalClarity2012`, `LocalDehaze`, `LocalSharpness`…); for components `What`, `MaskValue`, `MaskBlendMode`, `MaskInverted`, `ZeroX/ZeroY/FullX/FullY`, `Top/Left/Bottom/Right`, `Angle`, `Midpoint`, `Roundness`, `Feather`, `Flipped`, `Radius`, `Flow`, `CenterWeight`, `Dabs`, `Masks`.
- **Component kinds** `Mask/Gradient`, `Mask/CircularGradient`, `Mask/Paint`, and a radial gradient block from a Camera Raw 11.4 file (`Top/Left/Bottom/Right/Angle/Midpoint/Roundness/Feather/Flipped/Version`).

What comes from **other projects' observations of Lightroom** (not from Adobe, not checked against a file of ours):

- Local values are stored scaled: **`LocalExposure2012` = EV / 4**, the other sliders −1..1 (UI value / 100).
- **`MaskBlendMode`**: 0 = add, 1 = subtract. Lumen writes 2 for intersect, which Lightroom may not accept (it is reported to coerce 2 to 0 and to represent intersections differently).
- **Brush**: in `MaskGroupBasedCorrections` a brush is a `Mask/Aggregate` whose `Masks` holds the `Mask/Paint` strokes; the older layout puts the `Mask/Paint` strokes straight in `CorrectionMasks`. Each stroke has `Radius`, `Flow`, `CenterWeight` and `Dabs`, a sequence of `"d x y"` strings in fractions of the image; erasing strokes have `MaskValue` 0. A leading `"f 0.3"` item can carry the flow. Newer Lightroom versions may keep part of the brush data in a binary `.acr` file instead.
- `Flipped` is the opposite of `MaskInverted` on radial gradients (the older radial filter acted outside the ellipse unless flipped).

**Lumen's assumptions** (to check against Lightroom files):

- Coordinates are fractions of the **uncropped** image in the file's **stored orientation**, like the crop; a mask stays on the same part of the picture when the crop changes.
- Brush `Radius` is a fraction of the image's long edge; `CenterWeight` is the fraction of the radius at full strength (1 − feather).
- A linear gradient goes from no effect at `Zero` to full effect at `Full` (smoothstep); a radial gradient is the ellipse inscribed in `Top/Left/Bottom/Right`, turned by `Angle` (clockwise), full inside, with `Feather` the soft part of the radius; `Midpoint` and `Roundness` are kept but not used.
- Components combine in order: add = maximum, subtract = multiply by (1 − component), intersect = multiply.

**Kept as they are**: unknown fields of corrections and components (sync ids, versions…) and components Lumen cannot render (`Mask/Image` AI masks, range masks, depth): they are written back unchanged. A correction holding one of those is not applied, and the app marks it.
