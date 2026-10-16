#!/usr/bin/env python3
"""聚光灯阴影（第 54 轮）的本机 GPU 探针。

为什么需要它：WGSL 里的错误**编译期抓不到**（naga 只在运行时才会报），而且"影子对不对"
这件事根本不能用眼睛验收——能看出"这里有个暗斑"，看不出"这个暗斑是不是在正确的方向、
是不是只在遮挡物后面出现、打开开关但不放遮挡物时会不会满屏自阴影"。

做法：用 CLI 的 `render` 渲 10 张图（同一套场景，只改一个变量），用纯 python 读 PNG 统计。
用的是**对照 + 差分**：每个"开关打开"的图都有一张"只关掉这一个变量"的对照图。

判据（全部是量化断言，不是"看着像"）：
  1. 不放遮挡物时，投影开/关**逐像素相同**（0 个像素差）——这一条同时证明了
     阴影相机/偏移是对的：只要偏移不够，地面自己就会投出满屏噪点；
  2. 放遮挡物时，投影开/关有差异，且**只有变暗**（没有变亮），最大变暗量可观；
  3. 变暗区域的**重心在遮挡物的 +x 侧**（光源在 -x 侧、朝 +x 照）——方向对；
  4. 把遮挡物沿 -x 挪 2 米，暗区重心跟着往左挪——影子跟着遮挡物走；
  5. 把光源镜像到 +x 侧，暗区重心翻到遮挡物的 -x 侧——影子跟着**光**走，不是固定方向；
     且镜像前后偏移**严格互为相反数**（场景与相机都关于 x=0 对称）；
  6. 在聚光灯**前面**插一盏不投影的点光源（占掉第 0 个槽位），暗区位置与面积不变
     ——"点光源优先占槽位"这条下标规则在 pack → uniform → 着色器整条链路上是对的。

用法：
    python3 tools/probe_spot_shadow.py --cli target/debug/quench3d
退出码 0 = 全部判据通过；1 = 有判据失败（并打印原始数字）。
"""

import argparse
import json
import pathlib
import struct
import subprocess
import sys
import zlib

# ---------------------------------------------------------------- PNG 写（拼对照图）

def write_png(path, w, h, rgb):
    """写 8 位 RGB PNG（只用来拼对照图，不参与判据）。"""
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += rgb[y * w * 3 : (y + 1) * w * 3]
    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
    out = b"\x89PNG\r\n\x1a\n"
    out += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    out += chunk(b"IDAT", zlib.compress(bytes(raw), 6))
    out += chunk(b"IEND", b"")
    pathlib.Path(path).write_bytes(out)


def side_by_side(paths, labels, out_path, gap=4):
    """把若干张同尺寸图横向拼起来（人眼验收用；判据仍然是纯数字）。"""
    ims = [Image(p) for p in paths]
    w, h, bpp = ims[0].w, ims[0].h, ims[0].bpp
    total_w = w * len(ims) + gap * (len(ims) - 1)
    buf = bytearray(b"\x20" * (total_w * h * 3))
    for k, im in enumerate(ims):
        x0 = k * (w + gap)
        for y in range(h):
            for x in range(w):
                i = (y * w + x) * bpp
                o = (y * total_w + x0 + x) * 3
                buf[o : o + 3] = im.px[i : i + 3]
    write_png(out_path, total_w, h, bytes(buf))
    print("对照图：" + str(out_path) + "  （左→右：" + " | ".join(labels) + "）")


def diff_panel(a, b, out_path, gain=4):
    """把"开/关投影"的差异放大画出（红=变暗），用来人眼确认暗区形状。"""
    im_a = Image(a)
    im_b = Image(b)
    w, h = im_a.w, im_a.h
    buf = bytearray(b"\x00" * (w * h * 3))
    mask = im_a.dark_mask(im_b, thr=2.0)
    for i in mask:
        v = min(255, int((luma(im_b.px, i, im_b.bpp) - luma(im_a.px, i, im_a.bpp)) * gain))
        buf[i * 3] = v
        buf[i * 3 + 1] = v // 4
        buf[i * 3 + 2] = v // 8
    for i in range(w * h):
        if i not in set(mask):
            g = int(luma(im_a.px, i, im_a.bpp) * 0.25)
            buf[i * 3 : i * 3 + 3] = bytes((g, g, g))
    write_png(out_path, w, h, bytes(buf))
    print(f"差异图：{out_path}（红=被遮挡变暗，放大 {gain} 倍，暗区 {len(mask)} px）")


# ---------------------------------------------------------------- PNG 读取

def read_png(path):
    """纯 python 读 8 位 RGB/RGBA PNG（含 5 种 filter）。返回 (w, h, bpp, bytes)。"""
    data = pathlib.Path(path).read_bytes()
    pos, w, h, idat, ct = 8, 0, 0, b"", 6
    while pos < len(data):
        ln = struct.unpack(">I", data[pos : pos + 4])[0]
        typ = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + ln]
        if typ == b"IHDR":
            w, h, bd, ct = struct.unpack(">IIBB", chunk[:10])
            assert bd == 8, f"只支持 8 位 PNG，实际 {bd}"
        elif typ == b"IDAT":
            idat += chunk
        pos += 12 + ln
    raw = zlib.decompress(idat)
    bpp = 4 if ct == 6 else 3
    stride = w * bpp
    out = bytearray()
    prev = bytearray(stride)
    i = 0
    for _ in range(h):
        f = raw[i]
        i += 1
        line = bytearray(raw[i : i + stride])
        i += stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        out += line
        prev = line
    return w, h, bpp, bytes(out)


def luma(px, i, bpp):
    r, g, b = px[i * bpp], px[i * bpp + 1], px[i * bpp + 2]
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


class Image:
    def __init__(self, path):
        self.path = str(path)
        self.w, self.h, self.bpp, self.px = read_png(path)

    def _check(self, other):
        assert (self.w, self.h, self.bpp) == (other.w, other.h, other.bpp), "尺寸/通道不一致"

    def diff(self, other):
        """返回 (变化像素数, 最大变暗量, 最大变亮量)。"""
        self._check(other)
        n = self.w * self.h
        changed = 0
        max_dark = 0.0
        max_light = 0.0
        for i in range(n):
            # d > 0 表示 self（"开了投影"那张）比 other（对照）更暗
            d = luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp)
            if abs(d) > 1e-9:
                changed += 1
            max_dark = max(max_dark, d)
            max_light = max(max_light, -d)
        return changed, max_dark, max_light

    def dark_mask(self, other, thr=3.0):
        """相对 `other` 变暗超过 thr 的像素集合。"""
        self._check(other)
        out = []
        for i in range(self.w * self.h):
            if luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp) > thr:
                out.append(i)
        return out

    def changed_mask(self, other, thr=2.0):
        self._check(other)
        out = []
        for i in range(self.w * self.h):
            if abs(luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp)) > thr:
                out.append(i)
        return out

    def centroid(self, mask):
        if not mask:
            return None
        sx = sum(i % self.w for i in mask) / len(mask)
        sy = sum(i // self.w for i in mask) / len(mask)
        return sx, sy

    def save_mask_png(self, mask, path):
        """把掩膜写成 PGM（不用第三方库也能看；纯调试用）。"""
        s = set(mask)
        rows = []
        for y in range(self.h):
            rows.append(bytes(255 if (y * self.w + x) in s else 0 for x in range(self.w)))
        pathlib.Path(path).write_bytes(b"P5\n%d %d\n255\n" % (self.w, self.h) + b"".join(rows))


# ---------------------------------------------------------------- 场景

def scene(out_dir, name, *, occluder_x=None, light_x=-3.0, cast=False, png=None, point=False):
    """同一套场景，只改一个变量。

    * 隔离光源：`preset: none` + `ambient: 0` + `sun_intensity: 0` + 天空色全 0
      （第 49 轮的教训：天空色贡献 65% 的环境光，"关掉太阳"不等于黑场景）；
    * 地面 40×40，一盏 4000 强度的聚光灯在 `light_x` 侧朝 +x/-x 斜照；
    * 遮挡物是一个悬空（static，不参与模拟）的 2×2×2 盒子，`occluder_x` 给它 x 位置；
    * `cast` 就是聚光灯的 `cast_shadows`。
    """
    cfg = {
        "ground": {"size": 40, "material": "floor"},
        "materials": {"floor": {"color": [0.62, 0.62, 0.62], "roughness": 0.95}},
        "camera": {"position": [0.0, 14.0, 16.0], "look_at": [0.0, 0.0, 0.0], "fov_deg": 45.0},
        "lighting": {
            "preset": "none",
            "sun_intensity": 0.0,
            "ambient": 0.0,
            "sky": False,
            "grid": False,
            "sky_colors": {"zenith": [0, 0, 0], "horizon": [0, 0, 0], "ground": [0, 0, 0]},
            "spot_lights": [
                {
                    "position": [light_x, 9.0, 0.0],
                    # 朝原点照：方向 = (0 - light_x, 0 - 9, 0)
                    "direction": [-light_x, -9.0, 0.0],
                    "color": [1.0, 1.0, 1.0],
                    "intensity": 4000.0,
                    "radius": 40.0,
                    "inner_deg": 20.0,
                    "outer_deg": 30.0,
                    "cast_shadows": cast,
                }
            ],
        },
        "render": {
            "width": 320,
            "height": 320,
            "msaa": 1,
            "output": str(png),
        },
    }
    if point:
        # 一盏**不投影**的点光源排在聚光灯前面：验证"点光源优先占槽位"这条规则
        # 在整条链路上（pack → uniform 下标 → 着色器比较）都对得上。
        # 它偏在一边、强度低，只是给地面加一点底光，不产生任何暗区。
        cfg["lighting"]["point_lights"] = [
            {
                "position": [10.0, 9.0, 4.0],
                "color": [1.0, 0.9, 0.8],
                "intensity": 200.0,
                "radius": 30.0,
            }
        ]
    if occluder_x is not None:
        cfg["objects"] = [
            {
                "name": "遮挡物",
                "shape": {"type": "box", "size": [2, 2, 2]},
                "position": [occluder_x, 3.0, 0.0],
                "material": "floor",
                "body": "static",
            }
        ]
    p = pathlib.Path(out_dir) / f"{name}.json"
    p.write_text(json.dumps(cfg, indent=2))
    return p


# ---------------------------------------------------------------- 主流程

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", default="target/debug/quench3d", help="quench3d 可执行文件")
    ap.add_argument("--out", default="target/tmp/probe54", help="输出目录")
    ap.add_argument("--keep", action="store_true", help="保留中间 PNG/PGM")
    args = ap.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    out = (root / args.out) if not pathlib.Path(args.out).is_absolute() else pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    cli = pathlib.Path(args.cli)
    if not cli.is_absolute():
        cli = root / cli
    if not cli.exists():
        print(f"找不到 CLI：{cli}", file=sys.stderr)
        return 2

    cases = {
        # 名字: (遮挡物 x, 光源 x, cast_shadows)
        "noocc_off": (None, -3.0, False),
        "noocc_on": (None, -3.0, True),
        "occ_off": (0.0, -3.0, False),
        "occ_on": (0.0, -3.0, True),
        "occ2_off": (-2.0, -3.0, False),
        "occ2_on": (-2.0, -3.0, True),
        "mirror_off": (0.0, 3.0, False),
        "mirror_on": (0.0, 3.0, True),
        "mix_off": (0.0, -3.0, False),
        "mix_on": (0.0, -3.0, True),
    }
    images = {}
    for name, (ox, lx, cast) in cases.items():
        png = out / f"{name}.png"
        # mix_* 这两个场景多一盏**不投影**的点光源，排在聚光灯前面（占掉第 0 个槽位），
        # 用来验证"聚光灯的 uniform 下标 = 点光源个数 + 它在聚光灯里的序号"
        doc = scene(
            out,
            name,
            occluder_x=ox,
            light_x=lx,
            cast=cast,
            png=png,
            point=name.startswith("mix_"),
        )
        r = subprocess.run(
            [str(cli), "render", str(doc)], capture_output=True, text=True, cwd=str(root)
        )
        if r.returncode != 0 or not png.exists():
            print(f"[{name}] 渲染失败（exit {r.returncode}）", file=sys.stderr)
            print(r.stdout[-2000:], r.stderr[-2000:], file=sys.stderr)
            return 2
        images[name] = Image(png)
        print(f"[{name}] {png.name} 已渲染")

    failures = []

    def check(name, ok, detail):
        print(f"{'PASS' if ok else 'FAIL'}  {name}: {detail}")
        if not ok:
            failures.append(name)

    # ---- 1. 对照：没有遮挡物时，投影开关必须逐像素相同 ----
    changed, dark, light = images["noocc_on"].diff(images["noocc_off"])
    check(
        "无遮挡物时开投影不改变任何像素",
        changed == 0,
        f"不同像素 {changed}/{images['noocc_off'].w * images['noocc_off'].h}"
        f"（最大变暗 {dark:.1f}、最大变亮 {light:.1f}）",
    )

    # ---- 2. 有遮挡物：差异必须存在、且只有变暗 ----
    changed, dark, light = images["occ_on"].diff(images["occ_off"])
    total = images["occ_off"].w * images["occ_off"].h
    check(
        "有遮挡物时开投影出现暗区",
        changed > 200,
        f"不同像素 {changed}（{changed / total * 100:.2f}%），最大变暗 {dark:.1f}",
    )
    check(
        "差异只有变暗（没有变亮）",
        light <= 1.0,
        f"最大变亮 {light:.1f}（<= 1 灰阶才算只有变暗）",
    )
    check("暗区足够明显", dark >= 20.0, f"最大变暗 {dark:.1f} 灰阶")

    # ---- 3. 方向：光源在 -x 侧，暗区重心必须落在遮挡物的 +x 侧 ----
    occ_screen = images["occ_off"].changed_mask(images["noocc_off"])
    dark_mask = images["occ_on"].dark_mask(images["occ_off"])
    occ_c = images["occ_off"].centroid(occ_screen)
    dark_c = images["occ_on"].centroid(dark_mask)
    assert occ_c and dark_c, "掩膜为空"
    check(
        "暗区重心在遮挡物的 +x 侧（朝光的方向）",
        dark_c[0] - occ_c[0] > 10.0,
        f"暗区重心 x = {dark_c[0]:.1f}，遮挡物重心 x = {occ_c[0]:.1f}，"
        f"偏移 {dark_c[0] - occ_c[0]:.1f} px（暗区 {len(dark_mask)} px）",
    )
    check(
        "暗区重心在遮挡物下方（影子落在地上）",
        dark_c[1] > occ_c[1],
        f"暗区重心 y = {dark_c[1]:.1f}，遮挡物重心 y = {occ_c[1]:.1f}",
    )

    # ---- 4. 影子跟着遮挡物走：遮挡物挪到 -2，暗区重心也要往左 ----
    dark2 = images["occ2_on"].dark_mask(images["occ2_off"])
    dark2_c = images["occ2_on"].centroid(dark2)
    occ2_screen = images["occ2_off"].changed_mask(images["noocc_off"])
    occ2_c = images["occ2_off"].centroid(occ2_screen)
    check(
        "遮挡物左移 2 米，暗区重心跟着左移",
        dark2_c[0] < dark_c[0] - 10.0,
        f"重心 x：{dark_c[0]:.1f} → {dark2_c[0]:.1f}（遮挡物 {occ_c[0]:.1f} → {occ2_c[0]:.1f}）",
    )
    # 偏移量**不该**一致：解析解 x_s - x_b = (h_floor - h_box)/(h_light - h_box) * (x_b - lx)
    # 说明遮挡物离光轴越远、世界空间偏移越小。所以这里只要求"两处都在 +x 侧且可测"。
    check(
        "两个位置下暗区都在遮挡物的 +x 侧",
        dark_c[0] - occ_c[0] > 5.0 and dark2_c[0] - occ2_c[0] > 5.0,
        f"偏移 {dark_c[0] - occ_c[0]:.1f} px（遮挡物 x=0）与 "
        f"{dark2_c[0] - occ2_c[0]:.1f} px（遮挡物 x=-2）",
    )
    # 解析解：世界空间偏移 1.5 m（x=0）与 0.5 m（x=-2），比值 3:1；像素侧还叠了透视缩放，
    # 所以只断言"偏移随遮挡物离开光轴而变小"（方向），并把两个数字都打印出来。
    check(
        "偏移量随遮挡物远离光轴而变小（解析解 1.5 m → 0.5 m）",
        (dark2_c[0] - occ2_c[0]) < (dark_c[0] - occ_c[0]),
        f"{dark_c[0] - occ_c[0]:.1f} px → {dark2_c[0] - occ2_c[0]:.1f} px",
    )

    # ---- 5. 镜像：光源挪到 +x 侧，暗区重心必须翻到遮挡物的 -x 侧 ----
    mirror_dark = images["mirror_on"].dark_mask(images["mirror_off"])
    mirror_c = images["mirror_on"].centroid(mirror_dark)
    check(
        "光源镜像到 +x 侧后暗区重心翻到 -x 侧",
        mirror_c[0] - occ_c[0] < -10.0,
        f"镜像暗区重心 x = {mirror_c[0]:.1f}，遮挡物重心 x = {occ_c[0]:.1f}",
    )
    # 场景与相机都关于 x=0 对称 → 镜像图的偏移必须是原图的**严格相反数**。
    # 这一条同时钉住了"V 翻转对不对""uv 有没有镜像错"：任何一处镜像错都会破坏它。
    off_a = dark_c[0] - occ_c[0]
    off_b = mirror_c[0] - occ_c[0]
    check(
        "镜像前后暗区偏移互为相反数（相机与场景都关于 x=0 对称）",
        abs(off_a + off_b) < 2.0,
        f"+{off_a:.1f} px vs {off_b:.1f} px（和 {off_a + off_b:.1f}）",
    )
    check(
        "镜像前后暗区面积一致（对称性）",
        abs(len(mirror_dark) - len(dark_mask)) <= max(4, len(dark_mask) // 50),
        f"{len(dark_mask)} px vs {len(mirror_dark)} px",
    )

    # ---- 6. 槽位：前面多一盏点光源时，影子必须还在**同一盏聚光灯**下面 ----
    # 点光源不投影，所以它的存在不该改变暗区的形状；如果 uniform 下标算错（少算/多算
    # 点光源的占位），这一盏聚光灯就会被当成别的灯，暗区要么消失、要么挪位。
    mix_dark = images["mix_on"].dark_mask(images["mix_off"])
    mix_c = images["mix_on"].centroid(mix_dark)
    check(
        "前面多一盏点光源后，暗区仍在同一位置（uniform 下标没错位）",
        mix_c is not None and abs(mix_c[0] - dark_c[0]) < 4.0 and abs(mix_c[1] - dark_c[1]) < 4.0,
        f"暗区重心 ({mix_c[0]:.1f}, {mix_c[1]:.1f}) vs ({dark_c[0]:.1f}, {dark_c[1]:.1f})，"
        f"面积 {len(mix_dark)} px vs {len(dark_mask)} px",
    )
    check(
        "点光源没有引入额外暗区（它本来就不投影）",
        abs(len(mix_dark) - len(dark_mask)) < max(40, len(dark_mask) // 20),
        f"{len(dark_mask)} px vs {len(mix_dark)} px",
    )

    if args.keep:
        images["occ_on"].save_mask_png(dark_mask, out / "dark_occ.pgm")
        images["mirror_on"].save_mask_png(mirror_dark, out / "dark_mirror.pgm")
        images["occ_off"].save_mask_png(occ_screen, out / "occ_screen.pgm")
        # 人眼验收用的对照图（判据仍然是上面的纯数字断言）
        side_by_side(
            [out / "occ_off.png", out / "occ_on.png", out / "noocc_on.png"],
            ["投影关", "投影开", "投影开但无遮挡物"],
            out / "compare.png",
        )
        diff_panel(out / "occ_on.png", out / "occ_off.png", out / "diff_dark.png")

    print()
    if failures:
        print(f"探针失败 ❌：{', '.join(failures)}")
        return 1
    print("探针全部通过 ✅（10 张图，6 组判据；图在 %s）" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
