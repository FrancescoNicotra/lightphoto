# Piano: editor RAW stile Lightroom che applica i preset XMP/LUT (Rust)

## Context
Vogliamo un'app desktop leggera e veloce, simile a Lightroom, che:
- apra file RAW (DNG prima di tutto, poi CR2/CR3, NEF, ARW, RAF…) e li sviluppi in modo non distruttivo;
- offra i controlli base (esposizione, contrasto, luci, ombre, bianchi, neri, temperatura/tinta, vibranza/saturazione);
- applichi i preset Lightroom/Camera Raw (.xmp, anche nella versione "mobile" .dng) e le LUT .cube/.3dl;
- esporti in JPEG (e TIFF/PNG) a più dimensioni.

Stack: **Rust**, UI con `egui`/`eframe` (backend wgpu), un solo eseguibile. La cartella di lavoro è vuota, quindi si parte da zero.

## Cosa abbiamo scoperto dai file forniti
I file sono in `Downloads\Dreamy days in spring\`.

1. **`Dreamy days in spring.xmp` non contiene una LUT.** È un preset **parametrico** (ProcessVersion 15.4). Contiene WB Custom 5600K, Contrast −50, Highlights −25, Shadows +50, Texture −10, Clarity −15, Dehaze −5, Vibrance +10, curva master e curve R/B, HSL (Yellow/Green hue +45, sat −30…), Split toning/Color grading (ombre 38°/20, luci 222°/15, mezzitoni 65°/40), Calibrazione (RedHue +20, GreenHue +20, BlueHue −20), Grain 30, Sharpening, Color NR.
   - Le curve e il nome sono **elementi figli** (`rdf:Seq` / `rdf:Alt`), non attributi.
   - `crs:Look` = "Adobe Color" con `Stubbed="true"`: è un profilo integrato di Adobe, solo referenziato e non incluso.
   - Quindi "applicare la LUT" significa in realtà **implementare nativamente ogni operazione `crs:`**.
2. **`... mobile version.dng`** è un vero RAW **Panasonic DMC-GX80** (LUMIX G 20mm), convertito in DNG da Lightroom Classic 13.4, con le stesse impostazioni nell'XMP incorporato (offset circa 858).
   - Qui il Look porta `crs:LookTable="E1095149…"`, un hash che punta a una tabella interna di Adobe.
   - È il file di test ideale. Il DNG contiene probabilmente anche la **preview renderizzata da Lightroom**, da usare come riferimento per misurare quanto ci avviciniamo.
3. **`xmp_to_lut.py` / `output_….cube`** è un tentativo precedente. I suoi errori spiegano perché il risultato non somiglia a Lightroom:
   - legge solo gli attributi XML, quindi **le curve di tono non vengono mai applicate**;
   - il contrasto viene applicato in lineare attorno a 0.5 e le ombre come moltiplicatore ×2.25. Così il **nero diventa 0.70**: è la prima riga del .cube;
   - fattori arbitrari (×1.2, ×1.5) e bande HSV con bordi netti, senza falloff;
   - mancano WB, vibranza, color grading, calibrazione, texture/clarity/dehaze e grana;
   - la Hald generata con `meshgrid` (indexing "xy") e il writer .cube usano ordini di assi incoerenti.
4. **Limite fondamentale: un XMP non è riducibile a una LUT.**
   - WB assoluta (5600K) ha senso solo sul RAW.
   - Texture, Clarity, Dehaze, Sharpening, NR e Grain sono operazioni **spaziali**.
   - Quindi l'app deve **applicare l'XMP direttamente nella pipeline RAW**. L'export .cube resta una funzione secondaria, che cattura solo la parte colore/tono puntuale.

---

## Step 0: setup del progetto
```
lumen/                      (nome provvisorio)
  crates/
    xmp/        parser/writer crs:* (attributi + elementi rdf), estrazione XMP da DNG/JPEG
    lut/        LUT 3D .cube/.3dl, interpolazione tetraedrica, export .cube
    raw/        decodifica RAW (DNG prima), demosaic, matrici colore
    pipeline/   DevelopSettings + operazioni
    export/     resize + JPEG/TIFF/PNG + metadati
    app/        UI egui (binario)
  tools/xmp-dump/  CLI reverse engineering
  tools/compare/   ΔE tra nostro render e preview Lightroom
  samples/      i file "Dreamy days in spring" (copiati, non versionati)
```
Dipendenze: `quick-xml`, `flate2`, `rawler` (pure Rust, DNG/CR3/NEF/ARW/RW2…), `rayon`, `glam`, `image`, `jpeg-encoder`, `fast_image_resize`, `kamadak-exif`, `eframe`/`egui`, `rfd`.

## Step 1: reverse engineering XMP (crate `xmp` + `xmp-dump`)
1. **Parser robusto**: namespace `crs:` (`http://ns.adobe.com/camera-raw-settings/1.0/`). Ogni proprietà può comparire come attributo oppure come elemento figlio. Va gestito anche `rdf:Seq` (curve, `PointColors`), `rdf:Alt` (Name/Group) e le strutture annidate (`crs:Look/rdf:Description`, `crs:LensBlur`).
2. **Estrazione XMP da contenitori**: .xmp sidecar, **DNG** (tag TIFF 700 / pacchetto `<?xpacket`) e JPEG (APP1). Così i preset "mobile version.dng" si importano come i .xmp.
3. **Modello `DevelopSettings`** tipizzato, con default Adobe e range. La mappatura completa delle chiavi va in `docs/xmp-format.md`; sul preset fornito si validano: Base, Presence, ToneCurvePV2012(+RGB), Parametric*, HSL ×8, SplitToning*/ColorGrade*, calibrazione (`RedHue`…`ShadowTint`), Grain*, Sharpen*, NR.
4. **Look/Profili**:
   - `crs:Look` con `Name`/`Amount`/`UUID`. Se `Stubbed`, si risolve per nome.
   - Per i profili con LUT incorporata (`crs:RGBTable` + `crs:Table_<md5>`): decoder base85 Adobe → zlib → tabella 3D con delta dall'identità, più spazio colore, gamma e amount. Il layout va verificato sul DNG SDK open source (`dng_big_table.cpp`).
   - "Adobe Color" e gli altri profili integrati non sono nel file. Opzione 1: se Camera Raw/Lightroom è installato, si caricano da `C:\ProgramData\Adobe\CameraRaw\Settings\Adobe\Profiles\` (e dalle cartelle CameraProfiles). Opzione 2: base neutra con una tone curve di default simile ad Adobe.
5. **Writer**: sidecar `foto.xmp` compatibile con Lightroom (round-trip senza perdere chiavi sconosciute).

**Output:** `xmp-dump "Dreamy days in spring.xmp"` stampa tutte le impostazioni raggruppate per pannello. `xmp-dump file.dng` fa lo stesso sull'XMP incorporato.

## Step 2: crate `lut`
- Parser `.cube` (`LUT_3D_SIZE`, `DOMAIN_*`, ordine R più veloce) e `.3dl`. Interpolazione **tetraedrica**, `amount` 0–200%.
- Export: si campiona la parte puntuale della pipeline (tono + colore, senza WB e senza operazioni spaziali) su una griglia 33³/65³ e si scrive un `.cube` corretto. Sostituisce `xmp_to_lut.py`.

## Step 3: decodifica RAW (crate `raw`)
- `rawler`, partendo dal **DNG del GX80**: CFA, black/white level, `AsShotNeutral`, `ColorMatrix`/`ForwardMatrix`, orientamento, preview incorporata.
- Normalizzazione, poi demosaic bilineare per l'MVP (RCD/AMaZE dopo), poi camera RGB → **ProPhoto lineare f32** (spazio di lavoro, come Adobe).
- Anteprima a 2048 px lato lungo per la UI. Supporto anche per JPEG/TIFF in ingresso.

## Step 4: pipeline (crate `pipeline`)
L'ordine segue quello di Camera Raw. Le operazioni puntuali vengono fuse dove possibile.
1. **WB**: Temperature/Tint → xy (CCT) → interpolazione delle matrici DNG fra i due illuminanti → adattamento.
2. **Calibrazione**: rotazione/scala delle primarie (`RedHue/Sat`…) e `ShadowTint`.
3. **Esposizione** ×2^EV in lineare.
4. **Tono** in uno spazio percettivo (log/gamma, non lineare):
   - Contrasto come S-curve attorno al grigio medio.
   - Bianchi/Neri sugli endpoint con rolloff.
   - Luci/Ombre: per l'MVP curva mascherata per luminanza, poi locale con guided filter.
   - Curve parametriche, poi `ToneCurvePV2012` master e R/G/B in spazio **gamma**, con spline cubica monotona sui punti 0–255 e non interpolazione lineare.
5. **Presence**: Vibrance (pesata sulla saturazione) e Saturation. Texture/Clarity come contrasto locale multiscala (blur di base più detail boost). Dehaze dopo l'MVP.
6. **HSL** su 8 hue con pesi a campana sovrapposti, quindi senza bordi netti.
7. **Color grading/split toning**: tinte di ombre, mezzitoni e luci con maschere di luminanza, Blending e Balance.
8. **Look/LUT** (profilo XMP o .cube) nello spazio e gamma dichiarati, con Amount.
9. **Dettaglio**: sharpening (unsharp mask con radius/detail/masking) e color NR (blur di crominanza).
10. **Grana** procedurale con seed fisso (Amount/Size/Roughness), applicata **dopo** il resize nell'export.
11. Output: ProPhoto lineare → sRGB, dithering 8 bit.

CPU con `rayon` a tile, con un obiettivo sotto i 30 ms sull'anteprima. La stessa funzione serve per l'export a piena risoluzione. Un porting WGSL è possibile dopo l'MVP.

Nota onesta: gli algoritmi PV2012+ di Adobe sono proprietari, quindi gli slider saranno **molto simili ma non identici**. Curve, HSL, color grading e LUT si possono avvicinare molto. Lo misuriamo con `tools/compare`.

## Step 5: export (crate `export`)
- Dimensioni: originale, lato lungo 2048/1600/1080 px, custom, percentuale.
- Lanczos3 in lineare, sharpening per output opzionale, grana dopo il resize.
- JPEG qualità 0–100 con ICC sRGB. TIFF 16 bit e PNG.
- EXIF essenziali e orientamento. Batch in background con template del nome.

## Step 6: UI stile Lightroom (crate `app`, egui)
- Tema scuro (#1e1e1e/#2b2b2b, accento azzurro).
- **Sinistra**: preset (.xmp, .dng mobile, .cube) con hover-preview.
- **Centro**: immagine con fit/100%, pan, prima/dopo (`\`), istogramma.
- **Destra**: pannelli Base, Curva, HSL, Color grading, Dettaglio, Effetti (grana), Calibrazione, Profilo/LUT con Amount.
- **In basso**: filmstrip.
- Slider con doppio clic per il reset e valore editabile. Undo/redo. Worker thread che scarta le richieste obsolete.
- Dialog export (Ctrl+Shift+E).

## Milestone
| # | Contenuto | Risultato utilizzabile |
|---|---|---|
| M1 | Step 0–1–2 | `xmp-dump` legge correttamente il preset fornito (curve incluse) ed esporta `.cube` |
| M2 | Step 3 + CLI `lumen-cli develop "mobile version.dng" -o out.jpg` | il DNG del GX80 sviluppato senza preset |
| M3 | Step 4 (WB, tono, curve, HSL, vibranza, color grading, calibrazione) + Step 5 | il DNG con "Dreamy days in spring" applicato, confrontato con la preview Lightroom |
| M4 | Step 6 | **MVP desktop** |
| Post | Texture/Clarity locali, Dehaze, demosaic migliore, GPU, maschere, catalogo, X-Trans | |

## Verifica
- `cargo test`:
  - il parser legge tutti i 100+ campi del preset fornito, incluse le 4 curve;
  - XMP estratto dal DNG ≡ .xmp per le chiavi `crs:` condivise;
  - round-trip del writer;
  - tetraedrica corretta su identità e inversione;
  - curve monotone e `(0,0)-(255,255)` = identità.
- **`tools/compare`**: render del DNG con il preset contro la preview incorporata da Lightroom (o un export JPEG fatto da te in Lightroom), con ΔE2000 medio/95° percentile e mappa delle differenze. Target per l'MVP: ΔE medio < 3.
- Test manuale UI: apertura cartella, slider in tempo reale, applicazione di .xmp, .dng mobile e .cube, export a 3 dimensioni con controllo di dimensioni ed EXIF.
