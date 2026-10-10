use super::*;

#[test]
fn box_radii_match_the_gaussian_variance() {
    for sigma in [0.5_f32, 1.0, 2.0, 3.7, 10.0, 25.0] {
        let variance: f32 = box_radii(sigma)
            .iter()
            .map(|&radius| {
                let width = (radius * 2 + 1) as f32;
                (width * width - 1.0) / 12.0
            })
            .sum();
        assert!(
            (variance - sigma * sigma).abs() <= sigma * sigma * 0.15 + 0.5,
            "sigma {sigma}: variance {variance}"
        );
    }
    assert_eq!(box_radii(0.0), [0; 3]);
    assert_eq!(box_radii(f32::NAN), [0; 3]);
}

#[test]
fn blur_spreads_an_impulse_symmetrically_and_keeps_its_mass() {
    let (width, height) = (41, 41);
    let mut pixels = vec![0.0; width * height];
    pixels[20 * width + 20] = 1000.0;
    gaussian_blur(&mut pixels, width, height, 3.0);
    let total: f32 = pixels.iter().sum();
    assert!((total - 1000.0).abs() < 0.5, "{total}");
    let at = |x: usize, y: usize| pixels[y * width + x];
    assert!(at(20, 20) > at(23, 20));
    assert!(at(23, 20) > 0.0);
    assert!((at(17, 20) - at(23, 20)).abs() < 1e-3);
    assert!((at(20, 17) - at(20, 23)).abs() < 1e-3);
    assert_eq!(at(0, 0), 0.0);
}

#[test]
fn box_blur_treats_pixels_past_the_edge_as_zero() {
    let input = [3.0, 0.0, 0.0, 0.0];
    let mut output = [0.0; 4];
    box_blur(&input, &mut output, 1, 1);
    assert_eq!(output, [1.0, 1.0, 0.0, 0.0]);
}

#[test]
fn rasters_take_from_the_budget_and_stop_when_it_is_spent() {
    let mut builder = tiny_skia::PathBuilder::new();
    builder.push_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap());
    let path = builder.finish().unwrap();
    let shadow = ShadowParams {
        offset: (0.0, 0.0),
        blur_radius: 2.0,
        color: CssColor::BLACK,
    };
    let mut budget = Budget::default();
    let raster = blurred_raster(&path, shadow, &mut budget).expect("a raster");
    let used = (raster.pixel_width * raster.pixel_height) as f32;
    assert_eq!(budget.pixels, DOCUMENT_RASTER_PIXELS - used);
    budget.pixels = 10.0;
    assert!(blurred_raster(&path, shadow, &mut budget).is_none());
    assert_eq!(budget.pixels, 10.0);
}

#[test]
fn rounded_rasters_stay_within_the_pixel_limit() {
    let shadow = ShadowParams {
        offset: (0.3, 0.7),
        blur_radius: 0.01,
        color: CssColor::BLACK,
    };
    // A long, very thin shape: rounding its height up to a pixel must not
    // let the raster grow past the limit.
    for (width, height) in [(1.0e8_f32, 0.001_f32), (3000.0, 2000.0), (0.5, 0.5)] {
        let mut builder = tiny_skia::PathBuilder::new();
        builder.push_rect(tiny_skia::Rect::from_xywh(0.25, 0.25, width, height).unwrap());
        let path = builder.finish().unwrap();
        let mut budget = Budget::default();
        if let Some(raster) = blurred_raster(&path, shadow, &mut budget) {
            let area = u64::from(raster.pixel_width) * u64::from(raster.pixel_height);
            assert!(area as f32 <= MAX_RASTER_PIXELS, "{width}x{height}: {area}");
        }
    }
}

#[test]
fn fitting_scale_bounds_the_rounded_area() {
    for (width, height, pixels) in [
        (100.0_f32, 50.0_f32, 4.0e6_f32),
        (1.0e8, 0.001, 4.0e6),
        (0.001, 0.001, 100.0),
    ] {
        let scale = fitting_scale(width, height, pixels);
        let area = (width * scale + 2.0) * (height * scale + 2.0);
        assert!(area <= pixels * 1.0001, "{width}x{height}: {area}");
    }
    assert!(fitting_scale(1.0, 1.0, 4.0).is_nan());
    assert!(fitting_scale(1.0, 1.0, -5.0).is_nan());
}
