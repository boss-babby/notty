#!/usr/bin/env python3
"""Builds web/fonts/NotoSans{SC,JP,KR}-subset.otf (the CJK fonts the WebAssembly build fetches in the background, see src/theme.rs).

Each subset keeps the characters of the national legacy standard (GB 2312 / JIS X 0208 / KS X 1001: 99.9 % of everyday text), kana, Hangul
syllables of KS X 1001, CJK / fullwidth punctuation and ASCII. Source: Noto Sans CJK subset OTFs (SIL OFL 1.1).
  pip install fonttools brotli && python3 tools/build-cjk-subsets.py
"""
import pathlib, sys, urllib.request
from fontTools.ttLib import TTFont
from fontTools import subset

root = pathlib.Path(__file__).resolve().parent.parent
out = root / "web" / "fonts"
out.mkdir(parents=True, exist_ok=True)
BASE = "https://github.com/notofonts/noto-cjk/raw/main/Sans/SubsetOTF"


def chars(codec, hi_lead, hi_trail=(0xA1, 0xFE)):
    s = set()
    for a in range(hi_lead[0], hi_lead[1] + 1):
        for b in range(hi_trail[0], hi_trail[1] + 1):
            try:
                s.add(ord(bytes([a, b]).decode(codec)))
            except Exception:
                pass
    return s


def sjis():
    s = set()
    for a in list(range(0x81, 0x9F)) + list(range(0xE0, 0xEF)):
        for b in range(0x40, 0xFD):
            try:
                s.add(ord(bytes([a, b]).decode("shift_jis")))
            except Exception:
                pass
    return s


common = set(range(0x20, 0x7F)) | set(range(0x3000, 0x3040)) | set(range(0xFF00, 0xFFF0)) | set(range(0x2000, 0x2070))
kana = set(range(0x3040, 0x3100)) | set(range(0x31F0, 0x3200))
sets = {
    "SC": ("SC", common | kana | chars("gb2312", (0xA1, 0xF7))),
    "JP": ("JP", common | kana | sjis()),
    "KR": ("KR", common | chars("euc_kr", (0xB0, 0xC8)) | chars("euc_kr", (0xCA, 0xFD))),
}
for key, (region, cps) in sets.items():
    dst = out / f"NotoSans{key}-subset.otf"
    tmp = pathlib.Path(f"/tmp/NotoSans{region}-Regular.otf")
    if not tmp.exists():
        print("download", region, file=sys.stderr)
        urllib.request.urlretrieve(f"{BASE}/{region}/NotoSans{region}-Regular.otf", tmp)
    font = TTFont(tmp)
    o = subset.Options()
    o.layout_features = ["kern", "vert", "vrt2", "locl"]
    o.name_IDs = [1, 2, 3, 4, 6]
    o.notdef_outline = True
    o.hinting = False
    o.desubroutinize = True
    s = subset.Subsetter(o)
    s.populate(unicodes=sorted(cps))
    s.subset(font)
    font.save(dst)
    print(dst.name, dst.stat().st_size // 1024, "KiB", len(cps), "code points")
