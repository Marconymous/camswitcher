"""Renders the CamSwitch icon: white "C", pastel-red camera glyph, white "S"
on a dark grey rounded square. Small sizes (<= 24 px) and the tray icon show
only the camera. Outputs go to crates/app/assets/ and are committed, so a
normal build needs no Python.

Run: py scripts/make-icon.py   (needs Pillow and the Windows Segoe fonts)
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

OUT = Path(__file__).resolve().parent.parent / "crates" / "app" / "assets"
FONTS = Path("C:/Windows/Fonts")
LETTER_FONT = FONTS / "segoeuib.ttf"  # Segoe UI Bold
SYMBOL_FONT = FONTS / "seguisym.ttf"  # Segoe UI Symbol: monochrome, not emoji

BG = (0x2B, 0x2B, 0x2B, 255)
CAMERA = (0xE8, 0x77, 0x7A, 255)
LETTER = (0xFF, 0xFF, 0xFF, 255)
SLASH = (0xE0, 0x30, 0x30, 255)
CAMERA_CHAR = "\U0001F4F7"

RADIUS = 0.18  # corner radius, fraction of the side
SUPERSAMPLE = 4

# Layout in units of the side length.
FULL_WIDTH = 0.86  # "C", camera and "S" together
FULL_GAP = 0.02
LETTER_SCALE = 0.95  # letters a bit smaller than the camera
SMALL_WIDTH = 0.80  # camera-only variant


def glyph_box(font, text):
    l, t, r, b = font.getbbox(text)
    return l, t, r - l, b - t


def layout(side, full):
    """Glyphs to draw as (text, font_path, font_size, color, x_center), plus
    the vertical centre. Sizes are found by measuring at a reference size."""
    ref = 1000
    cam = ImageFont.truetype(str(SYMBOL_FONT), ref)
    _, _, cam_w, _ = glyph_box(cam, CAMERA_CHAR)
    if not full:
        size = ref * SMALL_WIDTH * side / cam_w
        return [(CAMERA_CHAR, SYMBOL_FONT, size, CAMERA, side / 2)]

    let = ImageFont.truetype(str(LETTER_FONT), ref * LETTER_SCALE)
    _, _, c_w, _ = glyph_box(let, "C")
    _, _, s_w, _ = glyph_box(let, "S")
    total = c_w + cam_w + s_w + 2 * FULL_GAP * ref
    k = FULL_WIDTH * side / total  # reference units to pixels
    gap = FULL_GAP * ref * k
    x = (side - total * k) / 2
    out = []
    for text, path, size, w, color in (
        ("C", LETTER_FONT, ref * LETTER_SCALE, c_w, LETTER),
        (CAMERA_CHAR, SYMBOL_FONT, ref, cam_w, CAMERA),
        ("S", LETTER_FONT, ref * LETTER_SCALE, s_w, LETTER),
    ):
        out.append((text, path, size * k, color, x + w * k / 2))
        x += w * k + gap
    return out


def render(side, full, muted=False):
    big = side * SUPERSAMPLE
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((0, 0, big - 1, big - 1), radius=RADIUS * big, fill=BG)
    for text, path, size, color, cx in layout(big, full):
        font = ImageFont.truetype(str(path), round(size))
        l, t, w, h = glyph_box(font, text)
        d.text((cx - w / 2 - l, big / 2 - h / 2 - t), text, font=font, fill=color)
    if muted:
        m = 0.18 * big
        width = round(0.11 * big)
        line = ((m, big - m), (big - m, m))
        d.line(line, fill=BG, width=round(width * 1.8))
        d.line(line, fill=SLASH, width=width)
    return img.resize((side, side), Image.LANCZOS)


def svg(side=256):
    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{side}" height="{side}" viewBox="0 0 {side} {side}">',
        f'  <rect width="{side}" height="{side}" rx="{RADIUS * side:.1f}" fill="#2b2b2b"/>',
    ]
    for text, path, size, color, cx in layout(side, True):
        if text == CAMERA_CHAR:
            family, content = "Segoe UI Symbol", "&#x1F4F7;&#xFE0E;"
        else:
            family, content = "Segoe UI", text
        fill = "#{:02x}{:02x}{:02x}".format(*color[:3])
        parts.append(
            f'  <text x="{cx:.1f}" y="{side / 2:.1f}" font-family="{family}" font-weight="bold" '
            f'font-size="{size:.1f}" fill="{fill}" text-anchor="middle" dominant-baseline="central">{content}</text>'
        )
    parts.append("</svg>\n")
    return "\n".join(parts)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "icon.svg").write_text(svg(), encoding="utf-8")
    render(256, True).save(OUT / "icon-256.png")
    render(32, False).save(OUT / "tray.png")
    render(32, False, muted=True).save(OUT / "tray-muted.png")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    images = [render(s, s > 24) for s in sizes]
    images[-1].save(OUT / "icon.ico", sizes=[(s, s) for s in sizes], append_images=images[:-1])


if __name__ == "__main__":
    main()
