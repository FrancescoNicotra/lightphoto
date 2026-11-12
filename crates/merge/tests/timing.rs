//! Writes a synthetic 24 MP hand-held bracket for timing the CLI:
//!
//! ```text
//! LUMEN_BRACKET_DIR=/some/dir cargo test --release -p merge --test timing -- --ignored --nocapture
//! lumen-cli hdr /some/dir/BRK_0.dng /some/dir/BRK_1.dng /some/dir/BRK_2.dng -o out.dng --timings
//! ```

mod common;

use align::Transform;
use common::scene::{Mover, Scene, ShotSpec};

#[test]
#[ignore = "writes ~150 MB; timing only"]
fn write_24mp_bracket() {
    let dir = std::path::PathBuf::from(std::env::var("LUMEN_BRACKET_DIR").expect("LUMEN_BRACKET_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (6000, 4000);
    let mut scene = Scene::new(31, w, h);
    // A person-sized dark disk walking across the frame.
    scene.mover = Some(Mover { radius: 350.0, level: 0.01 });
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let walk = |i: usize| Some((1600.0 + 450.0 * i as f64, 1700.0));
    let specs = [
        ShotSpec { exif_time: Some(1.0 / 400.0), transform: Transform::translation(-9.3, 14.6), ..ShotSpec::new(0.25, 1) },
        ShotSpec { exif_time: Some(1.0 / 100.0), ..ShotSpec::new(1.0, 2) },
        ShotSpec { exif_time: Some(1.0 / 25.0), transform: Transform::similarity(1.001, 0.25f64.to_radians(), cx, cy, 12.0, -6.5), ..ShotSpec::new(4.0, 3) },
    ];
    for (i, s) in specs.iter().enumerate() {
        let s = &ShotSpec { mover: walk(i), ..s.clone() };
        let t = std::time::Instant::now();
        let p = dir.join(format!("BRK_{i}.dng"));
        common::write_dng(&p, &scene, s, w, h);
        eprintln!("wrote {} in {:?}", p.display(), t.elapsed());
    }
}

/// Peak of live heap bytes, to tell the merge's own memory from the
/// allocator's caching (`/usr/bin/time -l` reports the latter too).
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = unsafe { System.alloc(l) };
            if !p.is_null() {
                let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) };
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        }
    }
}

#[global_allocator]
static ALLOC: counting::Counting = counting::Counting;

#[test]
#[ignore = "needs the bracket written by write_24mp_bracket"]
fn peak_heap_of_a_24mp_merge() {
    use std::sync::atomic::Ordering;
    let dir = std::path::PathBuf::from(std::env::var("LUMEN_BRACKET_DIR").expect("LUMEN_BRACKET_DIR"));
    let sources = (0..3).map(|i| merge::Source::File(dir.join(format!("BRK_{i}.dng")))).collect();
    let peak = |what: &str| {
        let p = counting::PEAK.swap(counting::LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
        eprintln!("{what}: peak {:.0} MB, live {:.0} MB", p as f64 / 1e6, counting::LIVE.load(Ordering::Relaxed) as f64 / 1e6);
    };
    let opts = merge::MergeOptions { deghost: merge::Deghost::High, ..merge::MergeOptions::default() };
    let ctl = merge::Control::default();
    let prepared = merge::prepare(sources, &opts, &ctl).unwrap();
    peak("pass 1");
    let mut merged = prepared.merge(&ctl).unwrap();
    peak("pass 2");
    merged.write_dng(&dir.join("peak.dng"), &opts, &ctl).unwrap();
    peak("write");
}
