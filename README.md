# Lumen (nome provvisorio)

Editor RAW stile Lightroom in Rust, capace di applicare preset Lightroom/Camera Raw (`.xmp`, `.dng` mobile) e LUT `.cube`/`.3dl`.

## Stato: milestone M4 (MVP desktop)

| Crate | Contenuto |
|---|---|
| `crates/xmp` | parser/writer XMP (`crs:*`), estrazione da DNG/TIFF/JPEG, `DevelopSettings` tipizzato, decoder delle LUT incorporate (base85 Adobe + zlib) |
| `crates/lut` | `.cube` / `.3dl`, interpolazione tetraedrica, amount, export `.cube` |
| `crates/raw` | decodifica RAW con `rawler` (DNG, CR2/CR3, NEF, ARW, RW2, RAF…) e JPEG/PNG/TIFF; black/white level, demosaic bilineare (Bayer e X-Trans) o half-size, crop di default, orientamento; modello colore DNG (ColorMatrix/ForwardMatrix interpolate per temperatura, CameraCalibration, AnalogBalance) → **ProPhoto lineare**; temperatura/tinta su scala Camera Raw; preview incorporata |
| `crates/pipeline` | applica i preset: WB (assoluta/relativa), calibrazione, esposizione, profilo (curva Adobe Color, tabelle RGB), Base, curve parametrica e a punti, HSL, vibranza/saturazione, color grading, Texture/Clarity/Dehaze, sharpening, NR colore, vignettatura, grana, LUT `.cube`; export `.cube` della parte puntuale. Dettagli e limiti in [docs/pipeline.md](docs/pipeline.md) |
| `crates/export` | dimensioni (originale, lato lungo, %, box), Lanczos3 in lineare, grana dopo il resize, JPEG 8 bit / PNG e TIFF 16 bit con profilo ICC sRGB, EXIF essenziali, template dei nomi |
| `crates/app` | **app desktop** `lumen` (egui/wgpu): preset a sinistra con anteprima al passaggio, foto con adatta/100%, pan, zoom, prima/dopo e istogramma, pannelli di sviluppo a destra, filmstrip, undo/redo, sidecar `.xmp`, export in background. Dettagli in [docs/app.md](docs/app.md) |
| `tools/xmp-dump` | CLI di ispezione dei preset (`--cube` ora funziona anche sui preset parametrici) |
| `tools/lumen-cli` | `info`, `develop` (anche batch), `preview`, `cube` |
| `tools/compare` | ΔE2000 tra il nostro render e un riferimento, con mappa delle differenze |

Le note di formato sono in [docs/xmp-format.md](docs/xmp-format.md).

## Uso

### App

```bash
cargo run --release -p app                                    # riapre l'ultima cartella
cargo run --release -p app -- samples/pixls/P1010041.RW2 --presets samples
```

File → Apri cartella (⌘O / Ctrl+O) apre una cartella di RAW/JPEG; la filmstrip in basso le mostra tutte. Scegli la cartella dei preset (`.xmp`, `.dng` "mobile", `.cube`) con il pulsante **Cartella…** a sinistra: passando sopra un preset l'anteprima lo mostra, il clic lo applica. Gli slider aggiornano l'anteprima mentre li trascini; doppio clic per azzerarli, clic sul numero per scriverlo. `\` prima/dopo, ⌘Z / ⇧⌘Z (Ctrl+Z / Ctrl+Shift+Z) annulla e ripeti, ⌘S salva il sidecar `.xmp` accanto al RAW, ⇧⌘E (Ctrl+Shift+E) esporta. Tutte le scorciatoie e come funziona il rendering in [docs/app.md](docs/app.md).

### Riga di comando

```bash
cargo run -p xmp-dump -- "samples/Dreamy days in spring.xmp"
cargo run -p xmp-dump -- "samples/Dreamy days in spring - mobile version.dng" --raw
cargo run -p xmp-dump -- profilo.xmp --cube out.cube      # estrae una LUT incorporata
cargo run -p xmp-dump -- preset.xmp --sidecar copia.xmp   # test del writer
cargo test

cargo run --release -p lumen-cli -- info    foto.dng
cargo run --release -p lumen-cli -- develop foto.dng -o out.jpg --preset "samples/Dreamy days in spring.xmp" --size 2048
cargo run --release -p lumen-cli -- develop *.RW2 -o export/ --preset p.xmp --size 1080 --name "{name}-{preset}"   # batch
cargo run --release -p lumen-cli -- develop foto.dng -o out.tif --lut film.cube --lut-amount 0.7
cargo run --release -p lumen-cli -- develop foto.dng -o anteprima.jpg --size 1600 --fast   # sviluppa direttamente a 1600 px
cargo run --release -p lumen-cli -- cube    "samples/Dreamy days in spring.xmp" -o dreamy.cube
cargo run --release -p lumen-cli -- preview foto.dng -o lightroom.jpg            # preview incorporata (riferimento)
cargo run --release -p compare   -- out.jpg lightroom.jpg --diff diff.png
```

Se sul computer c'è Lightroom, Lightroom Classic o Camera Raw, `develop` usa automaticamente i profili di Adobe: il DCP "Adobe Standard" della fotocamera e le LookTable dei profili come Adobe Color. I file restano nell'installazione e non sono inclusi nel repository; vedi [docs/pipeline.md](docs/pipeline.md#profili-adobe-installati). `--no-adobe` li disattiva.

Senza `--preset` si ottiene il rendering di base (WB as-shot, curva ACR3 di Adobe, sharpening 40 e NR colore 25, i default di Camera Raw per i RAW). Sul RW2 della GX80 (16 MP, Apple Silicon, release) lo sviluppo a piena risoluzione con il preset richiede circa 0,6 s; con `--fast` a 1600 px meno di 0,1 s.

Per i test sul campione reale scarica il RW2 CC0 da https://raw.pixls.us/data/Panasonic/DMC-GX80/P1010041.RW2 in `samples/pixls/`. I test che lo usano vengono saltati se il file manca.

Su Windows, se il progetto si trova in un percorso molto lungo, il linker MSVC fallisce con `LNK1104`. In quel caso crea un `.cargo/config.toml` locale (ignorato da git) con:

```toml
[build]
target-dir = "C:/temp/lightphoto-target"
```

Il piano completo è in [plan/PIANO.md](plan/PIANO.md). I file di esempio (`samples/`) contengono il preset `.xmp`; il `.dng` non è versionato.

## Prossimi passi

- **Verifica M3 contro Lightroom**: con il DNG "mobile version" (preview renderizzata da Lightroom con il preset), target ΔE medio < 3.
- Dopo l'MVP: Luci/Ombre e Clarity locali (guided filter), NR luminanza, correzione lenti, demosaic migliore, GPU.
- App: ritaglio e raddrizzamento, catalogo con salvataggio automatico delle modifiche, miniature con l'orientamento EXIF, export di più foto insieme.

## Licenze

Il progetto è MIT, con due eccezioni. `rawler` (decodifica RAW) è **LGPL-2.1**: con il linking statico di Rust, distribuire il binario richiede di rispettare la LGPL (ad esempio fornendo i sorgenti o gli object file per il re-linking). Va deciso prima di distribuire l'app.

La tabella della curva ACR3 in `crates/pipeline/src/acr3.rs` viene dal DNG SDK di Adobe ed è distribuita con la sua licenza ([third_party/dng_sdk/LICENSE](third_party/dng_sdk/LICENSE)), che permette uso e ridistribuzione mantenendo l'avviso di copyright.
