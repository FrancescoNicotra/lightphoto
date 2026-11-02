# Formato XMP di Camera Raw / Lightroom: note di reverse engineering

Queste note vengono dall'analisi di `samples/Dreamy days in spring.xmp` e del relativo `... - mobile version.dng`. Il codice che le implementa è nel crate [`xmp`](../crates/xmp).

## 1. Struttura del pacchetto

```
x:xmpmeta
 └ rdf:RDF
    └ rdf:Description  (una o più; descrivono tutte la stessa risorsa)
        attributi  crs:Exposure2012="+0.50" ...      ← valori semplici
        elementi   <crs:ToneCurvePV2012><rdf:Seq>…   ← array, lang-alt, struct
```

- Namespace delle impostazioni: `crs` = `http://ns.adobe.com/camera-raw-settings/1.0/`.
- Un valore semplice può essere scritto come **attributo** oppure come **elemento figlio**. Il parser accetta entrambe le forme.
- I numeri sono testo, spesso con segno esplicito: `"+50"`, `"+1.0"`, `"0.00"`.
- Ci sono quattro forme di valore complesso:
  - `rdf:Seq`: curve, `PointColors`.
  - `rdf:Alt`: `Name`, `Group`, `Description`, con `xml:lang="x-default"`.
  - Struct: `crs:Look` contiene un `rdf:Description` annidato.
  - Struct abbreviata: `<crs:LensBlur crs:Version="1" …/>`, attributi su un elemento vuoto.

> Bug del vecchio `xmp_to_lut.py`: leggeva solo gli attributi, quindi curve e nome non venivano mai letti. In più cercava le curve separate da `;`.

## 2. I preset sono parziali

Un preset contiene solo le chiavi che vuole impostare. Si applica così: `impostazioni correnti` + chiavi presenti nel preset. È quello che fa `DevelopSettings::apply`.

## 3. Mappa delle chiavi (PV2012, ProcessVersion 11+/15.4)

| Pannello | Chiavi XMP | Range / note |
|---|---|---|
| WB | `WhiteBalance` (`As Shot`/`Auto`/`Custom`/nome), `Temperature` (K), `Tint` | assoluti, **solo RAW** |
| WB non-RAW | `IncrementalTemperature`, `IncrementalTint` | −100..100 |
| Base | `Exposure2012` (EV ±5), `Contrast2012`, `Highlights2012`, `Shadows2012`, `Whites2012`, `Blacks2012` | −100..100 |
| Presenza | `Texture`, `Clarity2012`, `Dehaze`, `Vibrance`, `Saturation` | −100..100 |
| Curva parametrica | `ParametricShadows/Darks/Lights/Highlights`, `Parametric{Shadow,Midtone,Highlight}Split` | split 25/50/75 di default |
| Curva a punti | `ToneCurvePV2012`, `…Red`, `…Green`, `…Blue`, `ToneCurveName2012` | `rdf:Seq` di `"x, y"` in 0..255 (spazio gamma) |
| HSL | `{Hue,Saturation,Luminance}Adjustment{Red,Orange,Yellow,Green,Aqua,Blue,Purple,Magenta}` | −100..100 |
| Color grading | ombre/luci: `SplitToning{Shadow,Highlight}{Hue,Saturation}`, `SplitToningBalance`; mezzitoni: `ColorGradeMidtone{Hue,Sat,Lum}`; `ColorGrade{Shadow,Highlight}Lum`, `ColorGradeGlobal{Hue,Sat,Lum}`, `ColorGradeBlending` | hue 0..359, sat 0..100 |
| Calibrazione | `ShadowTint`, `{Red,Green,Blue}{Hue,Saturation}` | −100..100 |
| Dettaglio | `Sharpness`, `SharpenRadius`, `SharpenDetail`, `SharpenEdgeMasking`, `LuminanceSmoothing`, `ColorNoiseReduction*` | |
| Effetti | `GrainAmount`, `GrainSize`, `GrainFrequency` (= Roughness), `GrainSeed`, `PostCropVignette*` | |
| Profilo | `crs:Look` (struct), `CameraProfile`, `CameraProfileDigest` | vedi §4 |

Il color grading (PV5) **riusa le chiavi dello split toning** per tonalità e saturazione di ombre e luci. Le chiavi `ColorGrade*` esistono solo per mezzitoni, luminanze, globale e blending.

## 4. Profili, Look e LUT

### Nel `.xmp` del preset
```xml
<crs:Look><rdf:Description crs:Name="Adobe Color" crs:Amount="1"
     crs:UUID="B952C231111CD8E0ECCF14B86BAA7077" crs:Stubbed="true"> …
```
`Stubbed="true"` significa che il profilo è citato solo per nome e UUID, perché è integrato in Camera Raw.

### Nel `.dng` "mobile version" lo stesso Look è completo
```xml
<crs:Parameters><rdf:Description crs:CameraProfile="Adobe Standard"
     crs:LookTable="E1095149FDB39D7A057BAB208837E2E1">
  <crs:ToneCurvePV2012> (0,0) (22,16) (40,35) (127,127) (224,230) (240,246) (255,255)
```
Quindi **"Adobe Color" = profilo camera "Adobe Standard" + una tabella HSV interna di Adobe (`LookTable`, solo hash) + una S-curve leggera**. La curva si può applicare esattamente. La LookTable non è nel file: si sostituisce con un'approssimazione o con i file di Camera Raw, se installato.

### Profili creativi con LUT incorporata (quelli creati da Photoshop con una .cube)
```xml
crs:RGBTable="<md5>"   crs:Table_<md5>="<blob>"
```
Si trovano dentro `crs:Look/crs:Parameters` nei preset, oppure al livello principale nei preset di tipo `PresetType="Look"`.

Decodifica del blob, implementata in `crates/xmp/src/rgb_table.rs` e verificata contro il layout del DNG SDK (`dng_big_table`):

1. **Base85 Adobe**. Alfabeto ``0-9 a-z A-Z . - : + = ^ ! / * ? ` ' | ( ) [ ] { } @ % $ #``. 5 caratteri formano un `u32` little-endian, con il primo carattere meno significativo. Un gruppo finale di n caratteri vale n−1 byte.
2. `u32 LE` con la lunghezza non compressa, seguito da uno stream **zlib**.
3. Contenuto binario, tutto little-endian:
   - `u32 tipo` (1 = RGB table), `u32 versione` (1);
   - `u32 dimensioni` (1 o 3), `u32 divisioni` (es. 32);
   - campioni `u16 ×3` come **delta dalla rampa identità** `nop[i] = (i·0xFFFF + d/2) / (d−1)`, con aritmetica modulo 2¹⁶. Ordine 3D: r esterno, g, b interno;
   - `u32 primarie` (0 sRGB, 1 AdobeRGB, 2 ProPhoto, 3 P3, 4 Rec2020), `u32 gamma` (0 lineare, 1 sRGB, 2 1.8, 3 2.2, 4 Rec2020), `u32 gamut` (0 clip, 1 extend);
   - `f64 min amount`, `f64 max amount`, e un `u32 flags` opzionale.

I codici numerici delle primarie e della gamma vanno ancora confermati su un profilo reale esportato da Camera Raw. `xmp-dump --cube` estrae la tabella in un `.cube`.

## 5. Perché un preset XMP non diventa una LUT

| Operazione | Puntuale (LUT ok) | Note |
|---|---|---|
| Curve, HSL, color grading, calibrazione, vibranza/saturazione, profilo | sì | |
| Contrasto / Bianchi / Neri | sì (approssimazione) | |
| Luci / Ombre | in parte | in Adobe sono **locali** |
| WB `Temperature` assoluta | no | richiede i dati RAW e la matrice camera |
| Texture, Clarity, Dehaze, Sharpening, NR, Grain, Vignettatura | no | operazioni spaziali |

Per questo il preset va applicato nella pipeline RAW. L'export `.cube` di un preset parametrico (milestone M3) cattura solo la parte puntuale.

## 6. Chiavi viste ma non ancora mappate

`HDREditMode`, `CurveRefineSaturation`, `OverrideLookVignette`, `PointColors` (point color, `-1` = vuoto), `LensBlur`, `AsShotTemperature/Tint`, `GrainSeed`, chiavi di lente e prospettiva (`Perspective*`, `Defringe*`, `LensProfileEnable`), `CameraProfileDigest`. `xmp-dump` le elenca nella sezione *Not mapped yet*.
