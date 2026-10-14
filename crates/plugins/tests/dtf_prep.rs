//! DTF Prep's committed release module exercised through the real PhotoCraft ABI v1 host.

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_geom::Rect;
use photocraft_plugins::{Area, Error, Limits, Plugin};
use photocraft_raster::Surface;
use serde_json::{Value, json};

const DTF_PREP: &[u8] = include_bytes!("../../../dist/dtf-prep.wasm");

fn rgba_surface(width: i32, height: i32) -> Surface {
    let mut surface = Surface::new(PixelFormat::RGBA32F);
    for y in 0..height {
        for x in 0..width {
            let alpha = 0.08 + ((x * 17 + y * 29) % 89) as f32 / 100.0;
            surface.write_pixel(x, y, &[0.15 + x as f32 / width as f32 * 0.7, 0.2 + y as f32 / height as f32 * 0.6, 0.55, alpha]);
        }
    }
    surface
}

fn apply(plugin: &Plugin, surface: &Surface, width: i32, height: i32, params: Value) -> Surface {
    plugin.apply(surface, Rect::new(0, 0, width, height), None, &params).expect("DTF Prep run")
}

#[test]
fn release_module_loads_and_exposes_the_operator_dialog() {
    let plugin = Plugin::load(DTF_PREP, Limits::default()).expect("valid DTF Prep module");
    let manifest = plugin.manifest();
    assert_eq!(manifest.id, "cz.optimaprint.dtfprep");
    assert_eq!(manifest.name, "DTF Prep");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.overlap, 2);
    assert_eq!(manifest.area, Area::Content);
    assert!(manifest.description.contains("hard-alpha"));
    let names: Vec<&str> = manifest.params.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "alphaThreshold",
            "processingMode",
            "pattern",
            "cellSize",
            "angle",
            "ventilationPercent",
            "knockout",
            "customR",
            "customG",
            "customB",
            "knockoutTolerance",
            "shadowColorBoost",
            "speckCleanup",
        ]
    );
    assert!(plugin.size() < 32 * 1024 * 1024);
}

#[test]
fn threshold_hard_alpha_hidden_rgb_and_soft_opaque_contract_hold() {
    let plugin = Plugin::load(DTF_PREP, Limits::default()).unwrap();
    let mut source = rgba_surface(96, 64);
    source.write_pixel(0, 0, &[0.9, 0.4, 0.2, 7.0 / 255.0]);
    source.write_pixel(1, 0, &[0.123, 0.456, 0.789, 1.0]);
    let opaque = source.pixel(1, 0);
    let output = apply(&plugin, &source, 96, 64, json!({"speckCleanup": "medium"}));

    assert_eq!(output.pixel(0, 0), vec![0.0, 0.0, 0.0, 0.0]);
    assert_eq!(output.pixel(1, 0), opaque, "soft mode protects opaque RGB and alpha");
    let mut holes = 0usize;
    let mut solids = 0usize;
    for y in 0..64 {
        for x in 0..96 {
            let pixel = output.pixel(x, y);
            let alpha = pixel[3];
            assert!(alpha == 0.0 || alpha == 1.0, "({x},{y}) has fractional alpha {alpha}");
            if alpha == 0.0 {
                holes += 1;
                assert_eq!(pixel, vec![0.0, 0.0, 0.0, 0.0], "({x},{y}) retains hidden RGB");
            } else {
                solids += 1;
            }
        }
    }
    assert!(holes > 0 && solids > 0, "partial alpha must become spatial binary coverage");
}

#[test]
fn full_artwork_patterns_angles_and_knockout_work() {
    let plugin = Plugin::load(DTF_PREP, Limits::default()).unwrap();
    let mut opaque = Surface::new(PixelFormat::RGBA32F);
    opaque.fill_rect(Rect::new(0, 0, 80, 64), &[0.25, 0.5, 0.75, 1.0]);
    let base = json!({
        "processingMode": "fullArtworkForcedHalftone",
        "ventilationPercent": 35,
        "cellSize": 8,
        "speckCleanup": "off"
    });
    let round_a = apply(&plugin, &opaque, 80, 64, base.clone());
    let round_b = apply(&plugin, &opaque, 80, 64, base.clone());
    assert_eq!(round_a, round_b, "round screen is deterministic");
    let holes = (0..64).flat_map(|y| (0..80).map(move |x| (x, y))).filter(|&(x, y)| round_a.pixel(x, y)[3] == 0.0).count();
    assert!(holes > 0 && holes < 80 * 64, "full artwork mode must ventilate opaque artwork");

    let lines_0 = apply(
        &plugin,
        &opaque,
        80,
        64,
        json!({"processingMode":"fullArtworkForcedHalftone","ventilationPercent":50,"pattern":"lines","cellSize":8,"angle":0,"speckCleanup":"off"}),
    );
    let lines_90 = apply(
        &plugin,
        &opaque,
        80,
        64,
        json!({"processingMode":"fullArtworkForcedHalftone","ventilationPercent":50,"pattern":"lines","cellSize":8,"angle":90,"speckCleanup":"off"}),
    );
    assert_ne!(lines_0, lines_90, "angle changes line orientation");
    assert_ne!(round_a, lines_0, "round and line screens differ");

    let mut colors = Surface::new(PixelFormat::RGBA32F);
    colors.write_pixel(0, 0, &[0.0, 0.0, 0.0, 1.0]);
    colors.write_pixel(1, 0, &[0.8, 0.1, 0.3, 1.0]);
    let knocked = apply(&plugin, &colors, 2, 1, json!({"knockout":"black","knockoutTolerance":32,"speckCleanup":"off"}));
    assert_eq!(knocked.pixel(0, 0), vec![0.0, 0.0, 0.0, 0.0]);
    assert_eq!(knocked.pixel(1, 0), colors.pixel(1, 0));
}

#[test]
fn strongest_cleanup_and_rotated_screen_are_seamless_across_real_host_bands() {
    let (width, height) = (900, 96);
    let source = rgba_surface(width, height);
    let params = json!({
        "processingMode": "fullArtworkForcedHalftone",
        "pattern": "roundDots",
        "cellSize": 9,
        "angle": 31,
        "ventilationPercent": 22,
        "speckCleanup": "strong"
    });
    let one_band = Plugin::load(DTF_PREP, Limits::default()).unwrap();
    let many_bands = Plugin::load(DTF_PREP, Limits { band_bytes: 1, ..Limits::default() }).unwrap();
    let expected = apply(&one_band, &source, width, height, params.clone());
    let actual = apply(&many_bands, &source, width, height, params);
    assert_eq!(actual, expected, "global screen and overlap cleanup must not restart at x=256/512/768");
}

#[test]
fn hard_selection_preserves_outside_and_unsupported_modes_fail_transactionally() {
    let plugin = Plugin::load(DTF_PREP, Limits::default()).unwrap();
    let source = rgba_surface(64, 32);
    let mut selection = Surface::new(PixelFormat::GRAY8);
    selection.fill_rect(Rect::new(32, 0, 64, 32), &[1.0]);
    let output = plugin.apply(&source, Rect::new(0, 0, 64, 32), Some(&selection), &json!({"pattern":"lines","speckCleanup":"off"})).unwrap();
    for y in 0..32 {
        for x in 0..64 {
            let pixel = output.pixel(x, y);
            if x < 32 {
                assert_eq!(pixel, source.pixel(x, y));
            } else {
                assert!(pixel[3] == 0.0 || pixel[3] == 1.0);
            }
        }
    }

    let rgb = PixelFormat { mode: ColorMode::Rgb, sample: SampleType::U8, alpha: false };
    let mut unsupported = Surface::new(rgb);
    unsupported.fill_rect(Rect::new(0, 0, 8, 8), &[0.2, 0.4, 0.6]);
    let before = unsupported.clone();
    let error = plugin.apply(&unsupported, Rect::new(0, 0, 8, 8), None, &json!({})).unwrap_err();
    assert_eq!(error, Error::Failed("pc_filter returned error code 2".into()));
    assert_eq!(unsupported, before, "failed runs are transactional");
}
