//! 环境贴图（IBL）：Radiance HDR / PNG / JPEG 全景图 → 预滤波 cubemap。
//!
//! 这是**离线**（装载时）的 CPU 预处理：先把等距柱状全景解码成线性 RGB，
//! 再生成 6 面立方体贴图；每个 mip 用固定数量的方向采样做粗糙度预滤波。
//! 运行时着色器只做一次 `textureSampleLevel(env_cube, .., roughness * max_mip)`。
//!
//! 支持的范围是刻意的：
//! * Radiance `.hdr`（`32-bit_rle_rgbe`，新/旧 RLE 与平铺扫描线）；
//! * `.png` / `.jpg` / `.jpeg` 等距柱状 LDR 图（sRGB → 线性）；
//! * 只支持标准 `-Y height +X width` 朝向；其它朝向返回错误，不猜。
//!
//! 不是 PMREM/GGX 精确预滤波：用锥形方向平均近似，质量足够预览，成本可预测。

use std::f32::consts::{PI, TAU};
use std::path::Path;

use quench3d_math::Vec3;

/// 一张等距柱状全景图（线性 RGB，行优先，第一行是天空顶部）。
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentMap {
    /// 宽（像素）。
    pub width: usize,
    /// 高（像素）。
    pub height: usize,
    /// 线性 RGB，长度 = width * height。
    pub pixels: Vec<[f32; 3]>,
    /// 环境光强度倍率。
    pub intensity: f32,
    /// 绕 Y 轴旋转（弧度）。
    pub rotation: f32,
}

/// cubemap 的一个 mip：6 个面，每面 `size * size` 个线性 RGB。
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentMip {
    /// 这一级边长（像素）。
    pub size: u32,
    /// 面顺序：+X, -X, +Y, -Y, +Z, -Z。
    pub faces: [Vec<[f32; 3]>; 6],
}

/// 预滤波好的 cubemap；`mips[0]` 最大、`mips.last()` 是 1×1。
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentCube {
    /// mip 链，按尺寸从大到小。
    pub mips: Vec<EnvironmentMip>,
}

impl EnvironmentMap {
    /// 从文件读取；`intensity` / `rotation_deg` 来自场景。
    pub fn load(path: &Path, intensity: f32, rotation_deg: f32) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("读取环境贴图 {} 失败: {e}", path.display()))?;
        let mut map = Self::from_bytes(path, &bytes)?;
        map.intensity = intensity;
        map.rotation = rotation_deg.to_radians();
        Ok(map)
    }

    /// 从内存解码（按扩展名选 HDR 或 LDR）。
    pub fn from_bytes(path: &Path, bytes: &[u8]) -> Result<Self, String> {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let map = if ext == "hdr" {
            Self::from_hdr(bytes)?
        } else {
            Self::from_ldr(bytes)?
        };
        if map.width == 0 || map.height == 0 {
            return Err("环境贴图尺寸不能为 0".to_string());
        }
        Ok(map)
    }

    /// 解码 Radiance RGBE。
    pub fn from_hdr(bytes: &[u8]) -> Result<Self, String> {
        let (width, height, pixels) = parse_radiance(bytes)?;
        Ok(Self {
            width,
            height,
            pixels,
            intensity: 1.0,
            rotation: 0.0,
        })
    }

    /// 解码 PNG/JPEG 等 LDR 图；sRGB 通道转线性。
    pub fn from_ldr(bytes: &[u8]) -> Result<Self, String> {
        let image = image::load_from_memory(bytes).map_err(|e| format!("解码环境贴图失败: {e}"))?;
        let rgba = image.to_rgba8();
        let (width, height) = (rgba.width() as usize, rgba.height() as usize);
        let mut pixels = Vec::with_capacity(width * height);
        for p in rgba.pixels() {
            pixels.push([
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
            ]);
        }
        Ok(Self {
            width,
            height,
            pixels,
            intensity: 1.0,
            rotation: 0.0,
        })
    }

    /// 采样一个方向（线性插值，u 环绕、v 夹紧）。
    pub fn sample(&self, dir: Vec3) -> [f32; 3] {
        let (u, v) = equirect_uv(dir);
        let x = (u * self.width as f32 - 0.5).rem_euclid(self.width as f32);
        let y = (v * self.height as f32 - 0.5).clamp(0.0, self.height as f32 - 1.0);
        let x0 = x.floor() as usize % self.width;
        let x1 = (x0 + 1) % self.width;
        let y0 = y.floor() as usize;
        let y1 = (y0 + 1).min(self.height - 1);
        let fx = x - x.floor();
        let fy = y - y.floor();
        let p = |xi: usize, yi: usize| self.pixels[yi * self.width + xi];
        let a = p(x0, y0);
        let b = p(x1, y0);
        let c = p(x0, y1);
        let d = p(x1, y1);
        let mut out = [0.0f32; 3];
        for k in 0..3 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            out[k] = top + (bottom - top) * fy;
        }
        out
    }

    /// 生成预滤波 cubemap；`face_size` 建议 32~128。
    pub fn to_cube(&self, face_size: u32) -> EnvironmentCube {
        let mut sizes = Vec::new();
        let mut s = face_size.max(2).next_power_of_two();
        while s >= 1 {
            sizes.push(s);
            if s == 1 {
                break;
            }
            s /= 2;
        }
        let mut mips = Vec::with_capacity(sizes.len());
        for (level, size) in sizes.into_iter().enumerate() {
            let roughness = if level == 0 {
                0.0
            } else {
                level as f32 / (face_size.max(2).trailing_zeros().max(1) as f32)
            }
            .clamp(0.0, 1.0);
            let samples = if level == 0 { 1 } else { 12 + level * 12 };
            let mut faces: [Vec<[f32; 3]>; 6] =
                std::array::from_fn(|_| Vec::with_capacity((size * size) as usize));
            for (face, out) in faces.iter_mut().enumerate() {
                for y in 0..size {
                    for x in 0..size {
                        let u = 2.0 * (x as f32 + 0.5) / size as f32 - 1.0;
                        let v = 2.0 * (y as f32 + 0.5) / size as f32 - 1.0;
                        let dir = face_direction(face, u, v);
                        out.push(if samples <= 1 {
                            self.sample(dir)
                        } else {
                            self.sample_cone(dir, roughness, samples)
                        });
                    }
                }
            }
            mips.push(EnvironmentMip { size, faces });
        }
        EnvironmentCube { mips }
    }

    /// 以 `dir` 为中心、按粗糙度张开的锥内平均；确定性 Fibonacci 方向。
    fn sample_cone(&self, dir: Vec3, roughness: f32, samples: usize) -> [f32; 3] {
        let n = dir.normalize_or_zero();
        if n == Vec3::ZERO {
            return [0.0; 3];
        }
        let tangent = if n.y.abs() < 0.99 {
            n.cross(Vec3::Y).normalize_or_zero()
        } else {
            n.cross(Vec3::X).normalize_or_zero()
        };
        let bitangent = n.cross(tangent);
        let cos_max = (1.0 - roughness * roughness * 0.98).clamp(-0.999, 0.999);
        let mut sum = [0.0f32; 3];
        let mut weight_sum = 0.0f32;
        let golden = PI * (3.0 - 5.0f32.sqrt());
        for i in 0..samples {
            let t = (i as f32 + 0.5) / samples as f32;
            let cos_theta = 1.0 - t * (1.0 - cos_max);
            let sin_theta = (1.0 - cos_theta * cos_theta).sqrt().max(0.0);
            let phi = i as f32 * golden;
            let local = tangent * (phi.cos() * sin_theta)
                + bitangent * (phi.sin() * sin_theta)
                + n * cos_theta;
            let s = self.sample(local.normalize_or_zero());
            let weight = cos_theta.max(0.0);
            for k in 0..3 {
                sum[k] += s[k] * weight;
            }
            weight_sum += weight;
        }
        if weight_sum <= 1e-6 {
            return self.sample(n);
        }
        [
            sum[0] / weight_sum,
            sum[1] / weight_sum,
            sum[2] / weight_sum,
        ]
    }

    /// 一像素的平均亮度（测试/诊断用）。
    pub fn average_luminance(&self) -> f32 {
        if self.pixels.is_empty() {
            return 0.0;
        }
        let sum: f32 = self
            .pixels
            .iter()
            .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
            .sum();
        sum / self.pixels.len() as f32
    }
}

/// 等距柱状 UV：`u` 环绕（0..1），`v` 从顶部 0 到底部 1。
pub fn equirect_uv(dir: Vec3) -> (f32, f32) {
    let d = dir.normalize_or_zero();
    let u = d.z.atan2(d.x) / TAU + 0.5;
    let v = d.y.clamp(-1.0, 1.0).acos() / PI;
    (u.rem_euclid(1.0), v.clamp(0.0, 1.0))
}

/// cubemap 面坐标（-1..1）→ 方向。
fn face_direction(face: usize, u: f32, v: f32) -> Vec3 {
    match face {
        0 => Vec3::new(1.0, -v, -u),  // +X
        1 => Vec3::new(-1.0, -v, u),  // -X
        2 => Vec3::new(u, 1.0, v),    // +Y
        3 => Vec3::new(u, -1.0, -v),  // -Y
        4 => Vec3::new(u, -v, 1.0),   // +Z
        _ => Vec3::new(-u, -v, -1.0), // -Z
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 解析 Radiance RGBE；返回 (width, height, 线性 RGB)。
fn parse_radiance(bytes: &[u8]) -> Result<(usize, usize, Vec<[f32; 3]>), String> {
    let mut pos = 0usize;
    let read_line = |pos: &mut usize| -> Option<String> {
        if *pos >= bytes.len() {
            return None;
        }
        let start = *pos;
        while *pos < bytes.len() && bytes[*pos] != b'\n' {
            *pos += 1;
        }
        let line = String::from_utf8_lossy(&bytes[start..*pos])
            .trim_end_matches('\r')
            .to_string();
        if *pos < bytes.len() {
            *pos += 1;
        }
        Some(line)
    };
    let magic = read_line(&mut pos).ok_or("HDR 文件为空")?;
    if !magic.starts_with("#?") && !magic.starts_with("#?RADIANCE") {
        return Err("不是 Radiance HDR（缺少 #?RADIANCE 头）".to_string());
    }
    let mut format_ok = false;
    loop {
        let line = read_line(&mut pos).ok_or("HDR 头在分辨率前结束")?;
        if line.is_empty() {
            break;
        }
        if line == "FORMAT=32-bit_rle_rgbe" {
            format_ok = true;
        }
    }
    if !format_ok {
        return Err("只支持 FORMAT=32-bit_rle_rgbe".to_string());
    }
    let res = read_line(&mut pos).ok_or("HDR 缺少分辨率行")?;
    let parts: Vec<&str> = res.split_whitespace().collect();
    if parts.len() != 4 || parts[0] != "-Y" || parts[2] != "+X" {
        return Err(format!(
            "只支持 \"-Y height +X width\" 朝向，实际是 {res:?}"
        ));
    }
    let height: usize = parts[1].parse().map_err(|_| "HDR 高度无效".to_string())?;
    let width: usize = parts[3].parse().map_err(|_| "HDR 宽度无效".to_string())?;
    if width == 0 || height == 0 || width > 32768 || height > 32768 {
        return Err(format!("HDR 尺寸无效：{width}×{height}"));
    }

    let mut rgbe = vec![[0u8; 4]; width * height];
    let mut row = 0usize;
    while row < height {
        if (8..=32767).contains(&width) && pos + 4 <= bytes.len() {
            let head = &bytes[pos..pos + 4];
            if head[0] == 2 && head[1] == 2 && head[2] & 0x80 == 0 {
                let w = ((head[2] as usize) << 8) | head[3] as usize;
                if w != width {
                    return Err(format!("HDR 扫描线宽度 {w} 与头 {width} 不一致"));
                }
                pos += 4;
                let mut scan = vec![0u8; width * 4];
                for c in 0..4 {
                    let mut x = 0usize;
                    while x < width {
                        let count = *bytes.get(pos).ok_or("HDR RLE 数据截断")? as usize;
                        pos += 1;
                        if count > 128 {
                            let run = count - 128;
                            let value = *bytes.get(pos).ok_or("HDR RLE run 截断")?;
                            pos += 1;
                            if x + run > width {
                                return Err("HDR RLE run 越界".to_string());
                            }
                            for _ in 0..run {
                                scan[x * 4 + c] = value;
                                x += 1;
                            }
                        } else {
                            if count == 0 || x + count > width {
                                return Err("HDR RLE literal 越界".to_string());
                            }
                            for _ in 0..count {
                                scan[x * 4 + c] = *bytes.get(pos).ok_or("HDR RLE literal 截断")?;
                                pos += 1;
                                x += 1;
                            }
                        }
                    }
                }
                for x in 0..width {
                    rgbe[row * width + x] = [
                        scan[x * 4],
                        scan[x * 4 + 1],
                        scan[x * 4 + 2],
                        scan[x * 4 + 3],
                    ];
                }
                row += 1;
                continue;
            }
        }
        // 平铺 / 旧 RLE：一个像素 4 字节；旧 RLE 用 (1,1,1,count) 重复**上一个像素**。
        // 注意 count 是"额外重复的像素数"，可以跨行，不是整行同色。
        let mut i = row * width;
        while i < width * height {
            let p = [
                *bytes.get(pos).ok_or("HDR 像素数据截断")?,
                *bytes.get(pos + 1).ok_or("HDR 像素数据截断")?,
                *bytes.get(pos + 2).ok_or("HDR 像素数据截断")?,
                *bytes.get(pos + 3).ok_or("HDR 像素数据截断")?,
            ];
            pos += 4;
            if p[0] == 1 && p[1] == 1 && p[2] == 1 {
                let count = p[3] as usize;
                let prev = if i > 0 { rgbe[i - 1] } else { p };
                for _ in 0..count {
                    if i >= width * height {
                        break;
                    }
                    rgbe[i] = prev;
                    i += 1;
                }
            } else {
                rgbe[i] = p;
                i += 1;
            }
        }
        row = height;
    }

    let mut pixels = Vec::with_capacity(width * height);
    for p in rgbe {
        let e = p[3] as i32;
        if e == 0 {
            pixels.push([0.0; 3]);
        } else {
            let scale = 2f32.powi(e - 136);
            pixels.push([
                p[0] as f32 * scale,
                p[1] as f32 * scale,
                p[2] as f32 * scale,
            ]);
        }
    }
    Ok((width, height, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_hdr(width: usize, height: usize, rgb: [u8; 3], exp: u8) -> Vec<u8> {
        let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
        bytes.extend_from_slice(format!("-Y {height} +X {width}\n").as_bytes());
        for _ in 0..width * height {
            bytes.extend_from_slice(&[rgb[0], rgb[1], rgb[2], exp]);
        }
        bytes
    }

    #[test]
    fn hdr_flat_scanlines_decode_to_linear_rgb() {
        let map = EnvironmentMap::from_hdr(&flat_hdr(2, 1, [128, 64, 32], 128)).unwrap();
        assert_eq!((map.width, map.height), (2, 1));
        // e=128 → 2^-8 = 1/256；128/256 = 0.5。
        assert!((map.pixels[0][0] - 0.5).abs() < 1e-6, "{:?}", map.pixels[0]);
        assert!((map.pixels[0][1] - 0.25).abs() < 1e-6);
        assert!((map.pixels[0][2] - 0.125).abs() < 1e-6);
        assert_eq!(map.pixels[1], map.pixels[0]);
    }

    #[test]
    fn hdr_new_rle_scanlines_decode() {
        let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
        bytes.extend_from_slice(b"-Y 1 +X 8\n");
        bytes.extend_from_slice(&[2, 2, 0, 8]);
        for value in [128u8, 64, 32, 128] {
            bytes.extend_from_slice(&[136, value]); // 128 + 8 个重复
        }
        let map = EnvironmentMap::from_hdr(&bytes).unwrap();
        assert_eq!((map.width, map.height), (8, 1));
        assert!((map.pixels[0][0] - 0.5).abs() < 1e-6);
        assert!((map.pixels[7][2] - 0.125).abs() < 1e-6);
    }

    #[test]
    fn hdr_old_rle_repeats_the_previous_pixel() {
        let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
        bytes.extend_from_slice(b"-Y 1 +X 2\n");
        bytes.extend_from_slice(&[10, 20, 30, 128]);
        bytes.extend_from_slice(&[1, 1, 1, 1]);
        let map = EnvironmentMap::from_hdr(&bytes).unwrap();
        assert_eq!(map.pixels.len(), 2);
        assert_eq!(map.pixels[0], map.pixels[1]);
    }

    #[test]
    fn hdr_rejects_unsupported_orientation() {
        let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
        bytes.extend_from_slice(b"+Y 1 +X 2\n");
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(EnvironmentMap::from_hdr(&bytes).is_err());
    }

    #[test]
    fn cubemap_has_all_faces_and_shrinking_mips() {
        let map = EnvironmentMap {
            width: 8,
            height: 4,
            pixels: (0..32)
                .map(|i| [i as f32 / 32.0, 0.25, 1.0 - i as f32 / 32.0])
                .collect(),
            intensity: 1.0,
            rotation: 0.0,
        };
        let cube = map.to_cube(2);
        assert_eq!(cube.mips[0].size, 2);
        assert_eq!(cube.mips.last().unwrap().size, 1);
        for mip in &cube.mips {
            for face in &mip.faces {
                assert_eq!(face.len(), (mip.size * mip.size) as usize);
                assert!(face.iter().flatten().all(|v| v.is_finite()));
            }
        }
        // +X 与 -X 的中心方向相反，渐变图应给出不同的颜色。
        assert_ne!(cube.mips[0].faces[0][0], cube.mips[0].faces[1][0]);
    }

    #[test]
    fn equirect_uv_maps_axes_to_expected_edges() {
        let (_, v) = equirect_uv(Vec3::new(0.0, 1.0, 0.0));
        assert!((v - 0.0).abs() < 1e-6, "顶部 v=0");
        let (_, v) = equirect_uv(Vec3::new(0.0, -1.0, 0.0));
        assert!((v - 1.0).abs() < 1e-6, "底部 v=1");
        let (u, _) = equirect_uv(Vec3::new(1.0, 0.0, 0.0));
        assert!((u - 0.5).abs() < 1e-6, "+X 在 u=0.5");
    }
}
