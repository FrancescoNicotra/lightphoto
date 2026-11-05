# App desktop

Crate [`crates/app`](../crates/app), binario `lumen`, con [egui](https://github.com/emilk/egui)/eframe 0.37 (backend wgpu).

```bash
cargo run --release -p app                                   # riapre l'ultima cartella
cargo run --release -p app -- samples/pixls                  # una cartella
cargo run --release -p app -- samples/pixls/P1010041.RW2 --presets samples
```

## Interfaccia

| Zona | Contenuto |
|---|---|
| Menu | **File**: Apri cartella (⌘O / Ctrl+O), Apri file, Cartella preset, Carica LUT, Salva impostazioni (.xmp, ⌘S), Esporta (⇧⌘E / Ctrl+Shift+E). **Modifica**: Annulla, Ripeti, Reimposta tutto. **Vista**: Adatta, 100%, Prima/Dopo |
| Sinistra | preset della cartella scelta (`.xmp`, `.dng` "mobile", `.cube`/`.3dl`), raggruppati per gruppo o sottocartella, con ricerca. Passando sopra un preset l'anteprima lo mostra; il clic lo applica |
| Centro | foto con Adatta / 100% / 200%; clic per passare da adatta a 100% nel punto cliccato, trascinamento per spostarsi, pinch o ⌘/Ctrl + rotella per lo zoom attorno al cursore. `\` mostra la foto prima delle modifiche |
| Destra | istogramma RGB con indicatori di clipping e dati di scatto; pannelli Profilo e LUT (Amount del profilo e della LUT), Base (WB con preset e Temp/Tinta, tono, presenza), Curva di viraggio (parametrica, punti master modificabili, R/G/B), HSL (8 bande × tonalità/saturazione/luminanza), Color grading (ruote a 3 vie e globale, Fusione, Bilanciamento), Dettaglio, Effetti (vignettatura, grana), Calibrazione |
| In basso | filmstrip della cartella aperta (preview incorporata nei RAW, l'immagine stessa per i JPEG/PNG/TIFF) e barra di stato con tempi di decodifica/sviluppo e note (profili Adobe usati, cosa è approssimato) |

Slider: doppio clic sull'etichetta o sulla barra riporta al valore predefinito; clic sul numero per scriverlo, trascinamento sul numero per regolarlo. Il doppio clic su Temp./Tinta torna al bilanciamento "Come scattato". Nella curva a punti: clic sulla curva per aggiungere un punto, trascinamento per spostarlo, doppio clic per toglierlo.

Altre scorciatoie (fuori dai campi di testo): ⌘Z / Ctrl+Z annulla, ⇧⌘Z / Ctrl+Shift+Z (o Ctrl+Y) ripete, `Z` adatta/100%, ← → foto precedente/successiva, Esc chiude il dialog di export. Si possono trascinare nella finestra cartelle, foto, preset `.xmp` e LUT.

## Modifiche, undo e sidecar

- Applicare un preset cambia solo le chiavi che contiene, come in Lightroom: due preset in sequenza si sommano.
- Undo/redo per istantanee: un gesto (un trascinamento, un valore scritto, un preset) è un passo. Le modifiche di ogni foto restano in memoria finché l'app è aperta.
- **⌘S** salva le impostazioni nel sidecar `nome.xmp` accanto al RAW, compatibile con Lightroom (chiavi `crs:`; profilo e tabelle copiati dal preset). All'apertura il sidecar viene letto. Per JPEG/TIFF/PNG Lightroom scrive dentro il file, quindi lì non si salva nulla.

## Rendering

- Un **worker thread** decodifica la foto a metà risoluzione (`DecodeOptions { half_size: true }`), la riduce a 2048 px e la sviluppa. Le richieste passano per una casella da un posto: una richiesta non ancora iniziata viene sostituita dalla successiva, quindi conta solo l'ultimo valore dello slider.
- Durante un trascinamento si sviluppa una **bozza a 1024 px**; al rilascio parte l'anteprima a 2048 px. I raggi di nitidezza e riduzione rumore sono scalati sulla dimensione piena (`RenderOptions::full_size`), così l'anteprima somiglia all'export.
- Quando la vista mostra più pixel dell'anteprima (100%, 200%) il worker decodifica la foto intera e la sviluppa a piena risoluzione dopo l'anteprima.
- I profili Adobe installati (vedi [pipeline.md](pipeline.md#profili-adobe-installati)) si preparano una volta per profilo e restano in cache.
- Tempi misurati sul RW2 della GX80 (Apple Silicon, release, con la macchina carica di altri processi, quindi per eccesso): decodifica half-size 60–120 ms; anteprima a 2048 px 130–300 ms senza preset e 200–400 ms con "Dreamy days in spring" (Texture/Clarity/Dehaze, sharpening e grana sono le fasi spaziali più care); bozza a 1024 px durante il trascinamento ~60 ms in un benchmark isolato della pipeline, ma 280–320 ms nell'app nelle stesse condizioni di carico. L'obiettivo di 100 ms per aggiornamento va **rimisurato a macchina scarica**. Il primo render con un profilo Adobe nuovo legge i file dell'installazione e può richiedere qualche secondo; poi resta in cache. Con lo script attivo (vedi sotto) ogni render stampa il suo tempo su stderr.

## Export

Dialog con dimensioni (originale, lato lungo, percentuale, riquadro), formato JPEG (qualità) / PNG 16 bit / TIFF 16 bit, nitidezza output, template del nome (`{name}`, `{preset}`, `{size}`) e cartella (quella della foto se non scelta). L'export decodifica e sviluppa la foto a piena risoluzione in un thread separato con [`export`](../crates/export) (Lanczos3, profilo ICC sRGB, EXIF); la barra di stato mostra l'esito. Le impostazioni del dialog, la cartella delle foto e quella dei preset vengono ricordate.

## Esecuzioni scriptate

Per verificare l'interfaccia senza cliccare (e rifare gli screenshot della documentazione) la variabile `LUMEN_SCRIPT` esegue una sequenza di passi separati da `;` o a capo; l'elenco è in [`script.rs`](../crates/app/src/script.rs). Esempio:

```bash
LUMEN_SCRIPT="wait; preset Dreamy; wait; drag Exposure2012 0 1 12; wait; shot dopo.png; undo; before on; wait; shot prima.png; quit" \
  cargo run --release -p app -- samples/pixls/P1010041.RW2 --presets samples
```

Con lo script attivo l'app scrive su stderr il tempo di ogni render.

## Limiti

- Nessun catalogo: si lavora per cartella, e le modifiche non salvate con ⌘S si perdono alla chiusura.
- Le miniature dei RAW usano la preview incorporata senza applicare l'orientamento EXIF.
- Mancano ritaglio, correzione lenti, maschere e i pannelli Ottica/Geometria.
