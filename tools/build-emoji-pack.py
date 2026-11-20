#!/usr/bin/env python3
"""Builds assets/emoji-noto.pack (colour emoji, Noto style, 64 px WebP) from the npm package `emoji-datasource-google`.

  npm pack emoji-datasource-google@15.1.2 && tar xzf emoji-datasource-google-*.tgz
  mkdir webp && for f in package/img/google/64/*.png; do convert $f -quality 86 -define webp:alpha-quality=95 webp/$(basename $f .png).webp; done
  python3 tools/build-emoji-pack.py webp assets/emoji-noto.pack

Format: "LEMO" | u32 count | count x (u8 keylen, key, u32 offset, u32 len) | blobs.  Keys are lower-case hex code points joined by '-',
without leading zeros and without U+FE0F (so "❤" and "❤️" share one picture). Noto Color Emoji: Apache-2.0 (assets/NOTO-EMOJI-LICENSE.txt).
"""
import os, struct, sys
src, out = sys.argv[1], sys.argv[2]
items = {}
for f in sorted(os.listdir(src)):
    if not f.endswith('.webp'):
        continue
    parts = [p for p in f[:-5].split('-') if p.lower() != 'fe0f']
    key = '-'.join(format(int(p, 16), 'x') for p in parts)
    if key not in items:
        items[key] = open(os.path.join(src, f), 'rb').read()
keys = sorted(items)
index_len = sum(1 + len(k) + 8 for k in keys)
off = 8 + index_len
index = b''
blobs = b''
for k in keys:
    index += struct.pack('<B', len(k)) + k.encode() + struct.pack('<II', off + len(blobs), len(items[k]))
    blobs += items[k]
open(out, 'wb').write(b'LEMO' + struct.pack('<I', len(keys)) + index + blobs)
print(len(keys), 'emoji,', (8 + len(index) + len(blobs)) // 1024, 'KiB')
