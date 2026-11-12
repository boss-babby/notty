#!/usr/bin/env bash
# Fetches the binary assets that are not stored in git: Inter (private-use glyphs stripped), Lucide subset, Noto Sans scripts, PWA icons.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p assets web/icons web/fonts
TMP=$(mktemp -d)
if [ ! -f assets/Inter-Regular.otf ]; then
  curl -fsSL -o "$TMP/inter.zip" https://github.com/rsms/inter/releases/download/v4.0/Inter-4.0.zip
  unzip -q -o "$TMP/inter.zip" -d "$TMP/inter"
  for w in Regular Bold; do cp "$(find "$TMP/inter" -name "Inter-$w.otf" | head -1)" "assets/Inter-$w.otf"; done
  python3 - <<'PY'
from fontTools.ttLib import TTFont
from fontTools import subset
for n in ("Regular", "Bold"):
    p = f"assets/Inter-{n}.otf"
    f = TTFont(p)
    keep = [c for c in f.getBestCmap() if not (0xE000 <= c <= 0xF8FF)]
    o = subset.Options(); o.layout_features = ["*"]; o.name_IDs = ["*"]; o.notdef_outline = True; o.glyph_names = False; o.hinting = False
    s = subset.Subsetter(o); s.populate(unicodes=keep); s.subset(f); f.save(p)
PY
fi
[ -f assets/lucide.ttf ] || python3 tools/build-fonts.py || true
BASE=https://raw.githubusercontent.com/notofonts/notofonts.github.io/main/fonts
for n in Bengali Devanagari Arabic Hebrew Thai Tamil Telugu Kannada Malayalam Gujarati Gurmukhi Oriya Sinhala Khmer Lao Myanmar Armenian Georgian Ethiopic; do
  f="assets/NotoSans$n-Regular.ttf"
  [ -s "$f" ] && continue
  curl -fsSL -o "$f" "$BASE/NotoSans$n/hinted/ttf/NotoSans$n-Regular.ttf" || curl -fsSL -o "$f" "$BASE/NotoSans$n/unhinted/ttf/NotoSans$n-Regular.ttf" || echo "FAILED $n"
done
python3 tools/make-icons.py
ls -la assets web/icons
