use crate::manifest::Tolerance;
use image::{ImageBuffer, Rgba, RgbaImage};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct DiffReport {
    pub total_pixels: u64,
    pub diff_pixels: u64,
    pub max_channel_diff: u8,
    pub pass: bool,
}

impl DiffReport {
    pub fn ratio(&self) -> f32 {
        if self.total_pixels == 0 {
            0.0
        } else {
            self.diff_pixels as f32 / self.total_pixels as f32
        }
    }
}

fn channel_diff(a: &Rgba<u8>, b: &Rgba<u8>) -> u8 {
    let dr = a[0].abs_diff(b[0]);
    let dg = a[1].abs_diff(b[1]);
    let db = a[2].abs_diff(b[2]);
    dr.max(dg).max(db)
}

pub fn compare(reference: &RgbaImage, actual: &RgbaImage, tol: Tolerance) -> DiffReport {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();

    if (rw, rh) != (aw, ah) {
        let total = u64::from(rw) * u64::from(rh);
        return DiffReport {
            total_pixels: total,
            diff_pixels: total,
            max_channel_diff: 255,
            pass: false,
        };
    }

    let total = u64::from(rw) * u64::from(rh);
    let mut diff = 0u64;
    let mut max_ch: u8 = 0;

    for (pa, pb) in reference.pixels().zip(actual.pixels()) {
        let c = channel_diff(pa, pb);
        if c > max_ch {
            max_ch = c;
        }
        if c > tol.max_channel_diff {
            diff += 1;
        }
    }

    let ratio = if total == 0 {
        0.0
    } else {
        diff as f32 / total as f32
    };
    let pass = ratio <= tol.max_diff_pixels_ratio;

    DiffReport {
        total_pixels: total,
        diff_pixels: diff,
        max_channel_diff: max_ch,
        pass,
    }
}

pub fn write_diff_image(
    reference: &RgbaImage,
    actual: &RgbaImage,
    tol: Tolerance,
    out_path: &Path,
) -> anyhow::Result<()> {
    let (rw, rh) = reference.dimensions();
    let (aw, ah) = actual.dimensions();
    let (w, h) = (rw.max(aw), rh.max(ah));

    let mut out: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([255, 255, 255, 255]));

    for y in 0..h {
        for x in 0..w {
            let ref_px = reference.get_pixel_checked(x, y);
            let act_px = actual.get_pixel_checked(x, y);

            match (ref_px, act_px) {
                (Some(r), Some(a)) if channel_diff(r, a) > tol.max_channel_diff => {
                    out.put_pixel(x, y, Rgba([255, 0, 0, 255]));
                }
                (Some(r), Some(_)) => {
                    let l = (0.299 * r[0] as f32 + 0.587 * r[1] as f32 + 0.114 * r[2] as f32) as u8;
                    let l_dim = l.saturating_add(80);
                    out.put_pixel(x, y, Rgba([l_dim, l_dim, l_dim, 255]));
                }
                _ => {
                    out.put_pixel(x, y, Rgba([255, 255, 0, 255]));
                }
            }
        }
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    out.save(out_path)?;
    Ok(())
}

pub fn load_png(path: &Path) -> anyhow::Result<RgbaImage> {
    let img = image::open(path)?.to_rgba8();
    Ok(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        ImageBuffer::from_pixel(w, h, Rgba(rgba))
    }

    const STRICT: Tolerance = Tolerance {
        max_channel_diff: 0,
        max_diff_pixels_ratio: 0.0,
    };
    const LOOSE: Tolerance = Tolerance {
        max_channel_diff: 4,
        max_diff_pixels_ratio: 0.01,
    };

    #[test]
    fn identical_images_pass_strict() {
        let a = solid(10, 10, [255, 0, 0, 255]);
        let b = solid(10, 10, [255, 0, 0, 255]);
        let r = compare(&a, &b, STRICT);
        assert!(r.pass);
        assert_eq!(r.diff_pixels, 0);
        assert_eq!(r.max_channel_diff, 0);
    }

    #[test]
    fn small_channel_diff_below_tolerance_passes() {
        let a = solid(10, 10, [100, 100, 100, 255]);
        let b = solid(10, 10, [103, 100, 100, 255]); // diff=3 < 4
        let r = compare(&a, &b, LOOSE);
        assert!(r.pass);
        assert_eq!(r.diff_pixels, 0, "3 <= 4 should not be counted as diff");
        assert_eq!(r.max_channel_diff, 3);
    }

    #[test]
    fn channel_diff_above_tolerance_counts_as_diff() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(10, 10, [10, 0, 0, 255]); // diff=10 > 4
        let r = compare(&a, &b, LOOSE);
        assert!(!r.pass);
        assert_eq!(r.diff_pixels, 100);
        assert_eq!(r.max_channel_diff, 10);
    }

    #[test]
    fn sparse_diff_within_ratio_passes() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.put_pixel(0, 0, Rgba([50, 0, 0, 255]));
        let r = compare(&a, &b, LOOSE);
        assert_eq!(r.diff_pixels, 1);
        assert!(r.pass, "1/100 = 0.01 must be within 0.01 ratio limit");
    }

    #[test]
    fn size_mismatch_fails_with_all_diff() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(12, 10, [0, 0, 0, 255]);
        let r = compare(&a, &b, LOOSE);
        assert!(!r.pass);
        assert_eq!(r.diff_pixels, r.total_pixels);
    }

    #[test]
    fn write_diff_image_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("diff.png");
        let a = solid(4, 4, [0, 0, 0, 255]);
        let mut b = a.clone();
        b.put_pixel(1, 1, Rgba([255, 0, 0, 255]));
        write_diff_image(&a, &b, LOOSE, &out).unwrap();
        assert!(out.exists());
        let loaded = load_png(&out).unwrap();
        assert_eq!(loaded.dimensions(), (4, 4));
    }
}
