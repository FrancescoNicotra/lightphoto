# Lumen

Lumen is a desktop RAW photo editor written in Rust, in the style of Adobe Lightroom. Its main purpose is to apply **Lightroom / Camera Raw presets** to your photos without Lightroom: `.xmp` presets, the `.dng` "mobile" presets shared by Lightroom Mobile users, and `.cube` / `.3dl` LUTs.

It opens RAW files from most cameras (DNG, CR2/CR3, NEF, ARW, RW2, RAF, ORF…) as well as JPEG, PNG and TIFF, develops them with the settings of the preset, lets you adjust every slider by hand, and exports JPEG, PNG or TIFF files.

## What the app does

- **Presets on the left**: pick a folder of presets; hovering one previews it on the photo, a click applies it. An **Amount** slider (0–200%) then sets the strength of the preset, as in Lightroom.
- **The photo in the centre**: fit or 100%/200% zoom, pan, zoom around the cursor, before/after with `\`, a split view with a draggable divider or the two side by side, RGB histogram with clipping marks.
- **Crop and straighten** (`R`): handles, aspect ratios (free, original, 1:1, 4:5, 3:2, 16:9, landscape or portrait), angle −45..45°; stored with Lightroom's crop keys.
- **White balance picker** (`W`) and **Auto** tone, and a thumbnail of the photo with each preset in the preset list.
- **Develop panels on the right**: Profile & LUT, Basic (white balance, tone, presence), Tone curve (parametric and point, R/G/B), HSL, Color grading wheels, Detail (sharpening, noise reduction), Effects (vignette, grain), Calibration.
- **Filmstrip at the bottom**: the photos of the open folder, with their file type, an *edited* badge and a mark on JPEGs that were already produced by Lightroom or Photoshop.
- **Several photos at once**: select with ⌘/Ctrl-click, ⇧-click or ⌘A / Ctrl+A, then apply a preset, paste copied settings or export them all together.
- **Nothing gets lost**: edits are saved automatically in an `.xmp` sidecar next to each photo, readable by Lightroom for RAW files. Original files are never modified.
- **Export** in the background: original size, long edge, percentage or box; JPEG with quality, 16-bit PNG or TIFF; output sharpening; file name templates; sRGB colour profile and EXIF data.
- **Undo / redo** of every change, and Lightroom's own colour profiles (Adobe Standard, Adobe Color…) when Lightroom or Camera Raw is installed on the computer.
- The interface is in **English or Italian**, following the language of the operating system.

How the app works and all its shortcuts: [docs/app.md](docs/app.md).

## Running the app

The app is built from source with Rust (stable, 2021 edition) through `rustup`.

### macOS

1. Install the Xcode command line tools, if they are not there yet:
   ```bash
   xcode-select --install
   ```
2. Install Rust:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
3. From the project folder, build and start the app:
   ```bash
   cargo run --release -p app
   ```

The first build takes a few minutes; later starts are immediate. The executable is `target/release/lumen`.

### Windows

1. Install the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with the **Desktop development with C++** workload (it provides the MSVC linker and the Windows SDK).
2. Install Rust with [rustup-init.exe](https://rustup.rs), keeping the default `x86_64-pc-windows-msvc` toolchain.
3. From the project folder, in PowerShell or the Command Prompt:
   ```bash
   cargo run --release -p app
   ```

The executable is `target\release\lumen.exe`. If the project sits in a very long path and the linker fails with `LNK1104`, create a `.cargo/config.toml` (ignored by git) with a short build folder:

```toml
[build]
target-dir = "C:/temp/lightphoto-target"
```

### Options

```bash
cargo run --release -p app -- path/to/photos                     # open a folder
cargo run --release -p app -- photo.RW2 --presets path/to/presets  # open a photo and a preset folder
```

Without arguments the app reopens the last folder. `LUMEN_LANG=en` or `LUMEN_LANG=it` overrides the interface language.

## Using it

1. **File → Open folder** (⌘O / Ctrl+O) and choose a folder of photos.
2. Click **Folder…** above the preset list and choose the folder with your `.xmp`, `.dng` or `.cube` presets.
3. Click a photo in the filmstrip, then a preset. Adjust **Amount** and the sliders: double-click a slider to reset it, click its number to type a value.
4. **File → Export** (⇧⌘E / Ctrl+Shift+E) writes the photo, or every selected photo, to the chosen folder.

## Command-line tools

The same engine is available from the terminal:

```bash
cargo run --release -p lumen-cli -- develop photo.RW2 -o out.jpg --preset "samples/Dreamy days in spring.xmp" --size 2048
cargo run --release -p lumen-cli -- develop *.RW2 -o export/ --preset preset.xmp --size 1080 --name "{name}-{preset}"
cargo run --release -p lumen-cli -- develop photo.dng -o out.tif --lut film.cube --lut-amount 0.7
cargo run --release -p lumen-cli -- info    photo.dng
cargo run --release -p lumen-cli -- preview photo.dng -o embedded.jpg     # the preview stored in the RAW file
cargo run --release -p lumen-cli -- cube    preset.xmp -o preset.cube     # the point-wise part of a preset as a LUT
cargo run --release -p lumen-cli -- dng     photo.RW2 -o photo-linear.dng # a RAW as a floating point linear DNG
cargo run --release -p lumen-cli -- hdr     a.RW2 b.RW2 c.RW2 --timings  # HDR merge into a-HDR.dng
cargo run --release -p xmp-dump  -- preset.xmp                           # what a preset contains
cargo run --release -p compare   -- ours.jpg lightroom.jpg --diff diff.png  # ΔE2000 against a reference
```

`cargo test` runs the tests. Some use the CC0 sample raw file from https://raw.pixls.us/data/Panasonic/DMC-GX80/P1010041.RW2, saved in `samples/pixls/`; they are skipped when it is missing.

## Project layout

| Folder | Content |
|---|---|
| `crates/app` | the desktop app (egui / wgpu) |
| `crates/pipeline` | the develop pipeline that applies Camera Raw settings ([docs/pipeline.md](docs/pipeline.md)) |
| `crates/raw` | RAW decoding (through `rawler`), demosaicing, DNG colour model, floating point DNG writer ([docs/linear-dng.md](docs/linear-dng.md)) |
| `crates/xmp` | reading and writing Camera Raw XMP settings, presets and embedded tables ([docs/xmp-format.md](docs/xmp-format.md)) |
| `crates/lut` | `.cube` / `.3dl` LUTs |
| `crates/export` | resizing and JPEG/PNG/TIFF encoding with colour profile and EXIF |
| `crates/align` | image alignment (pyramid + ECC, translation to homography) and warping, for HDR merge ([docs/align.md](docs/align.md)) |
| `crates/merge` | HDR merge of RAW brackets into a floating point DNG: exposures, weights, alignment, output ([docs/merge.md](docs/merge.md)) |
| `tools/` | `lumen-cli`, `xmp-dump`, `compare` |

## Adobe profiles

Presets refer to Adobe's colour profiles by name ("Adobe Color" on top of "Adobe Standard"). When Lightroom, Lightroom Classic or Camera Raw is installed, Lumen reads those profiles from the installation, on macOS and on Windows; nothing of Adobe's is included in this repository. Without them the result is close, but not identical. Details in [docs/pipeline.md](docs/pipeline.md#installed-adobe-profiles).

## Licences

The project is MIT licensed, with two exceptions. `rawler` (RAW decoding) is **LGPL-2.1**: because Rust links statically, distributing the executable requires complying with the LGPL (for instance by providing the sources or object files for relinking).

The ACR3 tone curve table in `crates/pipeline/src/acr3.rs` comes from Adobe's DNG SDK and is distributed under its licence ([third_party/dng_sdk/LICENSE](third_party/dng_sdk/LICENSE)), which allows use and redistribution with the copyright notice.
