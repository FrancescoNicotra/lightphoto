# Samples for the tests

Small crops of one photo, versioned so that the tests on real images run on
every machine and in CI. Built by `cargo run -p raw --example make_ci_samples`
from the full files in `samples/pixls/` (not versioned); the crop rectangles
are in that example.

| File | What it is |
|---|---|
| `P1010041-crop.dng` | 1024 × 768 photosites of [P1010041.RW2](https://raw.pixls.us/data/Panasonic/DMC-GX80/P1010041.RW2) (Panasonic DMC-GX80, raw.pixls.us, **CC0**) from (1696, 1328), as a Bayer DNG: the values unchanged (Lossless JPEG), with the RW2's black and white levels, colour matrices, as-shot white balance and EXIF (maker notes dropped); the lens correction of the RW2's maker notes is written as an OpcodeList3 WarpRectilinear for the crop. Its preview is the same scene in the camera JPEG. |
| `P1010041-camera-crop.jpg` | 800 × 600 from (560, 400) of the camera's JPEG embedded in the RW2 (1920 × 1440), re-encoded at quality 92. CC0 like the RAW. |
| `P1010041-camera-lightroom-dreamy-crop.jpg` | The same region of that JPEG developed by Lightroom 12 for iOS with `samples/Dreamy days in spring.xmp`, exported as sRGB JPEG. Of its metadata only `xmp:CreatorTool` is kept. |

The RAW crop and the JPEG crops are not the same region; once its lens
distortion is corrected, the RAW crop lands on the JPEG crop from about
(146, 152) at a scale of 1920 / 4592 (`crates/raw/tests/lens_framing.rs`).
