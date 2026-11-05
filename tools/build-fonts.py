#!/usr/bin/env python3
"""Rebuilds assets/lucide.ttf (and verifies it).

* downloads lucide-static (the font that the code points in src/lucide.rs were generated from),
* keeps ONLY the glyphs that src/lucide.rs references (the font shrinks from ~850 KB to ~100 KB),
* FAILS when a constant in src/lucide.rs points at a code point the font does not contain —
  that is what shows up as an empty box / wrong symbol in the UI.

Inter (assets/Inter-*.otf) must have its private-use glyphs stripped, otherwise they shadow Lucide (see src/theme.rs).
Usage:  pip install fonttools brotli && python3 tools/build-fonts.py
"""
import re, subprocess, sys, tarfile, tempfile, pathlib
from fontTools.ttLib import TTFont
from fontTools import subset

root = pathlib.Path(__file__).resolve().parent.parent
src = (root / "src" / "lucide.rs").read_text(encoding="utf8")
cps = sorted({int(m, 16) for m in re.findall(r'"\\u\{([0-9a-f]+)\}"', src)})
tmp = pathlib.Path(tempfile.mkdtemp())
subprocess.run(["npm", "pack", "lucide-static@1.69.0"], cwd=tmp, check=True, capture_output=True)
with tarfile.open(next(tmp.glob("lucide-static-*.tgz"))) as t:
    t.extractall(tmp)
font = TTFont(tmp / "package" / "font" / "lucide.ttf")
have = font.getBestCmap()
missing = [hex(c) for c in cps if c not in have]
if missing:
    sys.exit(f"icons without a glyph: {missing}")
o = subset.Options(); o.layout_features = []; o.name_IDs = [1, 2, 3, 4, 6]; o.notdef_outline = True; o.hinting = False
s = subset.Subsetter(o); s.populate(unicodes=cps); s.subset(font)
font.save(root / "assets" / "lucide.ttf")
print(f"ok: {len(cps)} icons -> assets/lucide.ttf")

for name in ("Inter-Regular.otf", "Inter-Bold.otf"):
    pua = [c for c in TTFont(root / "assets" / name).getBestCmap() if 0xE000 <= c <= 0xF8FF]
    if pua:
        sys.exit(f"{name} still has {len(pua)} private-use glyphs — they would shadow Lucide icons")
print("ok: Inter has no private-use glyphs")
