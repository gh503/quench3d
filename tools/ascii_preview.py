#!/usr/bin/env python3
"""把渲染结果转成终端可见的 ASCII 预览，便于在没有图形界面时快速检查画面。

用法: python3 tools/ascii_preview.py <png> [列数] [行数]
"""
import sys
from PIL import Image
import numpy as np

RAMP = " .:-=+*#%@"
HUE_KEYS = [
    ("R", (200, 60, 60)),
    ("O", (220, 140, 60)),
    ("Y", (220, 210, 70)),
    ("G", (70, 190, 90)),
    ("C", (70, 200, 200)),
    ("B", (70, 110, 220)),
    ("P", (170, 80, 200)),
    ("W", (225, 225, 225)),
    ("w", (150, 150, 150)),
    ("k", (50, 50, 50)),
]


def classify(rgb):
    r, g, b = rgb
    mx, mn = max(rgb), min(rgb)
    if mx < 45:
        return "k"
    if mx - mn < 26:
        return "W" if mx > 170 else ("w" if mx > 80 else "k")
    best, bd = "?", 1e9
    for name, c in HUE_KEYS:
        d = sum((a - b2) ** 2 for a, b2 in zip(rgb, c))
        if d < bd:
            bd, best = d, name
    return best


def main():
    path = sys.argv[1]
    cols = int(sys.argv[2]) if len(sys.argv) > 2 else 78
    rows = int(sys.argv[3]) if len(sys.argv) > 3 else 30
    img = Image.open(path).convert("RGB")
    w, h = img.size
    # 字符单元大约是 2:1，做纵向补偿
    small = img.resize((cols, rows), Image.LANCZOS)
    arr = np.asarray(small).astype(np.float32)

    print(f"== {path}  {w}x{h} -> {cols}x{rows} ==")
    print("-- 亮度图 --")
    for y in range(rows):
        line = []
        for x in range(cols):
            r, g, b = arr[y, x]
            lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
            line.append(RAMP[min(len(RAMP) - 1, int(lum * len(RAMP)))])
        print("".join(line))

    print("-- 色相图（R红 O橙 Y黄 G绿 C青 B蓝 P紫 W亮灰 w灰 k暗）--")
    for y in range(rows):
        print("".join(classify(arr[y, x]) for x in range(cols)))

    full = np.asarray(img).astype(np.float32)
    print("-- 统计 --")
    print(f"平均 RGB: {full.reshape(-1,3).mean(axis=0).round(1)}")
    print(f"最亮像素: {full.reshape(-1,3).max(axis=0)}  最暗: {full.reshape(-1,3).min(axis=0)}")
    print(f"唯一颜色数: {len(np.unique(full.reshape(-1,3), axis=0))}")
    # 分带亮度，用于判断天空/地面/阴影
    for i, name in enumerate(["顶部 1/4", "上中 1/4", "下中 1/4", "底部 1/4"]):
        band = full[i * h // 4:(i + 1) * h // 4]
        print(f"{name} 平均 RGB: {band.reshape(-1,3).mean(axis=0).round(1)}")
    # 阴影检测：下半部分较暗像素占比
    lower = full[h // 2:]
    lum = lower @ np.array([0.2126, 0.7152, 0.0722])
    print(f"下半部分暗像素(<60)占比: {(lum < 60).mean() * 100:.2f}%")
    print(f"高光像素(>240)占比: {(full.max(axis=2) > 240).mean() * 100:.2f}%")


if __name__ == "__main__":
    main()
