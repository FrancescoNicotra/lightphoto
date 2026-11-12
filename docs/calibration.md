# Banco di taratura: uno slider alla volta contro Lightroom

Le intensità degli slider della pipeline sono state tarate tutte insieme su un solo render di Lightroom (la JPEG della GX80 con il preset "Dreamy days in spring"). Otto costanti adattate insieme possono compensarsi a vicenda: un errore nel Contrasto può nascondersi dietro un errore nella Saturazione HSL. Il banco [`tools/calibrate`](../tools/calibrate) misura invece **ogni slider da solo**, ciascuno contro il proprio export di Lightroom.

Questa guida dice quali export fare dal telefono, come esportarli, dove metterli e come leggere i risultati.

## Perché dal telefono e sulle JPEG

Lightroom per iPhone/Android nella versione **gratuita** modifica ed esporta le JPEG senza limiti; i RAW invece sono bloccati. Per questo tutti i riferimenti partono dalla stessa JPEG della fotocamera:

```
samples/pixls/P1010041-camera.jpg
```

È l'anteprima incorporata nel RW2 della Panasonic GX80 (`lumen-cli preview`), la stessa usata per la taratura esistente. Il file è nella cartella `samples/pixls/` del progetto, che git ignora.

## 1. Portare la foto sul telefono

1. Manda `P1010041-camera.jpg` al telefono **come file** (AirDrop → «Salva in File», oppure iCloud Drive). Evita di passare da WhatsApp, Telegram o simili: ricomprimono la foto.
2. In Lightroom: **+** (aggiungi foto) → **Da File** → scegli `P1010041-camera.jpg`.
3. Apri la foto in modifica e controlla che **tutto sia a zero**: nessun preset, nessun profilo cambiato, nessuna correzione automatica. Se la foto mostra già delle modifiche, usa **Ripristina → Tutto** (menu «…» in alto o pulsante Ripristina in fondo ai pannelli; il nome può variare con la versione).

## 2. Regole valide per ogni export

- **Un solo slider diverso da zero per volta.** Tutti gli altri restano al valore iniziale.
- **Grana 0** in ogni export (pannello Effetti). La grana è casuale: non si può confrontare pixel per pixel.
- Imposta il valore **esatto** indicato nella tabella e controlla il numero mostrato accanto allo slider (es. `+50`, non `+49`). Trascinare lentamente con il dito, o toccare il numero quando la versione lo permette, aiuta.
- Dopo l'export **riporta lo slider a zero** prima del successivo: doppio tocco sullo slider, oppure Ripristina → Modifiche / Tutto.
- Se uno slider è bloccato nella versione gratuita (icona Premium), saltalo: la sua voce resta «mancante» e il banco la segnala senza errore.

## 3. Impostazioni di export

Dalla foto modificata: icona **Condividi** (quadrato con freccia) → **Esporta come…** e imposta:

| Impostazione | Valore |
|---|---|
| Tipo di file | **JPG** |
| Dimensioni | **Massima disponibile** (dimensione originale) |
| Qualità | **100%** |
| Filigrana | **disattivata** |
| Altre opzioni → Spazio colore | **sRGB** |
| Altre opzioni → Nitidezza output (Aumenta nitidezza per) | **nessuna / disattivata** |
| Altre opzioni → Metadati | indifferente |
| Altre opzioni → Nome file | quello della tabella sotto, senza `.jpg` (oppure rinomina dopo) |

Poi **Salva in File** (non nel Rullino, che può convertire il formato), e a fine sessione trasferisci tutti i file sul Mac con AirDrop.

## 4. Gli export da fare

Mettili tutti in **`samples/pixls/lightroom/`**, con esattamente questi nomi. I nomi usano `p` per più e `m` per meno: `contrast-m50` = Contrasto −50.

### Controllo (da fare per primo)

| Pannello | Slider | Valore | File |
|---|---|---|---|
| — | nessuna modifica | — | `P1010041-original.jpg` |

Misura quanto cambia la foto il solo passaggio da Lightroom (decodifica, ricompressione JPEG). È il pavimento di tutte le altre voci: nessuno slider può fare meglio di così.

### Luce

| Slider | Valore | Chiave `crs:` | File |
|---|---|---|---|
| Esposizione | −1,00 | `Exposure2012=-1` | `P1010041-exposure-m100.jpg` |
| Esposizione | +1,00 | `Exposure2012=+1` | `P1010041-exposure-p100.jpg` |
| Contrasto | −50 | `Contrast2012=-50` | `P1010041-contrast-m50.jpg` |
| Contrasto | +50 | `Contrast2012=+50` | `P1010041-contrast-p50.jpg` |
| Luci | −50 | `Highlights2012=-50` | `P1010041-highlights-m50.jpg` |
| Luci | +50 | `Highlights2012=+50` | `P1010041-highlights-p50.jpg` |
| Ombre | −50 | `Shadows2012=-50` | `P1010041-shadows-m50.jpg` |
| Ombre | +50 | `Shadows2012=+50` | `P1010041-shadows-p50.jpg` |
| Bianchi | −50 | `Whites2012=-50` | `P1010041-whites-m50.jpg` |
| Bianchi | +50 | `Whites2012=+50` | `P1010041-whites-p50.jpg` |
| Neri | −50 | `Blacks2012=-50` | `P1010041-blacks-m50.jpg` |
| Neri | +50 | `Blacks2012=+50` | `P1010041-blacks-p50.jpg` |

### Colore

| Slider | Valore | Chiave `crs:` | File |
|---|---|---|---|
| Temperatura | +50 | `IncrementalTemperature=+50` | `P1010041-temperature-p50.jpg` |
| Tinta | +50 | `IncrementalTint=+50` | `P1010041-tint-p50.jpg` |
| Vividezza | +50 | `Vibrance=+50` | `P1010041-vibrance-p50.jpg` |
| Saturazione | −50 | `Saturation=-50` | `P1010041-saturation-m50.jpg` |

Su una JPEG Temperatura e Tinta vanno da −100 a +100 (non in Kelvin).

### Mix colori (HSL)

Pannello Colore → **Mix** (Mix colori). Scegli il pallino del colore, poi Tonalità, Saturazione o Luminanza.

| Colore | Slider | Valore | Chiave `crs:` | File |
|---|---|---|---|---|
| Verdi | Tonalità | −50 | `HueAdjustmentGreen=-50` | `P1010041-green-hue-m50.jpg` |
| Verdi | Tonalità | +50 | `HueAdjustmentGreen=+50` | `P1010041-green-hue-p50.jpg` |
| Verdi | Saturazione | −50 | `SaturationAdjustmentGreen=-50` | `P1010041-green-sat-m50.jpg` |
| Verdi | Saturazione | +50 | `SaturationAdjustmentGreen=+50` | `P1010041-green-sat-p50.jpg` |
| Verdi | Luminanza | −50 | `LuminanceAdjustmentGreen=-50` | `P1010041-green-lum-m50.jpg` |
| Verdi | Luminanza | +50 | `LuminanceAdjustmentGreen=+50` | `P1010041-green-lum-p50.jpg` |
| Gialli | Tonalità | −50 | `HueAdjustmentYellow=-50` | `P1010041-yellow-hue-m50.jpg` |
| Gialli | Tonalità | +50 | `HueAdjustmentYellow=+50` | `P1010041-yellow-hue-p50.jpg` |
| Gialli | Saturazione | −50 | `SaturationAdjustmentYellow=-50` | `P1010041-yellow-sat-m50.jpg` |
| Gialli | Saturazione | +50 | `SaturationAdjustmentYellow=+50` | `P1010041-yellow-sat-p50.jpg` |
| Gialli | Luminanza | −50 | `LuminanceAdjustmentYellow=-50` | `P1010041-yellow-lum-m50.jpg` |
| Gialli | Luminanza | +50 | `LuminanceAdjustmentYellow=+50` | `P1010041-yellow-lum-p50.jpg` |

Verdi e gialli sono i colori dominanti di questa foto, quindi quelli che si misurano meglio.

### Color grading

Pannello Colore → **Color grading** (o Gradazione colore). Scegli la ruota, poi apri i cursori sotto la ruota per impostare Tonalità e Saturazione con precisione. Fusione (50), Bilanciamento (0) e Luminanza (0) restano ai valori iniziali.

| Ruota | Tonalità | Saturazione | Chiavi `crs:` | File |
|---|---|---|---|---|
| Mezzitoni | 60 | 50 | `ColorGradeMidtoneHue=60`, `ColorGradeMidtoneSat=50` | `P1010041-grade-midtones.jpg` |
| Ombre | 220 | 50 | `SplitToningShadowHue=220`, `SplitToningShadowSaturation=50` | `P1010041-grade-shadows.jpg` |
| Luci | 40 | 50 | `SplitToningHighlightHue=40`, `SplitToningHighlightSaturation=50` | `P1010041-grade-highlights.jpg` |

### Effetti

| Slider | Valore | Chiave `crs:` | File |
|---|---|---|---|
| Texture | −50 | `Texture=-50` | `P1010041-texture-m50.jpg` |
| Texture | +50 | `Texture=+50` | `P1010041-texture-p50.jpg` |
| Chiarezza | −50 | `Clarity2012=-50` | `P1010041-clarity-m50.jpg` |
| Chiarezza | +50 | `Clarity2012=+50` | `P1010041-clarity-p50.jpg` |
| Rimuovi foschia | −50 | `Dehaze=-50` | `P1010041-dehaze-m50.jpg` |
| Rimuovi foschia | +50 | `Dehaze=+50` | `P1010041-dehaze-p50.jpg` |

In tutto **39 export** (38 slider più il controllo). Non serve farli tutti in una volta: il banco misura quelli presenti e segnala gli altri come mancanti.

## 5. Eseguire il banco

```bash
cargo run --release -p calibrate
```

Opzioni:

- `--only TESTO`: solo le voci il cui nome contiene TESTO (es. `--only contrast`).
- `--bands`: per ogni voce, l'errore per fasce di luminosità del riferimento (utile per Luci, Ombre, Bianchi, Neri).
- `--samples CARTELLA`: dove cercare i file. Senza, il banco usa `$LUMEN_SAMPLES`, poi la prima cartella `samples/` con dentro `pixls/` risalendo dal progetto (così funziona anche da un worktree git, che non ha `samples/pixls`).
- `--manifest FILE`: un altro manifesto (predefinito [`tools/calibrate/references.toml`](../tools/calibrate/references.toml)).

Esempio di uscita:

```
entry                          ΔE median    p95 neutral     ΔL     Δa     Δb    max  result
dreamy                       2.84   2.50   5.96   13.01   -1.0   -0.6   +0.6   3.00  ok

1 measured, 1 passed, 0 above threshold, 38 missing
```

| Colonna | Significato |
|---|---|
| ΔE, median, p95 | ΔE2000 medio, mediano e al 95° percentile tra il nostro render e l'export di Lightroom, a 480 px sul lato lungo |
| neutral | ΔE tra la foto **senza modifiche** e l'export di Lightroom: quanto lo slider ha cambiato la foto. Un ΔE molto più basso di questo vuol dire che seguiamo bene Lightroom; uno vicino vuol dire che lo slider fa poco o va nella direzione sbagliata |
| ΔL, Δa, Δb | media di (nostro − Lightroom) in Lab: segno e direzione dell'errore (ΔL > 0 = noi più chiari, Δb > 0 = noi più gialli) |
| max | soglia della voce |
| result | `ok`, `FAIL` (sopra soglia) o `ERROR` |

La metrica è la stessa di `cargo run -p compare -- --size 480` e del test `matches_lightroom_on_the_camera_jpeg`: entrambe le immagini ridotte a 480 px (media per area, che toglie grana e nitidezza), colori portati in sRGB e tagliati a [0, 1] come in un file esportato, ΔE2000 da `raw::color`.

Il comando esce con errore se una voce misurata supera la soglia o non si può sviluppare; le voci con file mancanti danno solo un avviso. Così può girare in CI.

## 6. Soglie

Nel manifesto `max_delta_e = 3.0` vale per tutte le voci che non ne dichiarano una propria. Per le voci non ancora misurate è **provvisoria**: dopo il primo giro con gli export veri, fissa per ogni voce una soglia poco sopra il valore misurato (es. misurato + 0,3), così un peggioramento futuro fa fallire il banco. Il controllo `original` ha già una soglia stretta (1,0): se la supera, il problema è nel percorso JPEG → Lightroom → JPEG e non negli slider.

## 7. Aggiungere una voce

Il manifesto è [`tools/calibrate/references.toml`](../tools/calibrate/references.toml). Ogni voce:

```toml
[[reference]]
name = "contrast-m50"                             # nome univoco
source = "pixls/P1010041-camera.jpg"              # relativo alla cartella samples
kind = "jpeg"                                     # "jpeg" (JPEG/PNG/TIFF) oppure "raw"
preset = "Dreamy days in spring.xmp"              # facoltativo: un preset .xmp/.dng
settings = { Contrast2012 = -50 }                 # facoltativo: chiavi crs: senza prefisso
reference = "pixls/lightroom/P1010041-contrast-m50.jpg"
max_delta_e = 2.0                                 # facoltativo
note = "come è stato fatto il riferimento"        # facoltativo
```

Le impostazioni partono dai valori iniziali di Camera Raw per il tipo di sorgente (su una JPEG niente nitidezza e niente riduzione del rumore colore, come in Lightroom), poi si applica il preset, poi `settings`. Una chiave sconosciuta (es. `Contrast` invece di `Contrast2012`) è rifiutata subito, così un errore di battitura non misura una foto non modificata. I nomi delle chiavi sono quelli dei file `.xmp` di Lightroom; `cargo run -p xmp-dump -- preset.xmp` li mostra.

`kind = "raw"` è già supportato, per quando ci saranno un RAW e un suo render di Lightroom (oggi non disponibili gratis).

## Limiti

- Una sola foto: verde e giallo dominano, rosso, blu e magenta quasi assenti. Per l'HSL degli altri colori servirà una seconda foto con quei colori.
- Valori a ±50: dicono la forza a metà corsa, non la forma della curva. Se una voce va male, un export a ±100 dello stesso slider aiuta a capire se sbagliamo la forza o la forma.
- Le JPEG sono già sviluppate dalla fotocamera: Luci e Ombre su un RAW con molta gamma dinamica si comportano diversamente (vedi S8 nel piano).
- Il banco misura soltanto; le costanti sono scritte nel codice della pipeline e si correggono a mano.
