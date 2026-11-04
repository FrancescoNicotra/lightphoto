# Pipeline di sviluppo

Implementata nel crate [`pipeline`](../crates/pipeline). Ogni pannello di Camera Raw è tradotto in un'operazione nostra. Gli algoritmi PV2012+ di Adobe non sono pubblici, quindi gli slider vanno nella stessa direzione con una forza simile, ma non danno risultati identici. Lo misuriamo con `tools/compare`.

## Ordine

| # | Fase | Spazio | Note |
|---|---|---|---|
| 1 | **WB** | camera → ProPhoto lineare | `As Shot`, `Custom` (Temperature/Tint assolute con le matrici DNG), preset con nome (Daylight, Cloudy…), `Auto` (grey world). Sui JPEG/TIFF: `IncrementalTemperature/Tint` (±100 mired). |
| 1b | **HueSatMap del profilo camera** | ProPhoto lineare | solo con un DCP Adobe (vedi sotto), interpolata tra i due illuminanti come le matrici |
| 2 | **Calibrazione** | matrice 3×3 lineare | ogni primaria Rec.709 ruotata in tinta (±100 ≈ ±25°) e scalata in saturazione; il bianco resta bianco |
| 3 | **Esposizione** | lineare | `Exposure2012` + `BaselineExposure` del DNG |
| 4 | **Spalla** (solo RAW) | lineare | identità fino a 0,7, poi `tanh`: le alte luci oltre il clip restano recuperabili |
| 4b | **LookTable** | ProPhoto lineare | quella del Look (es. Adobe Color), altrimenti quella del DCP |
| 5 | **Curva di profilo** | lineare / Melissa* | solo RAW: `ProfileToneCurve` del DCP oppure, se manca (come in Adobe Standard, o senza DCP), la curva predefinita **ACR3** del DNG SDK ([`acr3.rs`](../crates/pipeline/src/acr3.rs)); applicata in lineare con il metodo "RGB tone" del SDK, che preserva la tinta. Poi la curva del Look (Adobe Color: S leggera) |
| 6 | **Tabella RGB del profilo** | primarie e gamma dichiarate | `crs:RGBTable` dei profili creativi, con Amount |
| 7 | **Base** | Melissa | Contrasto (curva a potenza attorno al grigio medio), Luci/Ombre (campane centrate a 0,75 e 0,25), Bianchi/Neri (estremi). **Globali**, non locali come in Adobe |
| 8 | **Curve** | Melissa | parametrica (4 regioni con split) → punti master → R/G/B, con spline cubica monotona (Fritsch–Carlson) |
| 9 | **ShadowTint** | Melissa | verde/magenta nelle ombre |
| 10 | **NR colore** | Melissa | blur della crominanza, raggio ∝ Amount |
| 11 | **Texture / Clarity** | luma Melissa | differenza da un blur (0,15% / 1,2% del lato lungo), pesata sui mezzitoni |
| 12 | **Dehaze** | Melissa | modello globale a velo |
| 13 | **Sharpening** | luma Melissa | unsharp mask: Radius, Detail (limite agli aloni), Masking (solo bordi) |
| 14 | **Vignettatura post-crop** | Melissa | Amount, Midpoint, Roundness, Feather |
| 15 | **HSL** | Rec.709 codificato → (Y, a, b) | 8 bande a pesi sovrapposti che sommano a 1. Hue ±100 = ±30°. La luminanza non tocca i grigi |
| 16 | **Vibranza / Saturazione** | (Y, a, b) | la vibranza agisce di più sui colori spenti e protegge gli incarnati |
| 17 | **Color grading** | (Y, a, b) | campane di luminanza per ombre/mezzitoni/luci (Blending ne regola la larghezza, Balance il centro), più la ruota globale |
| 18 | **LUT .cube** (`--lut`) | sRGB codificato | per ultima, con amount |
| 19 | **Grana** | dopo il resize | value noise a due ottave (Size, Frequency = Roughness), seed fisso |

\* *Melissa* = primarie ProPhoto con la curva di trasferimento sRGB, lo spazio delle curve e dei valori RGB di Lightroom.

## Profili Adobe installati

I preset citano i profili senza includerli: "Adobe Color" è `Stubbed` e il DNG riporta solo l'hash della sua LookTable. Se sul computer c'è un'installazione di Lightroom, Lightroom Classic o Camera Raw, `pipeline::adobe` la usa così:

- **Profilo camera**: `CameraProfiles/Adobe Standard/<marca> <modello> Adobe Standard.dcp` (o quello che il Look chiede in `crs:CameraProfile`). Se il nome non corrisponde, cerca tra i DCP della stessa marca quello con la stessa ColorMatrix: così la GX80 trova il profilo "Panasonic DMC-GX85". Il DCP sostituisce le matrici del file e aggiunge HueSatMap, LookTable ed eventuale curva.
- **Look**: il file `.xmp` con lo stesso UUID sotto `Settings/…/Profiles/`, ad esempio `Adobe Raw/Adobe Color.xmp`, che contiene la LookTable (36×16×16) e la curva del profilo.

Cartelle cercate: quelle in `LUMEN_ADOBE_DIR`; su macOS il bundle di Lightroom e Lightroom Classic e `/Library/Application Support/Adobe/CameraRaw`; su Windows `C:\ProgramData\Adobe\CameraRaw` e `%APPDATA%\Adobe\CameraRaw`. I file sono di Adobe: si leggono dall'installazione locale e non vengono copiati nel repository. `--no-adobe` li disattiva.

## Cosa manca o è approssimato

- **Senza un'installazione Adobe** la LookTable di Adobe Color non è disponibile: applichiamo solo la sua curva, e la CLI lo segnala.
- **Luci/Ombre, Texture, Clarity, Dehaze** sono globali o semplici. In Adobe sono filtri locali edge-aware (dopo l'MVP: guided filter).
- **Riduzione rumore della luminanza** non implementata (la CLI lo segnala).
- **Correzione delle lenti**: assente. La JPEG della fotocamera GX80, ad esempio, corregge la distorsione e quindi ha un'inquadratura più stretta.
- **Profili ICC in ingresso** (JPEG/TIFF): si assume sRGB.

## Export `.cube`

`lumen-cli cube preset.xmp -o out.cube` (oppure `xmp-dump preset.xmp --cube out.cube`) campiona le fasi 2, 5–9 e 15–17 in modalità "immagine già renderizzata", con ingresso e uscita sRGB. WB, operazioni spaziali e grana non entrano in una LUT.

## Verifica

```bash
cargo run --release -p lumen-cli -- develop foto.dng --preset preset.xmp -o nostro.jpg
cargo run --release -p lumen-cli -- preview foto.dng -o lightroom.jpg   # o un export fatto in Lightroom
cargo run --release -p compare -- nostro.jpg lightroom.jpg --diff diff.png
```

`compare` stampa il ΔE2000 medio, mediano e al 95° percentile, la Lab media delle due immagini e una mappa delle differenze (nero = 0, bianco ≥ 10). Il target per l'MVP è un ΔE medio < 3 contro Lightroom, e va misurato sul DNG "mobile version", che contiene la preview renderizzata da Lightroom con il preset. Sul RW2 di raw.pixls.us abbiamo come riferimento solo la JPEG della fotocamera, che ha un altro look e un'altra inquadratura: lì sono significative solo le medie (rendering di base con ACR3 e Adobe Standard: L* 39,2 contro 40,9, ΔE delle medie 3,6; la JPEG Panasonic è più satura per scelta).
