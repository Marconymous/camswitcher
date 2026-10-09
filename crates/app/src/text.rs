//! Renders text to a premultiplied BGRA bitmap with GDI: white, anti-aliased,
//! Segoe UI. Used for text overlays.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_NOPREFIX, DeleteDC, DeleteObject, DrawTextW, FW_SEMIBOLD,
    GdiFlush, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::core::w;

use crate::stills::StillImage;

/// `text` (may contain line breaks) with lines `line_px` high. `None` for
/// empty text or when GDI fails.
pub fn render(text: &str, line_px: u32) -> Option<StillImage> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    // An empty slice passes a dangling pointer, which DrawTextW dereferences.
    if wide.is_empty() || line_px == 0 {
        return None;
    }
    unsafe {
        let hdc = CreateCompatibleDC(None);
        let font = CreateFontW(
            -(line_px as i32),
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(hdc, font.into());
        let mut r = RECT::default();
        DrawTextW(hdc, &mut wide, &mut r, DT_CALCRECT | DT_NOPREFIX);
        let (w, h) = (r.right.max(1), r.bottom.max(1));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let result = match CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(dib) if !bits.is_null() => {
                let old_bmp = SelectObject(hdc, dib.into());
                // The section starts zeroed: white text on black, the
                // brightness becomes the coverage (alpha).
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00FF_FFFF));
                DrawTextW(hdc, &mut wide, &mut r, DT_NOPREFIX);
                let _ = GdiFlush();
                let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
                let bgra = px
                    .chunks_exact(4)
                    .flat_map(|p| {
                        let a = p[0].max(p[1]).max(p[2]);
                        [a, a, a, a]
                    })
                    .collect();
                SelectObject(hdc, old_bmp);
                let _ = DeleteObject(dib.into());
                Some(StillImage { width: w as u32, height: h as u32, bgra })
            }
            _ => None,
        };
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteDC(hdc);
        result
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn renders_white_coverage() {
        let img = super::render("Hi\nthere", 20).unwrap();
        assert!(img.height >= 40 && img.width > 10);
        assert!(img.bgra.chunks_exact(4).any(|p| p[3] == 255));
        assert!(img.bgra.chunks_exact(4).all(|p| p[0] == p[3]));
        assert!(super::render("", 20).is_none());
    }
}
