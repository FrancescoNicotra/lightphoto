# Desktop app

The [`crates/app`](../crates/app) crate, executable `lumen`, built with [egui](https://github.com/emilk/egui)/eframe 0.37 (wgpu backend).

```bash
cargo run --release -p app                                   # reopens the last folder
cargo run --release -p app -- samples/pixls                  # a folder
cargo run --release -p app -- samples/pixls/P1010041.RW2 --presets samples
```

## Language

The interface is in English, or in Italian when the operating system's language is Italian (read with `sys-locale`: the macOS preferred language, the Windows display language, `LANG` on Linux). `LUMEN_LANG=en` or `LUMEN_LANG=it` overrides it. Texts are written in [`i18n.rs`](../crates/app/src/i18n.rs) style, English next to Italian where they are used: `tr("Open folder…", "Apri cartella…")`.

## Interface

| Area | Content |
|---|---|
| Menu | **File**: Open folder (⌘O / Ctrl+O), Open file, Preset folder, Load LUT, Save settings (⌘S / Ctrl+S), Export (⇧⌘E / Ctrl+Shift+E). **Edit**: Undo, Redo, Copy settings (⇧⌘C / Ctrl+Shift+C), Paste settings (⇧⌘V / Ctrl+Shift+V), Select all (⌘A / Ctrl+A), Reset all. **View**: Fit, 100%, Before/After, single / split / side-by-side view. **Tools**: Crop & straighten (R), White balance picker (W), Auto tone |
| Left | presets of the chosen folder (`.xmp`, "mobile" `.dng`, `.cube`/`.3dl`), grouped by preset group or sub-folder, with search, each with a thumbnail of the open photo developed with it. Hovering a preset previews it; a click applies it to the open photo, or to every selected photo |
| Centre | the photo: Fit / 100% / 200%; click to switch between fit and 100% at the clicked point, drag to pan, pinch or ⌘/Ctrl + wheel to zoom around the cursor. The toolbar above it has the comparison views, the crop tool and the white balance picker |
| Right | RGB histogram with clipping marks and shooting data; preset **Amount**; panels Profile & LUT (amount of the profile and of the LUT), Basic (white balance presets and Temp/Tint, tone, presence), Tone curve (parametric, editable master points, R/G/B), HSL (8 bands × hue/saturation/luminance), Color grading (3-way and global wheels, Blending, Balance), Detail, Effects (vignette, grain), Calibration |
| Bottom | filmstrip of the open folder and status bar with decode/develop times, notes (Adobe profiles in use, what is approximated) and export progress |

Sliders: double-click the label or the track to reset; click the number to type a value, or drag on it. Double-clicking Temp./Tint goes back to the "As Shot" white balance. In the point curve: click on the curve to add a point, drag to move it, double-click to remove it.

Other shortcuts (outside text fields): ⌘Z / Ctrl+Z undo, ⇧⌘Z / Ctrl+Shift+Z (or Ctrl+Y) redo, `Z` fit/100%, `R` crop, `W` white balance picker, ← → previous/next photo, Enter applies the crop, Esc cancels the crop or the picker and closes the export dialog. Folders, photos, `.xmp` presets and LUTs can be dropped on the window.

## Before and after

- `\` (or **Before** in the toolbar) switches the photo to its state without edits.
- **Split**: the before on the left of a vertical divider, the after on the right; drag the divider (the round handle) to move it.
- **Side by side**: the two next to each other, each in half of the view.

All three work with zoom and pan, which the two halves share. The "before" is the default rendering of the file with the edit's crop, so both sides show the same frame.

## Crop and straighten

`R`, the **Crop** button or Tools → Crop & straighten open the tool. The photo is shown whole and straightened, with the frame on top:

- drag the handles to resize, inside the frame to move it, **outside the frame to rotate** the photo (or use the Angle slider, −45..45°);
- **Aspect**: free, the photo's original, 1:1, 4:5, 3:2, 16:9, landscape or portrait; corner handles keep the ratio;
- the frame always stays inside the picture: rotating shrinks it as needed, so no empty corners end up in the photo;
- **Done** (Enter) applies the crop as one undo step, **Cancel** (Esc) restores the previous one, **Reset** goes back to the whole photo.

The crop is stored with Lightroom's keys (`crs:HasCrop`, `CropTop/Left/Bottom/Right`, `CropAngle`) and read from sidecars and presets that have them. It is applied before every spatial stage of the [pipeline](pipeline.md), so the post-crop vignette follows the new frame; the export size and its `{size}` refer to the cropped photo. With an angle, Lightroom may place the rectangle slightly differently: the format does not say exactly how the angle and the rectangle combine.

## White balance picker and Auto

- **Picker** (`W`, or the button in the Basic panel): click a point that should be neutral grey or white. The worker averages a small area around it in the photo's own data: for raw files it finds the white point of the camera data through the camera profile (`ColorProfile::neutral_to_xy`) and sets Temperature/Tint; for JPEG/PNG/TIFF it sets the relative Temperature/Tint that bring that colour to neutral. Clipped points are ignored. One undo step.
- **Auto** (Basic panel or Tools): the photo is developed small with the tone sliders at zero and its lightness sets Exposure, Contrast, Highlights, Shadows, Whites and Blacks with a simple heuristic ([`auto.rs`](../crates/app/src/auto.rs)): exposure brings the median to 0.45; highlights pull down when more than 3% of the picture is above 0.9, shadows lift when more than 15% is below 0.15; whites and blacks stretch the ends to 0.97/0.03 (whites also pull back clipping); contrast aims at a 10–90% spread of 0.55. It is not Adobe's Auto. One undo step.

## Preset thumbnails

Each preset in the list shows the open photo developed with it, starting from the photo's default settings (so the thumbnail shows what the preset does, not the current edit). They are rendered at 160 px from a 320 px copy, by the same worker but only when nothing else is waiting, one at a time, so they never delay the main preview; a new photo or preset folder replaces the queue. They are kept per photo and preset while the folder is open.

## Filmstrip

Each photo shows its name, its file type (RAW types in blue), an **edited** badge when it carries settings (from this session or from its sidecar) and an **LR** / **PS** badge on JPEG/PNG/TIFF files whose XMP says they were written by Lightroom or Photoshop: they are edits or exports already, so a preset applied on top of them is applied twice. The tooltip names the program.

Selection works as in Lightroom: a click selects one photo, ⌘/Ctrl-click adds or removes one, ⇧-click selects a range, ⌘A / Ctrl+A selects all. The photo with the blue outline is the one open in the editor.

## Edits, presets, undo

- Applying a preset changes only the keys it contains, as in Lightroom: two presets in a row add up.
- **Amount** (0–200%) appears at the top of the panels after a preset is applied. Every numeric setting moves between the settings before the preset and those with it ([`blend.rs`](../crates/app/src/blend.rs)): sliders, curve points (blended as curves), HSL, colour grading (hues along the shorter arc), grain, white balance (in mired, starting from the camera's when the preset sets an absolute one). The profile follows through its own amount; RGB tables cannot be scaled and come with the preset as soon as the amount is above zero. Values are clamped to the slider ranges. Changing another setting ends the Amount control, as does reopening the photo: the sidecar stores the resulting values.
- **Copy / paste settings** copies everything the open photo has (develop settings, profile, LUT) and pastes it on the open photo or on every selected one.
- Undo/redo by snapshots: a gesture (a drag, a typed value, a preset, a change of Amount, a paste, a crop, a picked white balance, Auto) is one step. Each photo keeps its history while the app is open.

## Saving

Edits are saved automatically, one second after the last change, when switching photo and when quitting; ⌘S / Ctrl+S saves at once. Photos that were never edited get no file.

- RAW files: `name.xmp` next to the photo, Lightroom's sidecar, with the `crs:` settings, the profile and tables copied from the preset.
- JPEG/PNG/TIFF: `name.jpg.xmp` (the extension kept, so a RAW and a JPEG with the same name do not share a file). Lightroom stores their settings inside the image, which Lumen never modifies; it ignores these sidecars.
- Existing sidecars are updated in place, keeping other namespaces and unknown keys. The LUT path and amount go in Lumen's own namespace ([xmp-format.md](xmp-format.md#7-sidecars-written-by-lumen)).

Photos edited in a batch (preset or paste on a selection) are saved immediately.

## Rendering

- A **worker thread** decodes the photo at half resolution (`DecodeOptions { half_size: true }`), reduces it to 2048 px and develops it. Requests go through a one-slot mailbox: a request that has not started is replaced by the next one, so only the latest slider value is rendered.
- While a slider is dragged a **1024 px draft** is developed; the 2048 px preview follows on release. Sharpening and noise reduction radii are scaled from the full size (`RenderOptions::full_size`), so the preview looks like the export.
- When the view shows more pixels than the preview (100%, 200%) the worker decodes the whole photo and develops it at full resolution after the preview.
- Installed Adobe profiles (see [pipeline.md](pipeline.md#installed-adobe-profiles)) are prepared once per profile and cached.
- Timings on the GX80 RW2 (Apple Silicon, release), measured while the machine was busy with other work and therefore pessimistic: half-size decode 60–120 ms; 2048 px preview 130–300 ms without a preset and 200–400 ms with "Dreamy days in spring" (Texture/Clarity/Dehaze, sharpening and grain are the costly spatial stages); 1024 px draft about 60 ms in an isolated pipeline benchmark. The first render with a new Adobe profile reads the installation's files and can take a second or more; it is cached afterwards.

## Export

Dialog with size (original, long edge, percentage, box), format JPEG (quality) / 16-bit PNG / 16-bit TIFF, output sharpening, file name template (`{name}`, `{preset}`, `{size}`) and folder (the photo's own when none is chosen). With several photos selected it exports all of them. A background thread decodes and develops each photo at full resolution with [`export`](../crates/export) (Lanczos3, sRGB ICC profile, EXIF); the status bar shows the progress. The dialog's settings, the photo folder and the preset folder are remembered.

## Scripted runs

To check the interface without clicking (and to remake the documentation's screenshots), `LUMEN_SCRIPT` runs a sequence of steps separated by `;` or new lines; the list is in [`script.rs`](../crates/app/src/script.rs). Example:

```bash
LUMEN_SCRIPT="wait; preset Dreamy; wait; amount 50; wait; shot amount.png; select all; copy; paste; exportdir /tmp/out; export; wait; quit" \
  cargo run --release -p app -- samples/pixls --presets samples
```

With a script running the app prints the time of each render and each exported file on stderr (not visible in Windows release builds, which have no console).

## Limits

- No catalogue: the app works on folders; edits live in the sidecars.
- RAW thumbnails come from the preview embedded in the file: they ignore the EXIF orientation and do not show the edits.
- No lens correction, masks, Optics/Geometry panels.
