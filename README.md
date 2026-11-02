# Lumen (nome provvisorio)

Editor RAW stile Lightroom in Rust, capace di applicare preset Lightroom/Camera Raw (`.xmp`, `.dng` mobile) e LUT `.cube`/`.3dl`.

## Stato: milestone M2

| Crate | Contenuto |
|---|---|
| `crates/xmp` | parser/writer XMP (`crs:*`), estrazione da DNG/TIFF/JPEG, `DevelopSettings` tipizzato, decoder delle LUT incorporate (base85 Adobe + zlib) |
| `crates/lut` | `.cube` / `.3dl`, interpolazione tetraedrica, amount, export `.cube` |
| `crates/raw` | decodifica RAW con `rawler` (DNG, CR2/CR3, NEF, ARW, RW2, RAF…) e JPEG/PNG/TIFF; black/white level, demosaic bilineare (Bayer e X-Trans) o half-size, crop di default, orientamento; modello colore DNG (ColorMatrix/ForwardMatrix interpolate per temperatura, CameraCalibration, AnalogBalance) → **ProPhoto lineare**; temperatura/tinta su scala Camera Raw; preview incorporata |
| `tools/xmp-dump` | CLI di ispezione dei preset |
| `tools/lumen-cli` | `info`, `develop`, `preview` |

Le note di formato sono in [docs/xmp-format.md](docs/xmp-format.md).

## Uso

```bash
cargo run -p xmp-dump -- "samples/Dreamy days in spring.xmp"
cargo run -p xmp-dump -- "samples/Dreamy days in spring - mobile version.dng" --raw
cargo run -p xmp-dump -- profilo.xmp --cube out.cube      # estrae una LUT incorporata
cargo run -p xmp-dump -- preset.xmp --sidecar copia.xmp   # test del writer
cargo test

cargo run --release -p lumen-cli -- info    foto.dng
cargo run --release -p lumen-cli -- develop foto.dng -o out.jpg --size 2048
cargo run --release -p lumen-cli -- develop foto.dng -o out.tif --temp 5600 --tint 0 --exposure 0.5
cargo run --release -p lumen-cli -- develop foto.dng -o anteprima.jpg --half   # 2×2 binning, molto più veloce
cargo run --release -p lumen-cli -- preview foto.dng -o lightroom.jpg            # preview incorporata (riferimento per tools/compare)
```

`develop` per ora produce un rendering **neutro**: bilanciamento as-shot (o `--temp`/`--tint`), `BaselineExposure` del DNG più `--exposure`, conversione ProPhoto → sRGB con clip. Curva di tono e preset arrivano con la pipeline (M3). I file `.jpg` sono a 8 bit, `.png`/`.tif` a 16 bit.

Su Windows, se il progetto si trova in un percorso molto lungo, il linker MSVC fallisce con `LNK1104`. In quel caso crea un `.cargo/config.toml` locale (ignorato da git) con:

```toml
[build]
target-dir = "C:/temp/lightphoto-target"
```

Il piano completo è in [plan/PIANO.md](plan/PIANO.md). I file di esempio (`samples/`) contengono il preset `.xmp`; il `.dng` non è versionato.

## Prossimi passi

- **M3**: pipeline (WB, tono, curve, HSL, color grading, calibrazione) ed export JPEG multi-dimensione.
- **M4**: UI egui.

## Licenze

Il progetto è MIT, ma `rawler` (decodifica RAW) è **LGPL-2.1**. Con il linking statico di Rust, distribuire il binario richiede di rispettare la LGPL (ad esempio fornendo i sorgenti o gli object file per il re-linking). Va deciso prima di distribuire l'app.
