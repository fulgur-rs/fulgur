use fulgur_vrt::diff::compare;
use fulgur_vrt::manifest::Tolerance;
use image::{ImageBuffer, Rgba};

#[test]
fn fulgur_vrt_diff_is_reachable_as_devdep() {
    let a: image::RgbaImage = ImageBuffer::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
    let b = a.clone();
    let tol = Tolerance {
        max_channel_diff: 0,
        max_diff_pixels_ratio: 0.0,
    };
    let r = compare(&a, &b, tol);
    assert!(r.pass);
}
