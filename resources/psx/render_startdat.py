# /// script
# requires-python = ">=3.10"
# dependencies = ["resvg-py==0.5.0", "pillow==13.0.0"]
# ///
"""Render startdat.svg to the 480x272 8-bit greyscale startdat.png the PS1
EBOOT builder embeds. Run with `uv run resources/psx/render_startdat.py`.

The two OFL fonts the artwork names are fetched from a pinned google/fonts
commit and passed to the renderer directly, so no system font is used.
"""

import hashlib
import io
import tempfile
import urllib.request
from pathlib import Path

import resvg_py
from PIL import Image

FONTS_COMMIT = "e7db6ca603fd4862010383ff17ffd6cc4ffac1c0"
FONTS = ("spacegrotesk/SpaceGrotesk%5Bwght%5D.ttf", "jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf")
WIDTH, HEIGHT = 480, 272
PSX_DIR = Path(__file__).resolve().parent


def fetch_fonts(target: Path) -> list[str]:
    paths = []
    for font in FONTS:
        url = f"https://raw.githubusercontent.com/google/fonts/{FONTS_COMMIT}/ofl/{font}"
        path = target / font.rsplit("/", 1)[1].replace("%5B", "[").replace("%5D", "]")
        with urllib.request.urlopen(url) as response:
            path.write_bytes(response.read())
        paths.append(str(path))
    return paths


def main() -> None:
    with tempfile.TemporaryDirectory() as fonts_dir:
        rgba = resvg_py.svg_to_bytes(
            svg_path=str(PSX_DIR / "startdat.svg"),
            width=WIDTH,
            height=HEIGHT,
            skip_system_fonts=True,
            font_files=fetch_fonts(Path(fonts_dir)),
        )
    grey = Image.open(io.BytesIO(bytes(rgba))).convert("L")
    out = io.BytesIO()
    grey.save(out, format="PNG", optimize=True)
    (PSX_DIR / "startdat.png").write_bytes(out.getvalue())
    print(f"startdat.png {len(out.getvalue())} bytes sha256 {hashlib.sha256(out.getvalue()).hexdigest()}")


if __name__ == "__main__":
    main()
