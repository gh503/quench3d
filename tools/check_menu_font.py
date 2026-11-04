#!/usr/bin/env python3
"""Regression for the editor's 渲染 menu glyphs (requires fonttools)."""
from pathlib import Path
import sys
from fontTools.ttLib import TTFont

path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1] / 'crates/quench3d-editor/assets/ui-font.ttf'
with TTFont(path) as font:
    cmap = font.getBestCmap()
    missing = [f'{char} U+{ord(char):04X}' for char in '渲染' if ord(char) not in cmap]
if missing:
    print('FAIL: menu font missing ' + ', '.join(missing), file=sys.stderr)
    sys.exit(1)
print('PASS: menu font contains 渲 U+6E32 and 染 U+67D3')
