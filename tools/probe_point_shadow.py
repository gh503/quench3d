#!/usr/bin/env python3
"""点光源阴影（第 55 轮，立方体阴影）的本机 GPU 探针。

为什么需要它：WGSL 的错误编译期抓不到（naga 只在运行时才报），而"立方体阴影对不对"
根本不能用眼睛验收 —— 能看出"这里有个暗斑"，看不出"这个暗斑是不是落在正确的方向、
是不是把 ±X/±Z 四个面搞混了、是不是把点光与聚光的阴影图串了"。

做法：用 CLI 的 `render` 渲 16 张图（同一套场景，只改一个变量），用纯 python 读 PNG 统计。
全部判据都是**对照 + 差分 + 重心**，没有一条是"看着像"。

判据：
  1. 不放遮挡物时，投影开/关**逐像素相同**（0 个像素差）—— 这同时证明
     near/far、深度偏移、法线偏移是对的：偏移不够，地面自己就会投出满屏噪点；
  2. 放遮挡物时开投影出现暗区，且**只有变暗**（最大变亮 <= 1 灰阶）；
  3. 暗区重心落在遮挡物的**远离灯那一侧**（灯在原点上方、遮挡物在 +x，影子在 +x 更远处）；
  4. **±X 两个面**：遮挡物从 +x 挪到 -x，暗区重心随之镜像，且两个重心关于画面中线
     **互为相反数**（场景与相机都关于 x=0 对称）—— 面内 uv 只要镜像错就会破坏它；
  5. **±Z 两个面**：遮挡物改放到 ±z，暗区必须出现在**另一个位置**（y 方向差异 > 10 px），
     且 ±z 也互为镜像 —— 这一条覆盖了聚光灯那张图永远测不到的另外两个面；
  6. **下标规则**：前面多一盏**不投影**的点光源，暗区位置/面积不变
     （`point_shadow_params.x` 是点在 `point_lights` 里的下标，不是"第几盏投影灯"）；
  7. **两套资源互不干扰**：同时点亮一盏投影点光源与一盏投影聚光灯时，
     点光的暗区与聚光的暗区**各自都还在**（"only 点光"与"both"的差异、
     "only 聚光"与"both"的差异都必须 > 0），而且点光那张图开/关不影响聚光的暗区位置。

用法：
    python3 tools/probe_point_shadow.py --cli target/debug/quench3d
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
        """返回 (变化像素数, 最大变暗量, 最大变亮量)。d > 0 表示 self 比 other 更暗。"""
        self._check(other)
        changed = 0
        max_dark = 0.0
        max_light = 0.0
        for i in range(self.w * self.h):
            d = luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp)
            if abs(d) > 1e-9:
                changed += 1
            max_dark = max(max_dark, d)
            max_light = max(max_light, -d)
        return changed, max_dark, max_light

    def dark_mask(self, other, thr=3.0):
        self._check(other)
        return [
            i
            for i in range(self.w * self.h)
            if luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp) > thr
        ]

    def changed_mask(self, other, thr=2.0):
        self._check(other)
        return [
            i
            for i in range(self.w * self.h)
            if abs(luma(other.px, i, self.bpp) - luma(self.px, i, self.bpp)) > thr
        ]

    def centroid(self, mask):
        if not mask:
            return None
        sx = sum(i % self.w for i in mask) / len(mask)
        sy = sum(i // self.w for i in mask) / len(mask)
        return sx, sy

    def save_mask_pgm(self, mask, path):
        s = set(mask)
        rows = [
            bytes(255 if (y * self.w + x) in s else 0 for x in range(self.w))
            for y in range(self.h)
        ]
        pathlib.Path(path).write_bytes(b"P5\n%d %d\n255\n" % (self.w, self.h) + b"".join(rows))


def side_by_side(paths, labels, out_path, gap=4):
    ims = [Image(p) for p in paths]
    w, h = ims[0].w, ims[0].h
    total_w = w * len(ims) + gap * (len(ims) - 1)
    buf = bytearray(b"\x20" * (total_w * h * 3))
    for k, im in enumerate(ims):
        x0 = k * (w + gap)
        for y in range(h):
            for x in range(w):
                i = (y * w + x) * im.bpp
                o = (y * total_w + x0 + x) * 3
                buf[o : o + 3] = im.px[i : i + 3]
    write_png(out_path, total_w, h, bytes(buf))
    print("对照图：" + str(out_path) + "  （左→右：" + " | ".join(labels) + "）")


# ---------------------------------------------------------------- 场景

# 光在原点上方 8 米处；遮挡物中心在 y = 4（离地 3~5 米），水平偏 2.5 米。
# 解析解：地面上的影子从 x ≈ 2.5 * 8/(8-3) = 4.0 一直到 2.5 * 8/(8-5) = 6.7 米，
# 稳稳落在画面里（相机在 (0,14,16) 看到的世界 x 范围约 ±10 米）。
LIGHT_Y = 8.0
OCC_Y = 4.0
OCC_OFFSET = 2.5


def scene(out_dir, name, *, occ=None, cast=False, extra_point=False, spot=False,
          point_cast=False, png=None):
    """同一套场景，只改一个变量。

    `occ` 是遮挡物的世界位置（None = 不放）；`cast` 是那盏点光源的 `cast_shadows`；
    `extra_point` 在它**前面**再插一盏不投影的点光源（测下标规则）；
    `spot` 再加一盏聚光灯（`point_cast` 决定它投不投影），用来验证两套阴影图互不干扰。
    """
    lights = {}
    if extra_point:
        # 一盏不投影的点光源，排在投影灯前面：它的存在只该影响"下标"，不该影响暗区
        lights["point_lights"] = [
            {"position": [10.0, 8.0, 8.0], "color": [1.0, 1.0, 1.0],
             "intensity": 60.0, "radius": 30.0}
        ]
    point_main = {
        "position": [0.0, LIGHT_Y, 0.0],
        "color": [1.0, 1.0, 1.0],
        "intensity": 3000.0,
        "radius": 40.0,
        "cast_shadows": cast,
    }
    lights["point_lights"] = lights.get("point_lights", []) + [point_main]
    if spot:
        # 聚光灯照向 -z 侧（与点光的暗区方向不同），便于把两个暗区分开统计
        lights["spot_lights"] = [
            {
                "position": [0.0, 9.0, 0.0],
                "direction": [0.0, -9.0, -4.0],
                "color": [1.0, 1.0, 1.0],
                "intensity": 12000.0,
                "radius": 40.0,
                "inner_deg": 20.0,
                "outer_deg": 30.0,
                "cast_shadows": point_cast,
            }
        ]
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
            **lights,
        },
        "render": {"width": 320, "height": 320, "msaa": 1, "output": str(png)},
    }
    if occ is not None:
        cfg["objects"] = [
            {
                "name": "遮挡物",
                "shape": {"type": "box", "size": [2, 2, 2]},
                "position": list(occ),
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
    ap.add_argument("--out", default="target/tmp/probe55", help="输出目录")
    ap.add_argument("--keep", action="store_true", help="保留中间 PNG/PGM + 拼对照图")
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

    ox = OCC_OFFSET
    oy = OCC_Y
    cases = {
        # 名字: (遮挡物位置, 点光投影, 前面多一盏点光, 加聚光灯, 聚光投影)
        "noocc_off": (None, False, False, False, False),
        "noocc_on": (None, True, False, False, False),
        "occx_off": ((ox, oy, 0.0), False, False, False, False),
        "occx_on": ((ox, oy, 0.0), True, False, False, False),
        "occx2_off": ((ox * 0.5, oy, 0.0), False, False, False, False),
        "occx2_on": ((ox * 0.5, oy, 0.0), True, False, False, False),
        "occxn_off": ((-ox, oy, 0.0), False, False, False, False),
        "occxn_on": ((-ox, oy, 0.0), True, False, False, False),
        "occz_off": ((0.0, oy, ox), False, False, False, False),
        "occz_on": ((0.0, oy, ox), True, False, False, False),
        "occzn_off": ((0.0, oy, -ox), False, False, False, False),
        "occzn_on": ((0.0, oy, -ox), True, False, False, False),
        "two_off": ((ox, oy, 0.0), False, True, False, False),
        "two_on": ((ox, oy, 0.0), True, True, False, False),
        # 两套资源：聚光灯单独投（点光不投）/ 点光单独投 / 两盏都投
        "sp_off": ((ox, oy, 0.0), False, False, True, False),
        "sp_on": ((ox, oy, 0.0), False, False, True, True),
        "pt_off": ((ox, oy, 0.0), False, False, True, False),
        "pt_on": ((ox, oy, 0.0), True, False, True, False),
        "both_off": ((ox, oy, 0.0), False, False, True, False),
        "both_on": ((ox, oy, 0.0), True, False, True, True),
    }
    images = {}
    for name, (occ, cast, extra, spot, point_cast) in cases.items():
        png = out / f"{name}.png"
        if png.exists():
            png.unlink()
        doc = scene(
            out,
            name,
            occ=occ,
            cast=cast,
            extra_point=extra,
            spot=spot,
            point_cast=point_cast,
            png=png,
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
    checks = []

    def check(name, ok, detail):
        checks.append(name)
        print(f"{'PASS' if ok else 'FAIL'}  {name}: {detail}")
        if not ok:
            failures.append(name)

    total = images["noocc_off"].w * images["noocc_off"].h

    # ---- 1. 零差异基线：没有遮挡物时，投影开关必须逐像素相同 ----
    changed, dark, light = images["noocc_on"].diff(images["noocc_off"])
    check(
        "无遮挡物时开投影不改变任何像素（零差异基线）",
        changed == 0,
        f"不同像素 {changed}/{total}（最大变暗 {dark:.1f}、最大变亮 {light:.1f}）",
    )

    # ---- 2. 有遮挡物：必须出现暗区，且只有变暗 ----
    changed, dark, light = images["occx_on"].diff(images["occx_off"])
    check(
        "有遮挡物时开投影出现暗区",
        changed > 200,
        f"不同像素 {changed}（{changed / total * 100:.2f}%），最大变暗 {dark:.1f}",
    )
    check("差异只有变暗（没有变亮）", light <= 1.0, f"最大变亮 {light:.1f}")
    check("暗区足够明显", dark >= 20.0, f"最大变暗 {dark:.1f} 灰阶")

    # ---- 3. 方向：灯在原点上方，遮挡物在 +x → 影子必须落在 +x 更远处 ----
    dark_x = images["occx_on"].dark_mask(images["occx_off"])
    c_x = images["occx_on"].centroid(dark_x)
    # 遮挡物在屏幕上的位置：拿"有遮挡物 / 没有遮挡物"这两张比（投影开关不改变物体位置，
    # 只改变接收面的明暗——拿 occx_on vs occx_off 比得到的就是影子本身）
    occ_screen = images["occx_off"].changed_mask(images["noocc_off"])
    c_occ = images["occx_off"].centroid(occ_screen)
    check(
        "暗区重心在遮挡物**远离灯**的一侧（+x）",
        c_x is not None and c_occ is not None and c_x[0] > c_occ[0] + 5.0,
        f"暗区重心 x = {c_x[0]:.1f}，遮挡物重心 x = {c_occ[0]:.1f}",
    )
    check(
        "暗区重心在遮挡物下方（影子落在地上）",
        c_x[1] > c_occ[1],
        f"暗区重心 y = {c_x[1]:.1f}，遮挡物重心 y = {c_occ[1]:.1f}",
    )

    # ---- 4. 影子跟着遮挡物走（x 方向）----
    dark_x2 = images["occx2_on"].dark_mask(images["occx2_off"])
    c_x2 = images["occx2_on"].centroid(dark_x2)
    check(
        "遮挡物向光轴*靠近*时暗区重心也向画面中间靠",
        c_x2 is not None and c_x2[0] < c_x[0] - 5.0,
        f"重心 x：{c_x[0]:.1f}（遮挡物 +{ox}）→ {c_x2[0]:.1f}（遮挡物 +{ox * 0.5}）",
    )

    # ---- 5. ±X 面镜像：遮挡物挪到 -x，暗区必须镜像，且重心关于中线互为相反数 ----
    dark_xn = images["occxn_on"].dark_mask(images["occxn_off"])
    c_xn = images["occxn_on"].centroid(dark_xn)
    mid = images["noocc_on"].w / 2.0
    off_pos = c_x[0] - mid
    off_neg = c_xn[0] - mid
    check(
        "遮挡物挪到 -x 后暗区翻到中线另一侧",
        off_neg < -5.0 < 5.0 < off_pos,
        f"偏移 {off_pos:+.1f} px（+x 遮挡物）vs {off_neg:+.1f} px（-x 遮挡物）",
    )
    check(
        "±X 两个面的暗区偏移互为相反数（相机与场景都关于 x=0 对称）",
        abs(off_pos + off_neg) < 2.0,
        f"和 = {off_pos + off_neg:+.2f} px（面内 u 只要镜像错就会破坏它）",
    )
    check(
        "±X 两个面的暗区面积一致",
        abs(len(dark_xn) - len(dark_x)) <= max(8, len(dark_x) // 40),
        f"{len(dark_x)} px vs {len(dark_xn)} px",
    )

    # ---- 6. ±Z 面：换成 z 方向遮挡物，暗区必须换到另一个位置（且同样互为镜像）----
    dark_z = images["occz_on"].dark_mask(images["occz_off"])
    c_z = images["occz_on"].centroid(dark_z)
    check(
        "±Z 面的暗区出现在与 ±X 面不同的位置（覆盖了另外两个面）",
        c_z is not None and abs(c_z[1] - c_x[1]) > 10.0,
        f"z 遮挡物的暗区重心 (x={c_z[0]:.1f}, y={c_z[1]:.1f}) vs "
        f"x 遮挡物 (x={c_x[0]:.1f}, y={c_x[1]:.1f})",
    )
    dark_zn = images["occzn_on"].dark_mask(images["occzn_off"])
    c_zn = images["occzn_on"].centroid(dark_zn)
    row_mid = images["noocc_on"].h / 2.0
    check(
        "±Z 两个面的暗区落在画面中心的**上下两侧**（世界 z 轴翻转 = 画面上下翻转）",
        c_zn is not None and c_z[1] > row_mid > c_zn[1],
        f"+z 重心 y = {c_z[1]:.1f}（画面中心 {row_mid:.0f} 之下），"
        f"-z 重心 y = {c_zn[1]:.1f}（之上）",
    )
    check(
        "±Z 两个面的暗区 x 重心一致（z 方向的影子不该左右偏）",
        abs(c_zn[0] - c_z[0]) < 3.0,
        f"x 重心 {c_z[0]:.1f} vs {c_zn[0]:.1f}",
    )
    check(
        "靠相机那一侧的暗区离画面中心更远（透视：近的影子更大更靠下）",
        (c_z[1] - row_mid) > (row_mid - c_zn[1]),
        f"+z 偏移 {c_z[1] - row_mid:.1f} px vs -z 偏移 {row_mid - c_zn[1]:.1f} px",
    )

    # ---- 7. 下标规则：前面多一盏不投影的点光源，暗区位置/面积不变 ----
    dark_two = images["two_on"].dark_mask(images["two_off"])
    c_two = images["two_on"].centroid(dark_two)
    check(
        "前面多一盏不投影的点光源后，暗区仍在同一位置（下标没错位）",
        c_two is not None and abs(c_two[0] - c_x[0]) < 3.0 and abs(c_two[1] - c_x[1]) < 3.0,
        f"重心 ({c_two[0]:.1f}, {c_two[1]:.1f}) vs ({c_x[0]:.1f}, {c_x[1]:.1f})，"
        f"面积 {len(dark_two)} px vs {len(dark_x)} px",
    )
    check(
        "那盏灯没有引入额外暗区（它本来就不投影）",
        abs(len(dark_two) - len(dark_x)) < max(40, len(dark_x) // 20),
        f"{len(dark_x)} px vs {len(dark_two)} px",
    )

    # ---- 8. 两套资源互不干扰：点光与聚光各自的暗区都还在 ----
    # sp_* 只有聚光投影；pt_* 只有点光投影；both_* 两盏都投影（共用 sp_off 作对照）。
    d_pt = images["pt_on"].diff(images["pt_off"])
    d_sp = images["sp_on"].diff(images["sp_off"])
    check(
        "只有点光投影时也出现暗区（对照图里聚光没开投影）",
        d_pt[0] > 200 and d_pt[2] <= 1.0,
        f"不同像素 {d_pt[0]}，最大变暗 {d_pt[1]:.1f}，最大变亮 {d_pt[2]:.1f}",
    )
    check(
        "只有聚光投影时也出现暗区",
        d_sp[0] > 200 and d_sp[2] <= 1.0,
        f"不同像素 {d_sp[0]}，最大变暗 {d_sp[1]:.1f}，最大变亮 {d_sp[2]:.1f}",
    )
    # both 与"只有点光"的差异 = 聚光那一份；both 与"只有聚光"的差异 = 点光那一份
    d_both_from_pt = images["both_on"].diff(images["pt_on"])
    d_both_from_sp = images["both_on"].diff(images["sp_on"])
    check(
        "两盏同时投影时，聚光那一份影子还在（both 比 only-点光更暗）",
        d_both_from_pt[1] >= 20.0,
        f"both vs only-点光：不同像素 {d_both_from_pt[0]}，最大变暗 {d_both_from_pt[1]:.1f}",
    )
    check(
        "两盏同时投影时，点光那一份影子还在（both 比 only-聚光更暗）",
        d_both_from_sp[1] >= 20.0,
        f"both vs only-聚光：不同像素 {d_both_from_sp[0]}，最大变暗 {d_both_from_sp[1]:.1f}",
    )
    # 真正的"两张影子都在"：在 both_on 的暗区里，两个重心附近都必须有像素。
    dark_sp = images["sp_on"].dark_mask(images["sp_off"])
    c_sp = images["sp_on"].centroid(dark_sp)
    dark_both = images["both_on"].dark_mask(images["both_off"])

    def near(mask, c, r=20.0):
        im = images["both_on"]
        return sum(
            1
            for i in mask
            if ((i % im.w) - c[0]) ** 2 + ((i // im.w) - c[1]) ** 2 <= r * r
        )

    check(
        "两张影子同时存在：点光暗区重心附近有像素",
        c_sp is not None and near(dark_both, c_x) >= 50,
        f"点光重心 ({c_x[0]:.1f}, {c_x[1]:.1f}) 半径 20px 内 {near(dark_both, c_x)} px"
        f"（both 暗区共 {len(dark_both)} px）",
    )
    check(
        "两张影子同时存在：聚光暗区重心附近有像素",
        c_sp is not None and near(dark_both, c_sp) >= 50,
        f"聚光重心 ({c_sp[0]:.1f}, {c_sp[1]:.1f}) 半径 20px 内 {near(dark_both, c_sp)} px",
    )
    # 三张"都不投影"的图必须是同一张：这条把"场景里多一盏不投影的灯/聚光灯"
    # 与"渲染抖动"分开（否则上面那些差异里有多少是噪声就说不清了）。
    same = images["pt_off"].diff(images["sp_off"])[0] + images["both_off"].diff(images["sp_off"])[0]
    check(
        "三张都不投影的对照图逐像素相同（没有渲染抖动）",
        same == 0,
        f"pt_off/sp_off/both_off 之间的不同像素数 = {same}",
    )

    if args.keep:
        images["occx_on"].save_mask_pgm(dark_x, out / "dark_occx.pgm")
        images["occz_on"].save_mask_pgm(dark_z, out / "dark_occz.pgm")
        side_by_side(
            [out / "occx_off.png", out / "occx_on.png", out / "noocc_on.png"],
            ["投影关", "投影开（+x 遮挡物）", "投影开但无遮挡物"],
            out / "compare.png",
        )
        side_by_side(
            [out / "occz_on.png", out / "occxn_on.png", out / "both_on.png"],
            ["+z 遮挡物", "-x 遮挡物", "点光+聚光同时投影"],
            out / "compare_axes.png",
        )

    print()
    if failures:
        print(f"探针失败 ❌：{', '.join(failures)}")
        return 1
    print(
        f"探针全部通过 ✅（{len(cases)} 张图，{len(checks)} 条判据；图在 %s）" % out
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
