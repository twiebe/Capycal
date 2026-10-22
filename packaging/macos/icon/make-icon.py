"""Draw the Capycal app icon: capycal.svg and AppIcon.iconset/ next to this file,
and the favicons and menu bar logo of the book in docs/theme/ and docs/src/.
Also the tray icon of both desktop apps: a calendar sheet with the days of a
month, as GTK symbolic SVGs and as PDFs for the macOS menu bar.

A capybara relaxing in a hot spring with a yuzu, a calendar headed "CAL" on
its head. "CAL" is drawn as outlines from Inter Display Bold (https://rsms.me/inter/, SIL Open Font License), so
the SVG needs no font.

Usage: python3 make-icon.py <path to InterDisplay-Bold.ttf>
Needs: pip install fonttools cairosvg shapely
"""

import os
import sys

import cairosvg
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont
from shapely.geometry import Point, box
from shapely.ops import unary_union

HERE = os.path.dirname(os.path.abspath(__file__))
BOOK_THEME = os.path.join(HERE, "..", "..", "..", "docs", "theme")
BOOK_SRC = os.path.join(HERE, "..", "..", "..", "docs", "src")
GTK_ICONS = os.path.join(HERE, "..", "..", "..", "src", "bin", "capycal-app", "icons")
UI_ASSETS = os.path.join(HERE, "..", "..", "..", "src", "bin", "capycal-ui", "assets")
FONT = TTFont(sys.argv[1])
GLYPHS = FONT.getGlyphSet()
CMAP = FONT.getBestCmap()
CAP_HEIGHT = FONT["OS/2"].sCapHeight
UNITS = FONT["head"].unitsPerEm

PINK_TOP, PINK_BOTTOM = "#F07EC0", "#B83C8A"
FUR_TOP, FUR, FUR_DARK, MUZZLE, INK = "#B98A60", "#A87A52", "#86603F", "#6B4A30", "#3A2A20"
# Days with events, as (row, column) of the month grid
DAYS = {(0, 2): "#F59E0B", (1, 0): "#10B981", (1, 5): "#3B82F6", (2, 3): "#10B981"}
EMPTY_DAY = "#ECEEF2"


def text_path(label, cx, cy, cap_height, tracking, fill):
    """label as outlines, centered on (cx, cy) by its cap height"""
    scale = cap_height / CAP_HEIGHT
    paths, x = [], 0.0
    names = [CMAP[ord(ch)] for ch in label]
    for i, name in enumerate(names):
        pen = SVGPathPen(GLYPHS)
        GLYPHS[name].draw(pen)
        paths.append(f'<path transform="translate({x:.1f} 0)" d="{pen.getCommands()}"/>')
        x += GLYPHS[name].width + (tracking * UNITS if i + 1 < len(names) else 0)
    left = cx - x * scale / 2
    return (f'<g fill="{fill}" transform="translate({left:.1f} {cy + cap_height / 2:.1f}) '
            f'scale({scale:.5f} {-scale:.5f})">' + "".join(paths) + "</g>")


def calendar(x, y, w, h, rotation):
    """A calendar sheet headed CAL with three weeks of days"""
    r = w * 0.14
    hh = h * 0.34
    parts = [
        f'<g transform="rotate({rotation} {x + w / 2} {y + h / 2})">',
        f'<rect x="{x}" y="{y + 10}" width="{w}" height="{h}" rx="{r}" fill="#000" '
        f'opacity="0.18" filter="url(#blur)"/>',
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="#fff"/>',
        f'<clipPath id="cal"><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}"/></clipPath>',
        f'<rect x="{x}" y="{y}" width="{w}" height="{hh}" fill="#7C3AED" clip-path="url(#cal)"/>',
        text_path("CAL", x + w / 2, y + hh / 2, hh * 0.48, 0.04, "#fff"),
    ]
    cols, rows = 7, 3
    pad, gap = w * 0.09, w * 0.03
    cell_w = (w - 2 * pad - (cols - 1) * gap) / cols
    cell_h = (h - hh - 1.6 * pad - (rows - 1) * gap) / rows
    for row in range(rows):
        for col in range(cols):
            cx = x + pad + col * (cell_w + gap)
            cy = y + hh + 0.8 * pad + row * (cell_h + gap)
            parts.append(f'<rect x="{cx:.1f}" y="{cy:.1f}" width="{cell_w:.1f}" height="{cell_h:.1f}" '
                         f'rx="{cell_w * 0.22:.1f}" fill="{DAYS.get((row, col), EMPTY_DAY)}"/>')
    parts.append("</g>")
    return "\n".join(parts)


CAPYBARA = f'''<!-- body and head, peeking out of the water -->
<ellipse cx="512" cy="900" rx="330" ry="150" fill="{FUR_DARK}"/>
<path d="M300 470 C300 370 360 345 512 345 C664 345 724 370 724 470 L724 760
         C724 850 640 905 512 905 C384 905 300 850 300 760 Z" fill="url(#fur)"/>
<ellipse cx="322" cy="392" rx="32" ry="24" fill="{FUR_DARK}" transform="rotate(-25 322 392)"/>
<ellipse cx="702" cy="392" rx="32" ry="24" fill="{FUR_DARK}" transform="rotate(25 702 392)"/>
<ellipse cx="325" cy="396" rx="15" ry="10" fill="{MUZZLE}" transform="rotate(-25 325 396)"/>
<ellipse cx="699" cy="396" rx="15" ry="10" fill="{MUZZLE}" transform="rotate(25 699 396)"/>
<!-- content, closed eyes and soft cheeks -->
<path d="M372 520 Q402 544 432 520" fill="none" stroke="{INK}" stroke-width="14" stroke-linecap="round"/>
<path d="M592 520 Q622 544 652 520" fill="none" stroke="{INK}" stroke-width="14" stroke-linecap="round"/>
<ellipse cx="364" cy="585" rx="34" ry="24" fill="{PINK_TOP}" opacity="0.35"/>
<ellipse cx="660" cy="585" rx="34" ry="24" fill="{PINK_TOP}" opacity="0.35"/>
<path d="M362 680 C362 615 420 600 512 600 C604 600 662 615 662 680 L662 740 C662 800 600 815 512 815
         C424 815 362 800 362 740 Z" fill="{MUZZLE}"/>
<ellipse cx="468" cy="652" rx="17" ry="10" fill="{INK}" transform="rotate(20 468 652)"/>
<ellipse cx="556" cy="652" rx="17" ry="10" fill="{INK}" transform="rotate(-20 556 652)"/>
<path d="M512 690 L512 718 M512 718 Q490 740 470 728 M512 718 Q534 740 554 728" fill="none" stroke="{INK}"
      stroke-width="9" stroke-linecap="round"/>'''

# Hot spring water with a yuzu, and steam on both sides
WATER = '''<path d="M100 790 C180 760 260 820 340 790 C420 760 500 820 580 790 C660 760 740 820 820 790
         C870 772 900 780 924 790 L924 924 L100 924 Z" fill="#BDE8F2" opacity="0.92"/>
<path d="M100 830 C180 800 260 860 340 830 C420 800 500 860 580 830 C660 800 740 860 820 830 C870 812 900 820 924 830"
      fill="none" stroke="#fff" stroke-width="8" opacity="0.6"/>
<circle cx="800" cy="830" r="42" fill="#F5B82E"/><circle cx="790" cy="818" r="10" fill="#fff" opacity="0.5"/>
<path d="M190 700 C170 670 210 650 190 620 C170 590 210 570 190 540" fill="none" stroke="#fff" stroke-width="12"
      stroke-linecap="round" opacity="0.55"/>
<path d="M840 680 C820 650 860 630 840 600 C820 570 860 550 840 520" fill="none" stroke="#fff" stroke-width="12"
      stroke-linecap="round" opacity="0.55"/>'''


def icon():
    # The body follows Apple's icon grid: an 824 pt rounded square in 1024
    return f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
<defs>
  <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{PINK_TOP}"/><stop offset="1" stop-color="{PINK_BOTTOM}"/>
  </linearGradient>
  <linearGradient id="fur" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{FUR_TOP}"/><stop offset="1" stop-color="{FUR}"/>
  </linearGradient>
  <filter id="blur" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="22"/></filter>
  <filter id="shadow" x="-10%" y="-10%" width="120%" height="125%"><feGaussianBlur stdDeviation="12"/></filter>
  <clipPath id="squircle"><rect x="100" y="100" width="824" height="824" rx="185"/></clipPath>
</defs>
<rect x="100" y="112" width="824" height="824" rx="185" fill="#000" opacity="0.28" filter="url(#shadow)"/>
<rect x="100" y="100" width="824" height="824" rx="185" fill="url(#bg)"/>
<g clip-path="url(#squircle)">
{CAPYBARA}
{WATER}
</g>
{calendar(392, 168, 240, 190, -7)}
</svg>
'''


def rounded_box(left, top, right, bottom, r):
    return box(left + r, top + r, right - r, bottom - r).buffer(r, quad_segs=16)


def tray_calendar():
    """The tray calendar in a 16 x 16 box: an outlined sheet with a filled
    header and a month of days (5, 5, 3) on a tight grid"""
    left, top, right, bottom, radius, header, line = 0.8, 1.35, 15.2, 14.65, 2.3, 4.7, 1.4
    sheet = rounded_box(left, top, right, bottom, radius)
    inner = radius - line
    # the page below the header, round only at the bottom
    page = unary_union([
        box(left + line, header, right - line, bottom - line - inner),
        rounded_box(left + line, header, right - line, bottom - line, inner),
    ])
    pitch, day = 2.2, 0.6
    cx, cy = (left + right) / 2, (header + bottom - line) / 2
    days = [Point(cx + (col - 2) * pitch, cy + (row - 1) * pitch).buffer(day, quad_segs=8)
            for row, count in enumerate((5, 5, 3)) for col in range(count)]
    return unary_union([sheet.difference(page)] + days)


TRAY_BADGE = (12.2, 12.4, 3.0, 4.4)  # center, dot radius, radius of the gap around it


def path_data(geometry):
    """SVG path data of a (multi)polygon, holes included; fill it evenodd"""
    rings = []
    for polygon in getattr(geometry, "geoms", [geometry]):
        for ring in [polygon.exterior, *polygon.interiors]:
            points = list(ring.coords)[:-1]
            rings.append("M" + " L".join(f"{x:.2f} {y:.2f}" for x, y in points) + "Z")
    return " ".join(rings)


def tray_icons():
    x, y, dot, gap = TRAY_BADGE
    calendar = tray_calendar()
    cut = path_data(calendar.difference(Point(x, y).buffer(gap, quad_segs=16)))
    whole = path_data(calendar)
    style = ('<style id="current-color-scheme" type="text/css">.ColorScheme-Text{color:#232629}'
             '.ColorScheme-NegativeText{color:#da4453}</style>')

    def svg(body):
        return f'<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{body}</svg>\n'

    def sheet(d, extra=""):
        return f'<path class="ColorScheme-Text" fill="currentColor" fill-rule="evenodd"{extra} d="{d}"/>'

    def badge(cls):
        return f'<circle class="{cls}" fill="currentColor" cx="{x}" cy="{y}" r="{dot}"/>'

    gtk = {
        "": sheet(whole),
        "-syncing": sheet(cut) + badge("ColorScheme-Text"),
        "-error": sheet(cut) + badge("ColorScheme-NegativeText error"),
        "-inactive": sheet(whole, ' opacity="0.5"'),
    }
    for suffix, body in gtk.items():
        with open(os.path.join(GTK_ICONS, f"capycal-tray{suffix}-symbolic.svg"), "w") as f:
            f.write(svg(style + body))
    # macOS draws these as template images; the app adds the red dot of the error state
    mac = {
        "tray": f'<path fill-rule="evenodd" d="{whole}"/>',
        "tray-syncing": f'<path fill-rule="evenodd" d="{cut}"/><circle cx="{x}" cy="{y}" r="{dot}"/>',
        "tray-error": f'<path fill-rule="evenodd" d="{cut}"/>',
    }
    for name, body in mac.items():
        cairosvg.svg2pdf(bytestring=svg(body).encode(), write_to=os.path.join(UI_ASSETS, f"{name}.pdf"))


def main():
    tray_icons()
    svg = icon()
    with open(os.path.join(HERE, "capycal.svg"), "w") as f:
        f.write(svg)
    iconset = os.path.join(HERE, "AppIcon.iconset")
    os.makedirs(iconset, exist_ok=True)
    os.makedirs(BOOK_THEME, exist_ok=True)
    for path in (os.path.join(BOOK_THEME, "favicon.svg"), os.path.join(BOOK_SRC, "logo.svg")):
        with open(path, "w") as f:
            f.write(svg)
    cairosvg.svg2png(bytestring=svg.encode(), write_to=os.path.join(BOOK_THEME, "favicon.png"),
                     output_width=64, output_height=64)
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            suffix = "@2x" if scale == 2 else ""
            path = os.path.join(iconset, f"icon_{size}x{size}{suffix}.png")
            cairosvg.svg2png(bytestring=svg.encode(), write_to=path,
                             output_width=size * scale, output_height=size * scale)


if __name__ == "__main__":
    main()
