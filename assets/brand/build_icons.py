#!/usr/bin/env python3
"""Generate the Headway icon SVG variants into ./svg/ and export app icons into ../icon/.

Run from anywhere: python3 assets/brand/build_icons.py (needs rsvg-convert and ImageMagick)."""
from pathlib import Path

OUT = Path(__file__).parent / "svg"
OUT.mkdir(exist_ok=True)

SQUIRCLE = '<rect width="512" height="512" rx="112"/>'
ROSE = "#e11d48"

# The mark: a lowercase h drawn as a git graph, read bottom to top. The right leg is a
# branch that merges into the stem at the flat shoulder; the solid dot on top is HEAD.
STEM = "M184 168V392"
SHOULDER = "M184 264H284C310 264 328 282 328 308V392"
HEAD = (184, 124)
COMMITS = [(184, 264), (328, 388), (184, 388)]  # merge, branch commit, root commit


def mark(ink, hole, *, small=False):
    """The h in `ink`; commit rings are filled with `hole` (the colour behind the mark).

    The small version, for 32px and below, has heavier lines and no rings, which blur at that size.
    """
    width, head_r = (48, 46) if small else (40, 40)
    out = (
        f'<g fill="none" stroke="{ink}" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round">'
        f'<path d="{STEM}"/><path d="{SHOULDER}"/></g>'
    )
    if not small:
        out += "".join(
            f'<circle cx="{x}" cy="{y}" r="27" fill="{hole}" stroke="{ink}" stroke-width="15"/>' for x, y in COMMITS
        )
    return out + f'<circle cx="{HEAD[0]}" cy="{HEAD[1]}" r="{head_r}" fill="{ink}"/>'


def icon(name, *, bg, glyph_svg, border=None):
    edge = (
        f'<rect x="1" y="1" width="510" height="510" rx="111" fill="none" stroke="{border}" stroke-width="2"/>'
        if border
        else ""
    )
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">'
        f"<defs><clipPath id=\"sq\">{SQUIRCLE}</clipPath></defs>"
        f'<g clip-path="url(#sq)"><rect width="512" height="512" fill="{bg}"/>'
        f"{glyph_svg}</g>{edge}</svg>\n"
    )
    (OUT / f"{name}.svg").write_text(svg)


icon("headway-icon", bg=ROSE, glyph_svg=mark("#ffffff", ROSE))
icon("headway-icon-small", bg=ROSE, glyph_svg=mark("#ffffff", ROSE, small=True))
icon("headway-icon-dark", bg="#1b1f27", glyph_svg=mark("#ffffff", "#1b1f27"))
icon("headway-icon-light", bg="#ffffff", border="#e5e7eb", glyph_svg=mark(ROSE, "#ffffff"))

# Transparent single-colour glyph for tray icons, favicons and templates: the ring
# centres are cut out with a mask so whatever is behind shows through.
(OUT / "headway-glyph-mono.svg").write_text(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="64 64 384 384">'
    f'<mask id="m"><rect width="512" height="512" fill="#000"/>{mark("#fff", "#000")}</mask>'
    '<rect width="512" height="512" fill="#111827" mask="url(#m)"/></svg>\n'
)

# Horizontal lockup: icon + wordmark. Text uses system fonts; outline it for print.
inner = (OUT / "headway-icon.svg").read_text().split(">", 1)[1].rsplit("</svg>", 1)[0]
for variant, ink, sub in [("light", "#111827", "#6b7280"), ("dark", "#ffffff", "#9ca3af")]:
    page_bg = "#ffffff" if variant == "light" else "#0b1220"
    (OUT / f"headway-lockup-{variant}.svg").write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 360">'
        f'<rect width="1200" height="360" fill="{page_bg}"/>'
        f'<svg x="60" y="60" width="240" height="240" viewBox="0 0 512 512">{inner}</svg>'
        f'<text x="340" y="222" font-family="Inter, \'Plus Jakarta Sans\', \'Helvetica Neue\', Arial, sans-serif" '
        f'font-size="150" font-weight="700" letter-spacing="-4" fill="{ink}">Headway</text>'
        f'<text x="346" y="280" font-family="Inter, \'Helvetica Neue\', Arial, sans-serif" font-size="24" '
        f'letter-spacing="9" fill="{sub}">THE NATIVE GIT GUI</text></svg>\n'
    )

print("\n".join(sorted(p.name for p in OUT.iterdir())))


# ---- Export app icon files into the Rust project -------------------------
import struct
import subprocess

APP_ICONS = Path(__file__).parent.parent / "icon"
APP_ICONS.mkdir(parents=True, exist_ok=True)


def png(size):
    """Render one size; the simplified glyph reads better at 32px and below."""
    src = OUT / ("headway-icon-small.svg" if size <= 32 else "headway-icon.svg")
    dst = APP_ICONS / f"headway-{size}.png"
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), str(src), "-o", str(dst)], check=True)
    return dst


pngs = {s: png(s) for s in (16, 32, 48, 64, 128, 256, 512, 1024)}
subprocess.run(["magick", *(str(pngs[s]) for s in (16, 32, 48, 256)), str(APP_ICONS / "headway.ico")], check=True)

# macOS .icns: a header followed by PNG chunks tagged with their slot type.
ICNS_SLOTS = [("icp4", 16), ("icp5", 32), ("icp6", 64), ("ic07", 128), ("ic08", 256),
              ("ic09", 512), ("ic10", 1024), ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
chunks = b"".join(
    tag.encode() + struct.pack(">I", 8 + len(data)) + data
    for tag, size in ICNS_SLOTS
    for data in [pngs[size].read_bytes()]
)
(APP_ICONS / "headway.icns").write_bytes(b"icns" + struct.pack(">I", 8 + len(chunks)) + chunks)
(APP_ICONS / "headway.svg").write_text((OUT / "headway-icon.svg").read_text())
