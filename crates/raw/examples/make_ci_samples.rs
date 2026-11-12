//! Builds the small versioned samples in `samples/ci/` from the full files in
//! `samples/pixls/` (not versioned), so that the tests on real images run on
//! every machine and in CI:
//!
//! ```text
//! cargo run -p raw --example make_ci_samples
//! ```
//!
//! - `P1010041-crop.dng`: a crop of the GX80 RW2 (raw.pixls.us, CC0) as a
//!   Bayer DNG. The photosites are copied unchanged (Lossless JPEG, the same
//!   black and white levels, matrices, white balance and EXIF), only fewer
//!   of them; the embedded preview is the same crop of the camera JPEG.
//! - `P1010041-camera-crop.jpg`: the same region of the camera JPEG, at its
//!   own scale (1920 px wide for the whole frame).
//! - `P1010041-camera-lightroom-dreamy-crop.jpg`: the same region of
//!   Lightroom's render of that JPEG with the "Dreamy days in spring" preset.
//!   Only `xmp:CreatorTool` is kept of its metadata.
//!
//! The crops are fixed here: changing them means re-tuning the tests in
//! `crates/raw/tests/samples.rs` and `crates/pipeline/tests/samples.rs`.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, GenericImageView};
use rawler::decoders::RawDecodeParams;
use rawler::dng::writer::DngWriter;
use rawler::dng::{CropMode, DngCompression, DngPhotometricConversion, DNG_VERSION_V1_4};
use rawler::rawsource::RawSource;
use rawler::tags::{ExifTag, TiffCommonTag};
use rawler::RawImageData;

/// Sensor crop of the RW2, in photosites: x, y, width, height. Even origin so
/// that the Bayer phase (BGGR) is unchanged. The rocks in the pond, the
/// playground and the trees behind it.
const RAW_CROP: [usize; 4] = [1696, 1328, 1024, 768];
/// The same scene in the 1920 × 1440 camera JPEG, for the DNG's preview.
/// Measured on the rocks, not scaled from [`RAW_CROP`]: the camera corrects
/// the lens distortion in its JPEG, so the centre is about 5 % smaller there.
const PREVIEW_CROP: [u32; 4] = [706, 557, 408, 306];
/// Crop of the 1920 × 1440 camera JPEG and of Lightroom's render of it.
const JPEG_CROP: [u32; 4] = [560, 400, 800, 600];
const JPEG_QUALITY: u8 = 92;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples");
    let (src, dst) = (root.join("pixls"), root.join("ci"));
    std::fs::create_dir_all(&dst)?;

    crop_raw(&src.join("P1010041.RW2"), &src.join("P1010041-camera.jpg"), &dst.join("P1010041-crop.dng"))?;
    let camera = image::open(src.join("P1010041-camera.jpg"))?;
    write_jpeg(&crop(&camera, JPEG_CROP), &dst.join("P1010041-camera-crop.jpg"), None)?;
    let lightroom = image::open(src.join("P1010041-camera-lightroom-dreamy.jpg"))?;
    let xmp = minimal_xmp("Adobe Lightroom 12.0.0 (iOS)");
    write_jpeg(&crop(&lightroom, JPEG_CROP), &dst.join("P1010041-camera-lightroom-dreamy-crop.jpg"), Some(&xmp))?;

    for entry in std::fs::read_dir(&dst)? {
        let entry = entry?;
        println!("{:>9} B  {}", entry.metadata()?.len(), entry.path().display());
    }
    Ok(())
}

fn crop(img: &DynamicImage, [x, y, w, h]: [u32; 4]) -> DynamicImage {
    assert!(x + w <= img.width() && y + h <= img.height());
    img.view(x, y, w, h).to_image().into()
}

/// The photosites in [`RAW_CROP`] with the file's metadata, as a DNG.
fn crop_raw(rw2: &Path, camera_jpeg: &Path, out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let source = RawSource::new(rw2)?;
    let decoder = rawler::get_decoder(&source)?;
    let params = RawDecodeParams::default();
    let mut raw = decoder.raw_image(&source, &params, false)?;
    let metadata = decoder.raw_metadata(&source, &params)?;
    let [x0, y0, w, h] = RAW_CROP;
    assert!(x0 % 2 == 0 && y0 % 2 == 0, "odd crop origin changes the CFA phase");
    assert_eq!(raw.cpp, 1, "Bayer data expected");
    let (cfa_w, cfa_h) = (raw.blacklevel.width.max(1), raw.blacklevel.height.max(1));
    assert!(x0 % cfa_w == 0 && y0 % cfa_h == 0, "the black level pattern would shift");
    // Inside the area with image data: the default crop if any, else the active area.
    let area = raw.crop_area.or(raw.active_area);
    if let Some(a) = area {
        assert!(x0 >= a.p.x && y0 >= a.p.y && x0 + w <= a.p.x + a.d.w && y0 + h <= a.p.y + a.d.h, "crop outside {a:?}");
    }

    let RawImageData::Integer(data) = &raw.data else { return Err("integer raw data expected".into()) };
    let stride = raw.width;
    let cropped: Vec<u16> = (y0..y0 + h).flat_map(|y| data[y * stride + x0..y * stride + x0 + w].iter().copied()).collect();
    raw.data = RawImageData::Integer(cropped);
    raw.width = w;
    raw.height = h;
    raw.active_area = None;
    raw.crop_area = None;
    raw.blackareas.clear();

    let preview = crop(&image::open(camera_jpeg)?, PREVIEW_CROP);

    let mut file = BufWriter::new(File::create(out)?);
    let mut dng = DngWriter::new(&mut file, DNG_VERSION_V1_4)?;
    let mut sub = dng.subframe(0);
    sub.raw_image(&raw, CropMode::Best, DngCompression::Lossless, DngPhotometricConversion::Original, 1)?;
    sub.finalize()?;
    let mut sub = dng.subframe(1);
    sub.preview(&preview, 0.9)?;
    sub.finalize()?;
    dng.thumbnail(&preview)?;
    dng.load_base_tags(&raw)?;
    dng.load_metadata(&metadata)?;
    if !dng.root_ifd().contains(ExifTag::Orientation) {
        dng.root_ifd_mut().add_tag(ExifTag::Orientation, raw.orientation.to_u16());
    }
    dng.root_ifd_mut().add_tag(TiffCommonTag::Software, "lumen make_ci_samples");
    // Drop the maker notes: they describe the whole frame.
    dng.exif_ifd_mut().remove_tag(ExifTag::MakerNotes);
    dng.close()?;
    Ok(())
}

fn write_jpeg(img: &DynamicImage, out: &Path, xmp: Option<&[u8]>) -> Result<(), Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&img.to_rgb8())?;
    if let Some(xmp) = xmp {
        // An APP1 XMP segment right after SOI.
        let mut seg = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        seg.extend_from_slice(xmp);
        let len = u16::try_from(seg.len() + 2)?;
        let mut with = bytes[..2].to_vec();
        with.extend_from_slice(&[0xFF, 0xE1]);
        with.extend_from_slice(&len.to_be_bytes());
        with.extend_from_slice(&seg);
        with.extend_from_slice(&bytes[2..]);
        bytes = with;
    }
    std::fs::write(out, bytes)?;
    Ok(())
}

fn minimal_xmp(creator_tool: &str) -> Vec<u8> {
    format!(
        r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:CreatorTool="{creator_tool}"/>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="r"?>"#
    )
    .into_bytes()
}
