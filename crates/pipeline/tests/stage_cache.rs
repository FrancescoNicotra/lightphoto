//! The stage cache must give exactly the image an uncached render gives,
//! and recompute only the stages whose settings changed.

use pipeline::{Pipeline, Recomputed, RenderCache, RenderOptions, SourceKind};
use raw::{ColorSource, Image, Photo, PhotoInfo};
use xmp::DevelopSettings;

/// A rendered-image photo with smooth gradients, texture and some colour.
fn photo() -> Photo {
    let data = Image::from_fn(160, 120, |x, y| {
        let t = ((x * 7 + y * 13) % 11) as f32 / 110.0;
        [0.05 + 0.6 * x as f32 / 160.0 + t, 0.1 + 0.5 * y as f32 / 120.0, 0.3 + t]
    });
    Photo { info: PhotoInfo::default(), data, color: ColorSource::Working }
}

fn render(p: &Photo, s: &DevelopSettings, cache: &mut RenderCache) -> (Image, Recomputed) {
    let pipeline = Pipeline::new(s, SourceKind::of(p));
    let img = pipeline.render_cached(p, &RenderOptions::default(), cache);
    // Same result as a render from scratch, to the bit.
    assert_eq!(img, pipeline.render(p, &RenderOptions::default()));
    (img, cache.last)
}

const ALL: Recomputed = Recomputed { working: true, tone: true, spatial: true, colour: true };
const NONE: Recomputed = Recomputed { working: false, tone: false, spatial: false, colour: false };

#[test]
fn recomputes_only_what_changed() {
    let p = photo();
    let mut cache = RenderCache::new();
    let mut s = DevelopSettings::default();
    s.presence.texture = 20.0;
    assert_eq!(render(&p, &s, &mut cache).1, ALL);
    assert_eq!(render(&p, &s, &mut cache).1, NONE);

    s.hsl.saturation[3] = -40.0;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { colour: true, ..NONE });
    s.presence.vibrance = 25.0;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { colour: true, ..NONE });
    s.color_grade.midtone.sat = 30.0;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { colour: true, ..NONE });

    s.detail.sharpness = 80.0;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { spatial: true, colour: true, ..NONE });
    s.presence.clarity = -20.0;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { spatial: true, colour: true, ..NONE });

    s.basic.exposure = 0.4;
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { tone: true, spatial: true, colour: true, ..NONE });
    s.curves.master = xmp::Curve(vec![(0.0, 10.0), (255.0, 245.0)]);
    assert_eq!(render(&p, &s, &mut cache).1, Recomputed { tone: true, spatial: true, colour: true, ..NONE });

    // Grain is added after the render: nothing to redo.
    s.effects.grain_amount = 40.0;
    assert_eq!(render(&p, &s, &mut cache).1, NONE);

    s.crop.has_crop = true;
    s.crop.left = 0.1;
    assert_eq!(render(&p, &s, &mut cache).1, ALL);
    assert!(cache.memory() > 0);
}

#[test]
fn a_different_photo_is_never_served_from_the_cache() {
    let (a, mut b) = (photo(), photo());
    b.data.pixels[60 * 160 + 80] = [0.9, 0.1, 0.1];
    let s = DevelopSettings::default();
    let mut cache = RenderCache::new();
    let (img_a, _) = render(&a, &s, &mut cache);
    let (img_b, last) = render(&b, &s, &mut cache);
    assert!(last.working);
    assert_ne!(img_a, img_b);
    // Same pixels, different size of render: also a miss.
    let pipeline = Pipeline::new(&s, SourceKind::of(&a));
    pipeline.render_cached(&a, &RenderOptions { max_size: Some(80), ..Default::default() }, &mut cache);
    assert!(cache.last.working);
}

#[test]
fn owned_render_matches_borrowed_render() {
    let p = photo();
    let mut s = DevelopSettings::default();
    s.presence.clarity = 30.0;
    s.crop.has_crop = true;
    s.crop.left = 0.1;
    s.crop.bottom = 0.8;
    let pipeline = Pipeline::new(&s, SourceKind::of(&p));
    let opts = RenderOptions::default();
    assert_eq!(pipeline.render_owned(p.clone(), &opts), pipeline.render(&p, &opts));
}
