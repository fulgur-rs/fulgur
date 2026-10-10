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
