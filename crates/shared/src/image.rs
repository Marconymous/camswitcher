//! CPU image helpers: letterbox scaling (BGRA) and BGRA <-> NV12 conversion
//! (BT.601, limited range).

/// Largest rectangle with the source aspect ratio that fits `dw` x `dh`,
/// centered. Returns `(x, y, w, h)`, all even so NV12 chroma stays aligned.
pub fn fit_rect(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 {
        return (0, 0, 0, 0);
    }
    let (w, h) = if (sw as u64) * (dh as u64) > (sh as u64) * (dw as u64) {
        (dw, ((dw as u64 * sh as u64) / sw as u64) as u32)
    } else {
        (((dh as u64 * sw as u64) / sh as u64) as u32, dh)
    };
    let (w, h) = (w & !1, h & !1);
    (((dw - w) / 2) & !1, ((dh - h) / 2) & !1, w, h)
}

/// Fills a BGRA buffer with opaque black.
pub fn fill_black_bgra(dst: &mut [u8]) {
    for px in dst.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 255]);
    }
}

/// A rectangle `(x, y, w, h)` in pixels.
pub type Rect = (u32, u32, u32, u32);

/// Scales `src` (BGRA, `src_stride` bytes per row) into `dst` (BGRA, tightly
/// packed `dw` x `dh`), preserving aspect ratio with black bars. Bilinear.
pub fn scale_bgra_letterbox(src: &[u8], sw: u32, sh: u32, src_stride: usize, dst: &mut [u8], dw: u32, dh: u32) {
    let dst_len = (dw * dh * 4) as usize;
    let dst = &mut dst[..dst_len];
    let fit = fit_rect(sw, sh, dw, dh);
    if fit.2 != dw || fit.3 != dh {
        fill_black_bgra(dst);
    }
    scale_bgra_rect(src, src_stride, (0, 0, sw, sh), dst, dw, fit, false);
}

/// Scales the `src_rect` part of `src` (BGRA, `src_stride` bytes per row) onto
/// the `dst_rect` part of `dst` (BGRA, tightly packed, `dw` wide), mirrored
/// horizontally if `mirror`. Bilinear; pixels outside `dst_rect` are untouched.
pub fn scale_bgra_rect(src: &[u8], src_stride: usize, src_rect: Rect, dst: &mut [u8], dw: u32, dst_rect: Rect, mirror: bool) {
    scale_rect::<false>(src, src_stride, src_rect, dst, dw, dst_rect, mirror);
}

/// Scales a whole premultiplied-alpha BGRA image (tightly packed) to
/// `dw` x `dh`, keeping its alpha channel.
pub fn scale_premul(src: &[u8], sw: u32, sh: u32, dst: &mut Vec<u8>, dw: u32, dh: u32) {
    dst.resize((dw * dh * 4) as usize, 0);
    scale_rect::<true>(src, (sw * 4) as usize, (0, 0, sw, sh), dst, dw, (0, 0, dw, dh), false);
}

/// Bilinear scaling; `ALPHA` interpolates the alpha channel instead of
/// writing it opaque.
fn scale_rect<const ALPHA: bool>(src: &[u8], src_stride: usize, src_rect: Rect, dst: &mut [u8], dw: u32, dst_rect: Rect, mirror: bool) {
    let (sx, sy, sw, sh) = src_rect;
    let (ox, oy, fw, fh) = dst_rect;
    if fw == 0 || fh == 0 || sw == 0 || sh == 0 {
        return;
    }
    let dst_stride = (dw * 4) as usize;

    if fw == sw && fh == sh && !mirror && !ALPHA {
        for y in 0..sh as usize {
            let s0 = (sy as usize + y) * src_stride + sx as usize * 4;
            let s = &src[s0..s0 + (sw * 4) as usize];
            let d0 = (oy as usize + y) * dst_stride + ox as usize * 4;
            dst[d0..d0 + s.len()].copy_from_slice(s);
        }
        return;
    }

    // Per-column source byte offsets and weights (8-bit fixed point).
    let mut xs: Vec<(usize, usize, u32)> = (0..fw)
        .map(|dx| {
            let fx = ((dx as f32 + 0.5) * sw as f32 / fw as f32 - 0.5).max(0.0);
            let x0 = (fx as u32).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let w = ((fx - x0 as f32) * 256.0) as u32;
            ((sx + x0) as usize * 4, (sx + x1) as usize * 4, w.min(256))
        })
        .collect();
    if mirror {
        xs.reverse();
    }

    for dy in 0..fh {
        let fy = ((dy as f32 + 0.5) * sh as f32 / fh as f32 - 0.5).max(0.0);
        let y0 = (fy as u32).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let wy = (((fy - y0 as f32) * 256.0) as u32).min(256);
        let r0 = &src[(sy + y0) as usize * src_stride..];
        let r1 = &src[(sy + y1) as usize * src_stride..];
        let drow = &mut dst[(oy + dy) as usize * dst_stride + ox as usize * 4..];
        let channels = if ALPHA { 4 } else { 3 };
        for (dx, &(x0, x1, wx)) in xs.iter().enumerate() {
            let d = &mut drow[dx * 4..dx * 4 + 4];
            for c in 0..channels {
                let top = r0[x0 + c] as u32 * (256 - wx) + r0[x1 + c] as u32 * wx;
                let bot = r1[x0 + c] as u32 * (256 - wx) + r1[x1 + c] as u32 * wx;
                d[c] = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
            }
            if !ALPHA {
                d[3] = 255;
            }
        }
    }
}

/// Largest centered part of a `sw` x `sh` source with the aspect ratio of
/// `dw` x `dh` (crop-to-fill counterpart of [`fit_rect`]).
pub fn cover_rect(sw: u32, sh: u32, dw: u32, dh: u32) -> Rect {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0, sw, sh);
    }
    let (w, h) = if (sw as u64) * (dh as u64) > (sh as u64) * (dw as u64) {
        (((sh as u64 * dw as u64) / dh as u64).max(1) as u32, sh)
    } else {
        (sw, ((sw as u64 * dh as u64) / dw as u64).max(1) as u32)
    };
    ((sw - w) / 2, (sh - h) / 2, w, h)
}

/// Digital zoom: the `1/zoom` sized part of a `sw` x `sh` source centered on
/// `center` (0..1 each), shifted to stay inside the source.
pub fn zoom_rect(sw: u32, sh: u32, zoom: f32, center: [f32; 2]) -> Rect {
    let zoom = zoom.max(1.0);
    let w = ((sw as f32 / zoom).round() as u32).clamp(1, sw.max(1));
    let h = ((sh as f32 / zoom).round() as u32).clamp(1, sh.max(1));
    let place = |size: u32, part: u32, c: f32| -> u32 {
        let start = c.clamp(0.0, 1.0) * size as f32 - part as f32 / 2.0;
        (start.round().max(0.0) as u32).min(size.saturating_sub(part))
    };
    (place(sw, w, center[0]), place(sh, h, center[1]), w, h)
}

/// Rotates `src` (BGRA, `w` x `h`, `stride` bytes per row) clockwise by
/// `quarter_turns` x 90° into `dst` (tightly packed). Returns the new size.
pub fn rotate_bgra(src: &[u8], w: u32, h: u32, stride: usize, quarter_turns: u32, dst: &mut Vec<u8>) -> (u32, u32) {
    let turns = quarter_turns % 4;
    let (nw, nh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    dst.resize((nw * nh * 4) as usize, 0);
    let (w, h, nw) = (w as usize, h as usize, nw as usize);
    for y in 0..h {
        let row = &src[y * stride..y * stride + w * 4];
        for x in 0..w {
            let (dx, dy) = match turns {
                0 => (x, y),
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let d = (dy * nw + dx) * 4;
            dst[d..d + 4].copy_from_slice(&row[x * 4..x * 4 + 4]);
        }
    }
    (nw as u32, nh)
}

/// Converts tightly packed BGRA (`w` x `h`, both even) to NV12.
pub fn bgra_to_nv12(src: &[u8], w: u32, h: u32, dst: &mut Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    dst.resize(w * h * 3 / 2, 0);
    let (y_plane, uv_plane) = dst.split_at_mut(w * h);
    for y in 0..h {
        let row = &src[y * w * 4..(y + 1) * w * 4];
        let yrow = &mut y_plane[y * w..(y + 1) * w];
        for (x, px) in row.chunks_exact(4).enumerate() {
            let (b, g, r) = (px[0] as i32, px[1] as i32, px[2] as i32);
            yrow[x] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
        }
    }
    for cy in 0..h / 2 {
        let r0 = &src[(cy * 2) * w * 4..];
        let r1 = &src[(cy * 2 + 1) * w * 4..];
        let uvrow = &mut uv_plane[cy * w..(cy + 1) * w];
        for cx in 0..w / 2 {
            let i = cx * 8;
            let sum = |c: usize| r0[i + c] as i32 + r0[i + 4 + c] as i32 + r1[i + c] as i32 + r1[i + 4 + c] as i32;
            let (b, g, r) = ((sum(0) + 2) / 4, (sum(1) + 2) / 4, (sum(2) + 2) / 4);
            uvrow[cx * 2] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128) as u8;
            uvrow[cx * 2 + 1] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128) as u8;
        }
    }
}

/// Converts row `y` of an NV12 image (`w` x `h`) into BGRA pixels in `out`.
pub fn nv12_to_bgra_row(nv12: &[u8], w: u32, h: u32, y: u32, out: &mut [u8]) {
    let (w, h, y) = (w as usize, h as usize, y as usize);
    let yrow = &nv12[y * w..(y + 1) * w];
    let uvrow = &nv12[w * h + (y / 2) * w..];
    for x in 0..w {
        let c = yrow[x] as i32 - 16;
        let d = uvrow[x & !1] as i32 - 128;
        let e = uvrow[(x & !1) + 1] as i32 - 128;
        let clip = |v: i32| v.clamp(0, 255) as u8;
        out[x * 4] = clip((298 * c + 516 * d + 128) >> 8);
        out[x * 4 + 1] = clip((298 * c - 100 * d - 208 * e + 128) >> 8);
        out[x * 4 + 2] = clip((298 * c + 409 * e + 128) >> 8);
        out[x * 4 + 3] = 255;
    }
}

/// Fills an NV12 buffer with black (Y=16, U=V=128).
pub fn fill_black_nv12(dst: &mut Vec<u8>, w: u32, h: u32) {
    let ylen = (w * h) as usize;
    dst.resize(ylen * 3 / 2, 0);
    dst[..ylen].fill(16);
    dst[ylen..].fill(128);
}

/// Eight vertical color bars shifted by `offset` pixels, as NV12.
pub fn color_bars_nv12(dst: &mut Vec<u8>, w: u32, h: u32, offset: u32) {
    const BARS: [[u8; 3]; 8] = [
        [191, 191, 191],
        [0, 191, 191],
        [191, 191, 0],
        [0, 191, 0],
        [191, 0, 191],
        [0, 0, 191],
        [191, 0, 0],
        [16, 16, 16],
    ];
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let bar = (((x + offset) % w) * 8 / w) as usize;
            let i = ((y * w + x) * 4) as usize;
            bgra[i..i + 3].copy_from_slice(&BARS[bar]);
            bgra[i + 3] = 255;
        }
    }
    bgra_to_nv12(&bgra, w, h, dst);
}

/// Draws SMPTE color bars (EG 1-1990 layout: 75% bars, reverse castellations,
/// -I / white / +Q / PLUGE) into a tightly packed BGRA buffer of `w` x `h`.
pub fn smpte_bars_bgra(dst: &mut Vec<u8>, w: u32, h: u32) {
    // RGB values; black is 7.5% setup (#131313).
    const TOP: [[u8; 3]; 7] = [
        [192, 192, 192],
        [192, 192, 0],
        [0, 192, 192],
        [0, 192, 0],
        [192, 0, 192],
        [192, 0, 0],
        [0, 0, 192],
    ];
    const BLACK: [u8; 3] = [19, 19, 19];
    const MIDDLE: [[u8; 3]; 7] = [[0, 0, 192], BLACK, [192, 0, 192], BLACK, [0, 192, 192], BLACK, [192, 192, 192]];
    // Bottom band as (start in 1/84 width, color); a top bar is 12 units.
    // -I, white, +Q, black are 5/4 bar each; PLUGE -4%/0/+4% splits bar 6; bar 7 black.
    const BOTTOM: [(u32, [u8; 3]); 8] = [
        (0, [0, 33, 76]),
        (15, [255, 255, 255]),
        (30, [50, 0, 106]),
        (45, BLACK),
        (60, [9, 9, 9]),
        (64, BLACK),
        (68, [29, 29, 29]),
        (72, BLACK),
    ];
    let (wu, hu) = (w as usize, h as usize);
    dst.resize(wu * hu * 4, 0);
    if w == 0 || h == 0 {
        return;
    }
    let top_end = h * 2 / 3;
    let mid_end = h * 3 / 4;
    let mut rows: [Vec<u8>; 3] = Default::default();
    for (band, row) in rows.iter_mut().enumerate() {
        row.reserve(wu * 4);
        for x in 0..w {
            let rgb = match band {
                0 => TOP[(x * 7 / w) as usize],
                1 => MIDDLE[(x * 7 / w) as usize],
                _ => {
                    let unit = x * 84 / w; // 1/84 width = 1/3 of 1/28
                    BOTTOM.iter().rev().find(|(start, _)| unit >= *start).unwrap().1
                }
            };
            row.extend_from_slice(&[rgb[2], rgb[1], rgb[0], 255]);
        }
    }
    for y in 0..h {
        let band = if y < top_end { 0 } else if y < mid_end { 1 } else { 2 };
        let o = y as usize * wu * 4;
        dst[o..o + wu * 4].copy_from_slice(&rows[band]);
    }
}

/// Crossfade: `dst = dst * (1 - t) + src * t` per channel, `t` in 0..=1.
/// Both buffers BGRA of equal length.
pub fn blend_bgra(dst: &mut [u8], src: &[u8], t: f32) {
    let w = (t.clamp(0.0, 1.0) * 256.0).round() as u32;
    for (d, s) in dst.iter_mut().zip(src) {
        *d = ((*d as u32 * (256 - w) + *s as u32 * w + 128) >> 8) as u8;
    }
}

/// Draws premultiplied BGRA `src` (`sw` x `sh`) over opaque `dst` (`dw` x
/// `dh`) with its top-left corner at `(x, y)`; clipped to `dst`.
pub fn alpha_blit(dst: &mut [u8], dw: u32, dh: u32, src: &[u8], sw: u32, sh: u32, (x, y): (i32, i32)) {
    for sy in 0..sh as i32 {
        let dy = y + sy;
        if dy < 0 || dy >= dh as i32 {
            continue;
        }
        let x0 = x.max(0);
        let x1 = (x + sw as i32).min(dw as i32);
        for dx in x0..x1 {
            let s = ((sy as u32 * sw + (dx - x) as u32) * 4) as usize;
            let d = ((dy as u32 * dw + dx as u32) * 4) as usize;
            let a = src[s + 3] as u32;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                dst[d + c] = (src[s + c] as u32 + (dst[d + c] as u32 * (255 - a) + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// Fills `rect` of `dst` (`dw` wide) with `bgr`, blended at `alpha` (0..=255).
pub fn shade_rect(dst: &mut [u8], dw: u32, rect: Rect, bgr: [u8; 3], alpha: u8) {
    let (x, y, w, h) = rect;
    let a = alpha as u32;
    for row in y..y + h {
        let start = ((row * dw + x) * 4) as usize;
        for px in dst[start..start + (w * 4) as usize].chunks_exact_mut(4) {
            for c in 0..3 {
                px[c] = ((px[c] as u32 * (255 - a) + bgr[c] as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Swaps B and R in place (BGRA <-> RGBA).
pub fn swap_rb(buf: &mut [u8]) {
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_rect_letterbox_and_pillarbox() {
        assert_eq!(fit_rect(1920, 1080, 1280, 720), (0, 0, 1280, 720));
        // 4:3 into 16:9 -> pillarbox
        assert_eq!(fit_rect(640, 480, 1280, 720), (160, 0, 960, 720));
        // 21:9-ish into 16:9 -> letterbox
        let (x, y, w, h) = fit_rect(2560, 1080, 1280, 720);
        assert_eq!((x, w), (0, 1280));
        assert!(y > 0 && h < 720 && h % 2 == 0);
    }

    #[test]
    fn black_bgra_converts_to_nv12_black() {
        let mut bgra = vec![0u8; 8 * 4 * 4];
        fill_black_bgra(&mut bgra);
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 8, 4, &mut nv12);
        let mut black = Vec::new();
        fill_black_nv12(&mut black, 8, 4);
        assert_eq!(nv12, black);
    }

    #[test]
    fn white_roundtrip() {
        let bgra = vec![255u8; 4 * 2 * 4];
        let mut nv12 = Vec::new();
        bgra_to_nv12(&bgra, 4, 2, &mut nv12);
        assert_eq!(nv12[0], 235);
        let mut row = vec![0u8; 16];
        nv12_to_bgra_row(&nv12, 4, 2, 0, &mut row);
        assert!(row.iter().all(|&v| v >= 253));
    }

    #[test]
    fn smpte_bars_layout() {
        let (w, h) = (1280u32, 720u32);
        let mut bgra = Vec::new();
        smpte_bars_bgra(&mut bgra, w, h);
        assert_eq!(bgra.len(), (w * h * 4) as usize);
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [bgra[i + 2], bgra[i + 1], bgra[i]]
        };
        assert_eq!(px(10, 10), [192, 192, 192]); // gray
        assert_eq!(px(w - 10, 10), [0, 0, 192]); // blue
        assert_eq!(px(10, 500), [0, 0, 192]); // castellation under gray
        assert_eq!(px(w / 2, 700), [50, 0, 106]); // +Q
        assert_eq!(px(762, 700), [19, 19, 19]); // black
        assert_eq!(px(10, 700), [0, 33, 76]); // -I
    }

    /// 2x1 image: left red, right blue (BGRA).
    fn red_blue() -> Vec<u8> {
        vec![0, 0, 255, 255, 255, 0, 0, 255]
    }

    #[test]
    fn mirror_flips_columns() {
        let src = red_blue();
        let mut dst = vec![0u8; 8];
        scale_bgra_rect(&src, 8, (0, 0, 2, 1), &mut dst, 2, (0, 0, 2, 1), true);
        assert_eq!(&dst[..4], &src[4..]);
        assert_eq!(&dst[4..], &src[..4]);
    }

    #[test]
    fn rotate_quarter_turns() {
        // 2x1 -> 1x2, clockwise: left pixel ends up on top.
        let src = red_blue();
        let mut dst = Vec::new();
        assert_eq!(rotate_bgra(&src, 2, 1, 8, 1, &mut dst), (1, 2));
        assert_eq!(&dst[..4], &src[..4]);
        assert_eq!(rotate_bgra(&src, 2, 1, 8, 3, &mut dst), (1, 2));
        assert_eq!(&dst[..4], &src[4..]);
        assert_eq!(rotate_bgra(&src, 2, 1, 8, 2, &mut dst), (2, 1));
        assert_eq!(&dst[..4], &src[4..]);
    }

    #[test]
    fn cover_and_zoom_rects() {
        // 4:3 source filling 16:9: full width, cropped height.
        assert_eq!(cover_rect(640, 480, 1280, 720), (0, 60, 640, 360));
        assert_eq!(cover_rect(1920, 1080, 1280, 720), (0, 0, 1920, 1080));
        assert_eq!(zoom_rect(1920, 1080, 1.0, [0.5, 0.5]), (0, 0, 1920, 1080));
        assert_eq!(zoom_rect(1920, 1080, 2.0, [0.5, 0.5]), (480, 270, 960, 540));
        // Panned past the edge: clamped inside.
        assert_eq!(zoom_rect(1920, 1080, 2.0, [1.0, 0.0]), (960, 0, 960, 540));
    }

    #[test]
    fn blend_endpoints_and_middle() {
        let a = [0u8, 100, 200, 255];
        let b = [200u8, 0, 100, 255];
        let mut d = a;
        blend_bgra(&mut d, &b, 0.0);
        assert_eq!(d, a);
        blend_bgra(&mut d, &b, 1.0);
        assert_eq!(d, b);
        let mut d = a;
        blend_bgra(&mut d, &b, 0.5);
        assert_eq!(d, [100, 50, 150, 255]);
    }

    #[test]
    fn alpha_blit_composites_and_clips() {
        let mut dst = [10u8, 10, 10, 255].repeat(4); // 2x2
        // 2x1: opaque red, half-transparent white (premultiplied).
        let src = [0u8, 0, 255, 255, 128, 128, 128, 128];
        alpha_blit(&mut dst, 2, 2, &src, 2, 1, (1, 1));
        assert_eq!(&dst[..12], &[10, 10, 10, 255].repeat(3)[..]);
        assert_eq!(&dst[12..15], &[0, 0, 255]);
        // Partly off-screen: no panic, untouched elsewhere.
        alpha_blit(&mut dst, 2, 2, &src, 2, 1, (-1, -1));
        alpha_blit(&mut dst, 2, 2, &src, 2, 1, (5, 0));
    }

    #[test]
    fn scale_premul_keeps_alpha() {
        let src = [0u8, 0, 0, 0].repeat(4);
        let mut dst = Vec::new();
        scale_premul(&src, 2, 2, &mut dst, 4, 4);
        assert!(dst.iter().all(|&v| v == 0));
    }

    #[test]
    fn scale_same_size_copies() {
        let src: Vec<u8> = (0..4 * 2 * 4).map(|i| i as u8).collect();
        let mut dst = vec![0u8; src.len()];
        scale_bgra_letterbox(&src, 4, 2, 16, &mut dst, 4, 2);
        assert_eq!(src, dst);
    }

    #[test]
    fn scale_uniform_color_stays_uniform() {
        let src = [10u8, 20, 30, 255].repeat(640 * 480);
        let mut dst = vec![0u8; 1280 * 720 * 4];
        scale_bgra_letterbox(&src, 640, 480, 640 * 4, &mut dst, 1280, 720);
        // inside the picture
        let i = (360 * 1280 + 640) * 4;
        assert_eq!(&dst[i..i + 4], &[10, 20, 30, 255]);
        // pillarbox bar
        assert_eq!(&dst[0..4], &[0, 0, 0, 255]);
    }
}
