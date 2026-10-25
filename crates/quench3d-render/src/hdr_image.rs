//! 浮点图像：Radiance HDR 与 OpenEXR 的读写。
//!
//! 只做渲染器真正需要的范围：
//! * Radiance `.hdr`：RGBE 解码（复用 [`EnvironmentMap`] 的解析器）与编码；
//! * OpenEXR：**单 part、scanline**、`HALF`/`FLOAT`/`UINT` 通道，压缩为
//!   `NONE` 或 `RLE`；写盘用 `NONE`（无损、最简单，任何 EXR 阅读器都能读）。
//!
//! 不支持：tiled/multi-part/deep、PIZ/ZIP/DWAA 压缩、色度子采样。
//! 这些会在解析时明确报错，不会静默读错。
//!
//! 像素统一为**线性** RGBA（`[f32; 4]`，alpha 缺省 1）。HDR 没有 alpha 通道，
//! 读入时补 1；写 HDR 时丢弃 alpha。

use crate::environment::EnvironmentMap;

/// CPU 侧线性浮点图像（行优先，左上角为原点）。
#[derive(Clone, Debug, PartialEq)]
pub struct FloatImage {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// 线性 RGBA，长度 = width * height。
    pub rgba: Vec<[f32; 4]>,
}

impl FloatImage {
    /// 由 RGBA 浮点数据构造（校验长度）。
    pub fn from_rgba32f(width: u32, height: u32, rgba: Vec<[f32; 4]>) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("浮点图像尺寸不能为 0".to_string());
        }
        if rgba.len() != width as usize * height as usize {
            return Err(format!(
                "浮点图像像素数 {} 与尺寸 {width}×{height} 不匹配",
                rgba.len()
            ));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// 从文件读取（按扩展名选 `.hdr` / `.exr`）。
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .map_err(|e| format!("读取浮点图像 {} 失败: {e}", path.display()))?;
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "hdr" => Self::from_hdr(&bytes),
            "exr" => Self::from_exr(&bytes),
            other => Err(format!(
                "不支持的浮点图像扩展名 .{other}（只支持 .hdr/.exr）"
            )),
        }
    }

    /// 按扩展名写盘。
    pub fn write_file(&self, path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let path = path.as_ref();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let bytes = match ext.as_str() {
            "hdr" => self.to_hdr(),
            "exr" => self.to_exr()?,
            other => {
                return Err(format!(
                    "不支持的浮点图像扩展名 .{other}（只支持 .hdr/.exr）"
                ))
            }
        };
        std::fs::write(path, bytes)
            .map_err(|e| format!("写入浮点图像 {} 失败: {e}", path.display()))
    }

    /// 解码 Radiance HDR（RGBE）。复用环境贴图那条经过测试的解析器。
    pub fn from_hdr(bytes: &[u8]) -> Result<Self, String> {
        let map = EnvironmentMap::from_hdr(bytes)?;
        let rgba = map.pixels.iter().map(|p| [p[0], p[1], p[2], 1.0]).collect();
        Self::from_rgba32f(map.width as u32, map.height as u32, rgba)
    }

    /// 编码 Radiance HDR（平铺 RGBE，不用新式 RLE）。
    ///
    /// 平铺是标准允许的形式，解析器与外部工具都能读；对贴图尺寸的图像体积不是问题。
    pub fn to_hdr(&self) -> Vec<u8> {
        let mut out = format!(
            "#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {} +X {}\n",
            self.height, self.width
        )
        .into_bytes();
        out.reserve(self.rgba.len() * 4);
        for p in &self.rgba {
            out.extend_from_slice(&encode_rgbe([p[0], p[1], p[2]]));
        }
        out
    }

    /// 解码 OpenEXR（单 part、scanline、NONE/RLE 压缩）。
    pub fn from_exr(bytes: &[u8]) -> Result<Self, String> {
        parse_exr(bytes)
    }

    /// 编码 OpenEXR（单 part、scanline、无压缩、HALF RGB）。
    pub fn to_exr(&self) -> Result<Vec<u8>, String> {
        write_exr(self)
    }

    /// 垂直翻转（左上 ↔ 左下）。
    pub fn flip_vertical(&mut self) {
        let h = self.height as usize;
        let w = self.width as usize;
        for y in 0..h / 2 {
            for x in 0..w {
                self.rgba.swap(y * w + x, (h - 1 - y) * w + x);
            }
        }
    }

    /// 用盒式滤波生成完整 mip 链（从原尺寸到 1×1 倒序返回）。
    pub fn generate_mip_chain(&self) -> Vec<(u32, u32, Vec<[f32; 4]>)> {
        let mut levels = Vec::new();
        let mut w = self.width;
        let mut h = self.height;
        let mut data = self.rgba.clone();
        levels.push((w, h, data.clone()));
        while w > 1 || h > 1 {
            let nw = (w / 2).max(1);
            let nh = (h / 2).max(1);
            let mut next = vec![[0.0f32; 4]; (nw * nh) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let mut acc = [0.0f32; 4];
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = (x * 2 + dx).min(w - 1);
                            let sy = (y * 2 + dy).min(h - 1);
                            let p = data[(sy * w + sx) as usize];
                            for c in 0..4 {
                                acc[c] += p[c];
                            }
                        }
                    }
                    for c in 0..4 {
                        next[(y * nw + x) as usize][c] = acc[c] * 0.25;
                    }
                }
            }
            data = next;
            w = nw;
            h = nh;
            levels.push((w, h, data.clone()));
        }
        levels
    }

    /// 生成 `Rgba16Float` 上传用的半精度字节（行优先，mip 0）。
    pub fn to_rgba16f_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len() * 8);
        for p in &self.rgba {
            for c in p {
                out.extend_from_slice(
                    &half::f16::from_f32(c.clamp(-65504.0, 65504.0)).to_le_bytes(),
                );
            }
        }
        out
    }
}

/// RGBE 编码：指数取 `ceil(log2(max))`，尾数保留 8 位。
fn encode_rgbe(rgb: [f32; 3]) -> [u8; 4] {
    let v = rgb[0].max(rgb[1]).max(rgb[2]).max(0.0);
    if !v.is_finite() || v < 1e-32 {
        return [0, 0, 0, 0];
    }
    let e = v.log2().floor() as i32 + 1;
    let scale = 2f32.powi(8 - e);
    let q = |c: f32| (c.max(0.0) * scale).round().clamp(0.0, 255.0) as u8;
    [
        q(rgb[0]),
        q(rgb[1]),
        q(rgb[2]),
        (e + 128).clamp(0, 255) as u8,
    ]
}

fn exr_attr(out: &mut Vec<u8>, name: &str, ty: &str, data: &[u8]) {
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(ty.as_bytes());
    out.push(0);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn write_exr(image: &FloatImage) -> Result<Vec<u8>, String> {
    let w = image.width as usize;
    let h = image.height as usize;
    if w == 0 || h == 0 {
        return Err("EXR 尺寸不能为 0".to_string());
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0x76, 0x2f, 0x31, 0x01]); // magic
    out.extend_from_slice(&2u32.to_le_bytes()); // version 2, scanline

    // chlist：B、G、R，均为 HALF、采样 1。
    let mut chlist = Vec::new();
    for name in ["B", "G", "R"] {
        chlist.extend_from_slice(name.as_bytes());
        chlist.push(0);
        chlist.extend_from_slice(&1i32.to_le_bytes()); // HALF
        chlist.push(0); // pLinear
        chlist.extend_from_slice(&[0; 3]);
        chlist.extend_from_slice(&1i32.to_le_bytes()); // xSampling
        chlist.extend_from_slice(&1i32.to_le_bytes()); // ySampling
    }
    chlist.push(0);
    exr_attr(&mut out, "channels", "chlist", &chlist);
    exr_attr(&mut out, "compression", "compression", &[0]); // NONE
    let mut box2i = Vec::new();
    for v in [0i32, 0, w as i32 - 1, h as i32 - 1] {
        box2i.extend_from_slice(&v.to_le_bytes());
    }
    exr_attr(&mut out, "dataWindow", "box2i", &box2i);
    exr_attr(&mut out, "displayWindow", "box2i", &box2i);
    exr_attr(&mut out, "lineOrder", "lineOrder", &[0]); // increasing
    exr_attr(&mut out, "pixelAspectRatio", "float", &1.0f32.to_le_bytes());
    let mut swc = Vec::new();
    swc.extend_from_slice(&0.0f32.to_le_bytes());
    swc.extend_from_slice(&0.0f32.to_le_bytes());
    exr_attr(&mut out, "screenWindowCenter", "v2f", &swc);
    exr_attr(
        &mut out,
        "screenWindowWidth",
        "float",
        &1.0f32.to_le_bytes(),
    );
    out.push(0); // header 结束

    let row_bytes = w * 3 * 2;
    let data_start = out.len() + h * 8;
    for row in 0..h {
        out.extend_from_slice(&((data_start + row * (8 + row_bytes)) as u64).to_le_bytes());
    }
    for y in 0..h {
        out.extend_from_slice(&(y as i32).to_le_bytes());
        out.extend_from_slice(&(row_bytes as u32).to_le_bytes());
        // 通道顺序 B、G、R。
        for c in [2usize, 1, 0] {
            for x in 0..w {
                let px = image.rgba[y * w + x][c];
                out.extend_from_slice(
                    &half::f16::from_f32(px.clamp(-65504.0, 65504.0)).to_le_bytes(),
                );
            }
        }
    }
    Ok(out)
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| "EXR 偏移溢出".to_string())?;
        let out = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| "EXR 数据截断".to_string())?;
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn cstr(&mut self) -> Result<String, String> {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.bytes.len() {
            return Err("EXR 字符串未以 0 结束".to_string());
        }
        let s = String::from_utf8_lossy(&self.bytes[start..self.pos]).to_string();
        self.pos += 1;
        Ok(s)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PixelType {
    Uint,
    Half,
    Float,
}

impl PixelType {
    fn size(self) -> usize {
        match self {
            Self::Uint | Self::Float => 4,
            Self::Half => 2,
        }
    }
}

struct Channel {
    name: String,
    kind: PixelType,
}

fn parse_exr(bytes: &[u8]) -> Result<FloatImage, String> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != [0x76, 0x2f, 0x31, 0x01] {
        return Err("不是 OpenEXR（magic 不匹配）".to_string());
    }
    let version = r.u32()?;
    let version_number = version & 0xff;
    let flags = version >> 8;
    if version_number != 2 {
        return Err(format!("只支持 EXR version 2，实际 {version_number}"));
    }
    if flags & 0x2 != 0 {
        return Err("不支持 tiled EXR（只支持 scanline）".to_string());
    }
    if flags & 0x10 != 0 {
        return Err("不支持 multi-part EXR".to_string());
    }

    let mut channels: Vec<Channel> = Vec::new();
    let mut compression = 0u8;
    let mut data_window = (0i32, 0i32, -1i32, -1i32);
    let mut line_order = 0u8;
    loop {
        let name = r.cstr()?;
        if name.is_empty() {
            break;
        }
        let ty = r.cstr()?;
        let size = r.u32()? as usize;
        let payload = r.take(size)?;
        match name.as_str() {
            "channels" => {
                let mut cr = Reader::new(payload);
                loop {
                    let cname = cr.cstr()?;
                    if cname.is_empty() {
                        break;
                    }
                    let kind = match cr.i32()? {
                        0 => PixelType::Uint,
                        1 => PixelType::Half,
                        2 => PixelType::Float,
                        other => return Err(format!("EXR 通道 {cname} 像素类型 {other} 不支持")),
                    };
                    cr.u8()?; // pLinear
                    cr.take(3)?;
                    let xs = cr.i32()?;
                    let ys = cr.i32()?;
                    if xs != 1 || ys != 1 {
                        return Err(format!(
                            "不支持子采样通道 {cname}（xSampling={xs}, ySampling={ys}）"
                        ));
                    }
                    channels.push(Channel { name: cname, kind });
                }
            }
            "compression" => {
                compression = *payload.first().ok_or("EXR compression 为空")?;
            }
            "dataWindow" => {
                if payload.len() != 16 {
                    return Err("EXR dataWindow 长度不是 16".to_string());
                }
                let mut br = Reader::new(payload);
                data_window = (br.i32()?, br.i32()?, br.i32()?, br.i32()?);
            }
            "lineOrder" => {
                line_order = *payload.first().ok_or("EXR lineOrder 为空")?;
            }
            _ => {
                let _ = ty;
            }
        }
    }
    if channels.is_empty() {
        return Err("EXR 没有通道".to_string());
    }
    let (x_min, y_min, x_max, y_max) = data_window;
    if x_max < x_min || y_max < y_min {
        return Err(format!("EXR dataWindow 无效：{data_window:?}"));
    }
    let w = (x_max - x_min + 1) as usize;
    let h = (y_max - y_min + 1) as usize;
    if w == 0 || h == 0 || w > 65536 || h > 65536 {
        return Err(format!("EXR 尺寸无效：{w}×{h}"));
    }
    if !matches!(compression, 0 | 1) {
        return Err(format!(
            "不支持 EXR 压缩 {compression}（只支持 NONE=0 / RLE=1）"
        ));
    }

    let table_start = r.pos;
    let mut offsets = Vec::with_capacity(h);
    for _ in 0..h {
        offsets.push(r.u64()?);
    }
    let expected: usize = channels.iter().map(|c| w * c.kind.size()).sum();
    let mut rgba = vec![[0.0f32, 0.0, 0.0, 1.0]; w * h];
    let mut seen = vec![false; h];
    for off in offsets {
        let mut br = Reader::new(bytes);
        if off as usize >= bytes.len() {
            return Err(format!("EXR scanline 偏移 {off} 越界"));
        }
        br.pos = off as usize;
        let y = br.i32()? - y_min;
        let data_size = br.u32()? as usize;
        let raw = br.take(data_size)?;
        if y < 0 || y as usize >= h {
            return Err(format!("EXR scanline y={y} 越界"));
        }
        let row = match compression {
            0 => {
                if raw.len() < expected {
                    return Err(format!(
                        "EXR 未压缩 scanline 太短：{} < {expected}",
                        raw.len()
                    ));
                }
                raw[..expected].to_vec()
            }
            1 => exr_rle_decode(raw, expected)?,
            _ => unreachable!(),
        };
        let mut cursor = 0usize;
        for ch in &channels {
            for x in 0..w {
                let v = match ch.kind {
                    PixelType::Half => {
                        let bits = u16::from_le_bytes(row[cursor..cursor + 2].try_into().unwrap());
                        half::f16::from_bits(bits).to_f32()
                    }
                    PixelType::Float => {
                        f32::from_le_bytes(row[cursor..cursor + 4].try_into().unwrap())
                    }
                    PixelType::Uint => {
                        u32::from_le_bytes(row[cursor..cursor + 4].try_into().unwrap()) as f32
                    }
                };
                cursor += ch.kind.size();
                match ch.name.as_str() {
                    "R" => rgba[y as usize * w + x][0] = v,
                    "G" => rgba[y as usize * w + x][1] = v,
                    "B" => rgba[y as usize * w + x][2] = v,
                    "A" => rgba[y as usize * w + x][3] = v,
                    _ => {}
                }
            }
        }
        seen[y as usize] = true;
    }
    if seen.iter().any(|s| !s) {
        return Err("EXR 有 scanline 缺失（offset table 不完整）".to_string());
    }
    let _ = line_order; // 表项顺序即行号；lineOrder 只影响文件里的块顺序
    let _ = table_start;
    FloatImage::from_rgba32f(w as u32, h as u32, rgba)
}

/// EXR 的 RLE：RLE 解压 → 预测器逆变换 → 奇偶字节反交错。
fn exr_rle_decode(src: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut packed = Vec::with_capacity(expected);
    let mut i = 0usize;
    while i < src.len() {
        let count = src[i] as i8;
        i += 1;
        if count < 0 {
            let n = (-count) as usize;
            if i + n > src.len() {
                return Err("EXR RLE literal 截断".to_string());
            }
            packed.extend_from_slice(&src[i..i + n]);
            i += n;
        } else {
            let n = count as usize + 1;
            let value = *src.get(i).ok_or("EXR RLE run 截断")?;
            i += 1;
            packed.extend(std::iter::repeat_n(value, n));
        }
        if packed.len() > expected + 1 {
            return Err("EXR RLE 解压超过一行".to_string());
        }
    }
    if packed.len() != expected {
        return Err(format!(
            "EXR RLE 解压长度 {} 与期望 {expected} 不符",
            packed.len()
        ));
    }
    // 预测器逆变换。OpenEXR 的预测器从第 2 个字节开始，首字节原样保留
    // （不是从 p=0 开始），镜像 `ImfRleCompressor.cpp`。
    let mut pred = vec![0u8; expected];
    pred[0] = packed[0];
    let mut p = pred[0];
    for i in 1..expected {
        let v = (packed[i] as i32 + p as i32 - 128).rem_euclid(256) as u8;
        pred[i] = v;
        p = v;
    }
    // 奇偶字节反交错
    let half = expected.div_ceil(2);
    let mut out = vec![0u8; expected];
    for i in 0..expected {
        out[i] = if i % 2 == 0 {
            pred[i / 2]
        } else {
            pred[half + i / 2]
        };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FloatImage {
        FloatImage::from_rgba32f(
            3,
            2,
            vec![
                [0.0, 0.25, 2.0, 1.0],
                [1.0, 0.5, 0.125, 1.0],
                [10.0, 0.0, 0.75, 1.0],
                [0.01, 4.0, 0.0, 1.0],
                [2.5, 2.5, 2.5, 1.0],
                [100.0, 0.1, 0.3, 1.0],
            ],
        )
        .unwrap()
    }

    #[test]
    fn hdr_round_trip_keeps_linear_radiance() {
        let img = sample();
        let bytes = img.to_hdr();
        let decoded = FloatImage::from_hdr(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (3, 2));
        for (a, b) in img.rgba.iter().zip(&decoded.rgba) {
            // RGBE 共用 8 位指数：小通道会被量化到 0 或一个指数台阶，误差上限
            // 由**最亮通道**决定，而不是各自相对误差。
            let peak = a[0].max(a[1]).max(a[2]).max(1.0);
            for c in 0..3 {
                let err = (a[c] - b[c]).abs();
                assert!(
                    err <= 0.02 * peak + 0.01,
                    "HDR 往返误差过大：{a:?} vs {b:?}"
                );
            }
            assert_eq!(b[3], 1.0);
        }
    }

    #[test]
    fn exr_round_trip_keeps_half_precision_rgb() {
        let img = sample();
        let bytes = img.to_exr().unwrap();
        assert_eq!(&bytes[..4], &[0x76, 0x2f, 0x31, 0x01]);
        let decoded = FloatImage::from_exr(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (3, 2));
        for (a, b) in img.rgba.iter().zip(&decoded.rgba) {
            for c in 0..3 {
                let want = half::f16::from_f32(a[c]).to_f32();
                assert!(
                    (want - b[c]).abs() <= 1e-6,
                    "EXR 往返必须等于 HALF 量化值：{a:?} -> {b:?}"
                );
            }
            assert_eq!(b[3], 1.0);
        }
    }

    #[test]
    fn float_mip_chain_averages_by_box_filter() {
        let img = FloatImage::from_rgba32f(
            2,
            2,
            vec![
                [0.0, 0.0, 0.0, 1.0],
                [2.0, 2.0, 2.0, 1.0],
                [4.0, 4.0, 4.0, 1.0],
                [6.0, 6.0, 6.0, 1.0],
            ],
        )
        .unwrap();
        let chain = img.generate_mip_chain();
        assert_eq!(chain.len(), 2);
        assert_eq!((chain[1].0, chain[1].1), (1, 1));
        assert_eq!(chain[1].2[0], [3.0, 3.0, 3.0, 1.0]);
    }

    #[test]
    fn exr_rejects_unsupported_compression_and_truncation() {
        let mut bytes = sample().to_exr().unwrap();
        // 找到 compression 属性的 payload（属性名后是类型名和长度）。
        let needle = b"compression\0compression\0";
        let at = bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("compression 属性");
        let payload = at + needle.len() + 4;
        bytes[payload] = 3; // ZIP
        assert!(FloatImage::from_exr(&bytes).is_err());

        let mut truncated = sample().to_exr().unwrap();
        truncated.truncate(truncated.len() / 2);
        assert!(FloatImage::from_exr(&truncated).is_err());
    }

    #[test]
    fn exr_rle_decoder_inverts_predictor_and_interleave() {
        // 手写一行 8 字节：原文 0,1,2,3,4,5,6,7。
        // 编码 = 预测器（d = cur - prev + 128）→ 奇偶交错 → RLE literal。
        let original = [0u8, 1, 2, 3, 4, 5, 6, 7];
        // 编码顺序：奇偶字节重排 → 预测器 → RLE。
        let half = 4;
        let mut inter = [0u8; 8];
        for i in 0..8 {
            if i % 2 == 0 {
                inter[i / 2] = original[i];
            } else {
                inter[half + i / 2] = original[i];
            }
        }
        let mut pred = [0u8; 8];
        pred[0] = inter[0];
        let mut p = inter[0];
        for i in 1..8 {
            pred[i] = (inter[i] as i32 - p as i32 + 128) as u8;
            p = inter[i];
        }
        let mut rle = Vec::new();
        rle.push((-8i8) as u8);
        rle.extend_from_slice(&pred);
        assert_eq!(exr_rle_decode(&rle, 8).unwrap(), original);
    }
}
