"""Generate the application icon.

Kept in the repository so the icon is reproducible rather than an opaque binary
someone has to redraw from scratch. Run it, then hand the output to Tauri:

    python icon/generate.py
    pnpm tauri icon icon/icon.png

The mark is four arrows pointing inward — the most direct way to say
"compress" without words, which matters at 16x16 in a taskbar.
"""

from PIL import Image, ImageDraw

OUTPUT = "icon/icon.png"

SIZE = 1024
# Pillow has no antialiased drawing, so everything is drawn at 4x and
# downsampled. Without this the diagonals come out visibly stepped.
SUPERSAMPLE = 4

# Coordinates below are authored in a 96x96 space to match the design mockup.
DESIGN = 96

BACKGROUND = (28, 28, 34, 255)  # #1c1c22, the app's panel colour
ACCENT = (127, 119, 221, 255)  # #7F77DD, the app's accent

CORNER_RADIUS = 21  # in design units
STROKE = 5.5

# The mockup's arrows fill under half the tile, which reads thin once Windows
# shrinks it to 16x16 in the taskbar. Everything is scaled about the centre so
# the glyph occupies a more usual ~60% of the icon.
GLYPH_SCALE = 1.3


def build() -> Image.Image:
    canvas = SIZE * SUPERSAMPLE
    scale = canvas / DESIGN

    image = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(
        [0, 0, canvas - 1, canvas - 1],
        radius=int(CORNER_RADIUS * scale),
        fill=BACKGROUND,
    )

    width = STROKE * GLYPH_SCALE * scale
    centre = DESIGN / 2

    def point(x: float, y: float) -> tuple[float, float]:
        # Scale about the centre of the tile, not the origin, so the glyph
        # grows outward rather than sliding toward the bottom-right.
        return (
            (centre + (x - centre) * GLYPH_SCALE) * scale,
            (centre + (y - centre) * GLYPH_SCALE) * scale,
        )

    def stroke(start: tuple[float, float], end: tuple[float, float]) -> None:
        """A thick line with round caps.

        Pillow's `line` has square ends, which make the arrowheads look chipped
        where segments meet, so each endpoint gets a circle of the same radius.
        """
        draw.line([point(*start), point(*end)], fill=ACCENT, width=int(width))
        for x, y in (start, end):
            cx, cy = point(x, y)
            r = width / 2
            draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=ACCENT)

    # Each arrow is a diagonal running from the corner toward the centre, with
    # a two-segment head at the inner end.
    arrows = [
        # (tail, head, head corner A, head corner B)
        ((26, 26), (38, 38), (38, 26), (26, 38)),  # top-left
        ((70, 26), (58, 38), (58, 26), (70, 38)),  # top-right
        ((26, 70), (38, 58), (26, 58), (38, 70)),  # bottom-left
        ((70, 70), (58, 58), (70, 58), (58, 70)),  # bottom-right
    ]

    for tail, head, corner_a, corner_b in arrows:
        stroke(tail, head)
        stroke(corner_a, head)
        stroke(head, corner_b)

    return image.resize((SIZE, SIZE), Image.LANCZOS)


if __name__ == "__main__":
    build().save(OUTPUT)
    print(f"wrote {OUTPUT} at {SIZE}x{SIZE}")
