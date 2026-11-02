# Lumen (nome provvisorio)

Editor RAW stile Lightroom in Rust, capace di applicare preset Lightroom/Camera Raw (`.xmp`, `.dng` mobile) e LUT `.cube`/`.3dl`.

## Stato: milestone M1

| Crate | Contenuto |
|---|---|
| `crates/xmp` | parser/writer XMP (`crs:*`), estrazione da DNG/TIFF/JPEG, `DevelopSettings` tipizzato, decoder delle LUT incorporate (base85 Adobe + zlib) |
| `crates/lut` | `.cube` / `.3dl`, interpolazione tetraedrica, amount, export `.cube` |
| `tools/xmp-dump` | CLI di ispezione dei preset |

Le note di formato sono in [docs/xmp-format.md](docs/xmp-format.md).

## Uso

```bash
cargo run -p xmp-dump -- "samples/Dreamy days in spring.xmp"
cargo run -p xmp-dump -- "samples/Dreamy days in spring - mobile version.dng" --raw
cargo run -p xmp-dump -- profilo.xmp --cube out.cube      # estrae una LUT incorporata
cargo run -p xmp-dump -- preset.xmp --sidecar copia.xmp   # test del writer
cargo test
```

Su Windows, se il progetto si trova in un percorso molto lungo, il linker MSVC fallisce con `LNK1104`. In quel caso crea un `.cargo/config.toml` locale (ignorato da git) con:

```toml
[build]
target-dir = "C:/temp/lightphoto-target"
```

Il piano completo è in [plan/PIANO.md](plan/PIANO.md). I file di esempio (`samples/`) contengono il preset `.xmp`; il `.dng` non è versionato.

## Prossimi passi

- **M2**: decodifica RAW (rawler) e CLI `develop`.
- **M3**: pipeline (WB, tono, curve, HSL, color grading, calibrazione) ed export JPEG multi-dimensione.
- **M4**: UI egui.
