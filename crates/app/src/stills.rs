//! Still pictures shown instead of a camera: SMPTE color bars, black, or a
//! user-chosen image file.

use std::path::Path;
use std::sync::Arc;

use shared::image::{fill_black_bgra, scale_bgra_letterbox, smpte_bars_bgra};

use crate::config::StillSource;

/// Decoded image, BGRA, tightly packed, opaque.
pub struct StillImage {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// A [`StillSource`] ready to draw.
#[derive(Clone)]
pub enum Still {
    ColorBars,
    Black,
    Image(Arc<StillImage>),
}

/// Image file extensions offered in the file picker.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp"];

impl Still {
    /// Resolves `source`; a file that fails to load falls back to color bars
    /// and returns the error alongside.
    pub fn resolve(source: &StillSource) -> (Self, Option<String>) {
        match source {
            StillSource::ColorBars => (Self::ColorBars, None),
            StillSource::Black => (Self::Black, None),
            StillSource::Image(path) => match load(path) {
                Ok(img) => (Self::Image(Arc::new(img)), None),
                Err(e) => {
                    log::warn!("loading {}: {e}", path.display());
                    (Self::ColorBars, Some(e))
                }
            },
        }
    }

    /// Draws into `dst` (BGRA, `w` x `h`). Images are letterboxed.
    pub fn draw(&self, dst: &mut Vec<u8>, w: u32, h: u32) {
        match self {
            Self::ColorBars => smpte_bars_bgra(dst, w, h),
            Self::Black => {
                dst.resize((w * h * 4) as usize, 0);
                fill_black_bgra(dst);
            }
            Self::Image(img) => {
                dst.resize((w * h * 4) as usize, 0);
                scale_bgra_letterbox(&img.bgra, img.width, img.height, (img.width * 4) as usize, dst, w, h);
            }
        }
    }
}

/// Decodes an image file to opaque BGRA (transparency composited onto black).
pub fn load(path: &Path) -> Result<StillImage, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let mut bgra = img.into_raw();
    for px in bgra.chunks_exact_mut(4) {
        let a = px[3] as u32;
        let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
        px[0] = ((b * a + 127) / 255) as u8;
        px[1] = ((g * a + 127) / 255) as u8;
        px[2] = ((r * a + 127) / 255) as u8;
        px[3] = 255;
    }
    Ok(StillImage { width, height, bgra })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_png_composites_alpha_and_swaps_channels() {
        let dir = std::env::temp_dir().join(format!("camswitch-still-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.png");
        let mut img = image::RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([255, 255, 255, 0]));
        img.save(&path).unwrap();

        let still = load(&path).unwrap();
        assert_eq!((still.width, still.height), (2, 1));
        assert_eq!(&still.bgra[..4], &[0, 0, 255, 255]);
        assert_eq!(&still.bgra[4..], &[0, 0, 0, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_falls_back_to_bars() {
        let (still, err) = Still::resolve(&StillSource::Image("does/not/exist.png".into()));
        assert!(matches!(still, Still::ColorBars));
        assert!(err.is_some());
    }
}
