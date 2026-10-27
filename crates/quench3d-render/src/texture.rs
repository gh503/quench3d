//! CPU 侧图像数据与 GPU 纹理上传（含 mipmap 生成）。

use quench3d_math::Vec2;
use wgpu::util::DeviceExt;

/// CPU 侧 RGBA8 图像。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageData {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// RGBA8 像素（行优先，左上角为原点）。
    pub rgba: Vec<u8>,
}

/// 图像相关错误。
#[derive(Debug)]
pub enum ImageError {
    /// 解码失败。
    Decode(String),
    /// IO 失败。
    Io(String),
    /// 数据长度与尺寸不匹配。
    SizeMismatch,
    /// 编码失败。
    Encode(String),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(m) => write!(f, "图像解码失败: {m}"),
            Self::Io(m) => write!(f, "读取图像失败: {m}"),
            Self::SizeMismatch => write!(f, "像素数据长度与宽高不匹配"),
            Self::Encode(m) => write!(f, "图像编码失败: {m}"),
        }
    }
}

impl std::error::Error for ImageError {}

/// KTX2 容器标识（`«KTX 20»\r\n\x1a\n`）。
const KTX2_ID: [u8; 12] = [
    0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb, 0x0d, 0x0a, 0x1a, 0x0a,
];

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn ktx2_error(message: impl Into<String>) -> ImageError {
    ImageError::Decode(format!("KTX2: {}", message.into()))
}

/// 解码 KTX2 的 level 0。当前支持未压缩 RGBA8 与 BC1（CPU 解成 RGBA8）；
/// BasisLZ/UASTC/BC7 等仍是未支持的压缩格式。
fn decode_ktx2(bytes: &[u8]) -> Result<ImageData, ImageError> {
    if bytes.len() < 12 + 68 + 24 || !bytes.starts_with(&KTX2_ID) {
        return Err(ktx2_error("文件头不完整"));
    }
    let vk_format = u32_at(bytes, 12).ok_or_else(|| ktx2_error("缺少 vkFormat"))?;
    let width = u32_at(bytes, 20).ok_or_else(|| ktx2_error("缺少宽度"))?;
    let height = u32_at(bytes, 24).ok_or_else(|| ktx2_error("缺少高度"))?;
    let depth = u32_at(bytes, 28).unwrap_or(0);
    let layers = u32_at(bytes, 32).unwrap_or(0);
    let faces = u32_at(bytes, 36).unwrap_or(1);
    let levels = u32_at(bytes, 40).unwrap_or(1).max(1);
    let supercompression = u32_at(bytes, 44).unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(ktx2_error("宽高不能为 0"));
    }
    if depth > 1 || layers > 1 || faces != 1 {
        return Err(ktx2_error("只支持 2D、单层、单面的 level 0"));
    }
    if supercompression != 0 {
        return Err(ktx2_error("不支持 BasisLZ/超压缩，请用未压缩或 BC1"));
    }
    if levels == 0 {
        return Err(ktx2_error("levelCount 非法"));
    }
    let offset = u64_at(bytes, 80).ok_or_else(|| ktx2_error("缺少 level 索引"))?;
    let length = u64_at(bytes, 88).ok_or_else(|| ktx2_error("缺少 level 长度"))?;
    let offset = usize::try_from(offset).map_err(|_| ktx2_error("level 偏移溢出"))?;
    let length = usize::try_from(length).map_err(|_| ktx2_error("level 长度溢出"))?;
    let payload = bytes
        .get(
            offset
                ..offset
                    .checked_add(length)
                    .ok_or_else(|| ktx2_error("level 长度溢出"))?,
        )
        .ok_or_else(|| ktx2_error("level 数据越界"))?;

    match vk_format {
        // VK_FORMAT_R8G8B8A8_UNORM / SRGB
        37 | 43 => {
            let pixels = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| ktx2_error("像素数溢出"))?;
            let rgba = payload
                .get(..pixels)
                .ok_or_else(|| ktx2_error("RGBA8 level 数据不足"))?
                .to_vec();
            Ok(ImageData {
                width,
                height,
                rgba,
            })
        }
        // VK_FORMAT_BC1_RGB/RGBA_UNORM/SRGB_BLOCK
        131..=134 => {
            let rgba = decode_bc1(payload, width, height, vk_format >= 133)?;
            Ok(ImageData {
                width,
                height,
                rgba,
            })
        }
        other => Err(ktx2_error(format!(
            "暂不支持 VkFormat {other}（仅 RGBA8 与 BC1；BasisU 仍未支持）"
        ))),
    }
}

/// 把 BC1 块解成 RGBA8。`rgba` 为真时启用 BC1 的 1-bit alpha 模式。
fn decode_bc1(payload: &[u8], width: u32, height: u32, rgba: bool) -> Result<Vec<u8>, ImageError> {
    let blocks_x = (width as usize).div_ceil(4);
    let blocks_y = (height as usize).div_ceil(4);
    let needed = blocks_x
        .checked_mul(blocks_y)
        .and_then(|n| n.checked_mul(8))
        .ok_or_else(|| ktx2_error("BC1 数据量溢出"))?;
    if payload.len() < needed {
        return Err(ktx2_error("BC1 level 数据不足"));
    }
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let block = &payload[(by * blocks_x + bx) * 8..][..8];
            let c0 = u16::from_le_bytes([block[0], block[1]]);
            let c1 = u16::from_le_bytes([block[2], block[3]]);
            let color = |c: u16| -> [u8; 4] {
                let r = ((c >> 11) & 0x1f) as u32 * 255 / 31;
                let g = ((c >> 5) & 0x3f) as u32 * 255 / 63;
                let b = (c & 0x1f) as u32 * 255 / 31;
                [r as u8, g as u8, b as u8, 255]
            };
            let a = color(c0);
            let b = color(c1);
            let mut palette = [[0u8; 4]; 4];
            palette[0] = a;
            palette[1] = b;
            if c0 > c1 {
                palette[2] = [
                    ((2 * a[0] as u32 + b[0] as u32) / 3) as u8,
                    ((2 * a[1] as u32 + b[1] as u32) / 3) as u8,
                    ((2 * a[2] as u32 + b[2] as u32) / 3) as u8,
                    255,
                ];
                palette[3] = [
                    ((a[0] as u32 + 2 * b[0] as u32) / 3) as u8,
                    ((a[1] as u32 + 2 * b[1] as u32) / 3) as u8,
                    ((a[2] as u32 + 2 * b[2] as u32) / 3) as u8,
                    255,
                ];
            } else {
                palette[2] = [
                    ((a[0] as u32 + b[0] as u32) / 2) as u8,
                    ((a[1] as u32 + b[1] as u32) / 2) as u8,
                    ((a[2] as u32 + b[2] as u32) / 2) as u8,
                    255,
                ];
                palette[3] = [0, 0, 0, if rgba { 0 } else { 255 }];
            }
            let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
            for py in 0..4 {
                for px in 0..4 {
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= width as usize || y >= height as usize {
                        continue;
                    }
                    let index = ((bits >> (2 * (py * 4 + px))) & 0x3) as usize;
                    let dst = (y * width as usize + x) * 4;
                    out[dst..dst + 4].copy_from_slice(&palette[index]);
                }
            }
        }
    }
    Ok(out)
}

impl ImageData {
    /// 由 RGBA8 数据构造（校验长度）。
    /// 编码为 PNG 字节流（Web 与桌面端都可用，便于下载或写盘）。
    pub fn write_png(&self, out: &mut Vec<u8>) -> Result<(), ImageError> {
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(out);
        encoder
            .write_image(
                &self.rgba,
                self.width,
                self.height,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| ImageError::Encode(e.to_string()))
    }

    /// 从 RGBA8 数据构造。
    pub fn from_rgba8(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ImageError> {
        if rgba.len() != (width as usize) * (height as usize) * 4 {
            return Err(ImageError::SizeMismatch);
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// 1x1 纯色。
    pub fn solid(color: [u8; 4]) -> Self {
        Self {
            width: 1,
            height: 1,
            rgba: color.to_vec(),
        }
    }

    /// 白色 1x1（默认贴图）。
    pub fn white() -> Self {
        Self::solid([255, 255, 255, 255])
    }

    /// 默认法线贴图（指向 +Z）。
    pub fn flat_normal() -> Self {
        Self::solid([128, 128, 255, 255])
    }

    /// 从 PNG/JPEG 字节解码。
    pub fn from_encoded(bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.starts_with(&KTX2_ID) {
            return decode_ktx2(bytes);
        }
        let img = image::load_from_memory(bytes).map_err(|e| ImageError::Decode(e.to_string()))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        Ok(Self {
            width: w,
            height: h,
            rgba: rgba.into_raw(),
        })
    }

    /// 从文件读取。
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, ImageError> {
        let bytes = std::fs::read(path).map_err(|e| ImageError::Io(e.to_string()))?;
        Self::from_encoded(&bytes)
    }

    /// 垂直翻转（把左上原点转成左下原点）。
    pub fn flip_vertical(&mut self) {
        let stride = self.width as usize * 4;
        let h = self.height as usize;
        for y in 0..h / 2 {
            let (a, b) = (y * stride, (h - 1 - y) * stride);
            for i in 0..stride {
                self.rgba.swap(a + i, b + i);
            }
        }
    }

    /// 生成棋盘格纹理（演示/占位用）。
    pub fn checker(size: u32, cells: u32, a: [u8; 4], b: [u8; 4]) -> Self {
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        let cell = (size / cells.max(1)).max(1);
        for y in 0..size {
            for x in 0..size {
                let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                let c = if on { a } else { b };
                let i = ((y * size + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&c);
            }
        }
        Self {
            width: size,
            height: size,
            rgba,
        }
    }

    /// 简易值噪声（确定性的，用于"没有美术资源也能看出材质细节"）。
    pub fn noise(size: u32, seed: u32) -> Self {
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        let mut state = seed | 1;
        let mut next = || {
            // xorshift32
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for i in 0..(size * size) as usize {
            let v = (next() >> 24) as u8;
            rgba[i * 4] = v;
            rgba[i * 4 + 1] = v;
            rgba[i * 4 + 2] = v;
            rgba[i * 4 + 3] = 255;
        }
        Self {
            width: size,
            height: size,
            rgba,
        }
    }

    /// 用盒式滤波生成完整的 mipmap 链（从 1x1 到原尺寸倒序返回）。
    pub fn generate_mip_chain(&self) -> Vec<(u32, u32, Vec<u8>)> {
        let mut levels = Vec::new();
        let mut w = self.width;
        let mut h = self.height;
        let mut data = self.rgba.clone();
        levels.push((w, h, data.clone()));
        while w > 1 || h > 1 {
            let nw = (w / 2).max(1);
            let nh = (h / 2).max(1);
            let mut next = vec![0u8; (nw * nh * 4) as usize];
            for y in 0..nh {
                for x in 0..nw {
                    let mut acc = [0u32; 4];
                    let mut count = 0u32;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let sx = (x * 2 + dx).min(w - 1);
                            let sy = (y * 2 + dy).min(h - 1);
                            let i = ((sy * w + sx) * 4) as usize;
                            for c in 0..4 {
                                acc[c] += data[i + c] as u32;
                            }
                            count += 1;
                        }
                    }
                    let o = ((y * nw + x) * 4) as usize;
                    for c in 0..4 {
                        next[o + c] = (acc[c] / count.max(1)) as u8;
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

    /// 尺寸（像素）。
    pub fn size(&self) -> Vec2 {
        Vec2::new(self.width as f32, self.height as f32)
    }
}

/// 纹理采样器过滤方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// 最近邻（像素风）。
    Nearest,
    /// 线性 + 各向异性（默认）。
    Linear,
}

/// 纹理上传选项。
#[derive(Clone, Copy, Debug)]
pub struct TextureOptions {
    /// 是否按 sRGB 解释（颜色贴图应为 `true`，法线/粗糙度贴图应为 `false`）。
    pub srgb: bool,
    /// 是否生成 mipmap。
    pub mipmaps: bool,
    /// 过滤方式。
    pub filter: Filter,
    /// 是否重复平铺。
    pub repeat: bool,
    /// 调试标签。
    pub label: &'static str,
}

impl Default for TextureOptions {
    fn default() -> Self {
        Self {
            srgb: true,
            mipmaps: true,
            filter: Filter::Linear,
            repeat: true,
            label: "quench3d-texture",
        }
    }
}

impl TextureOptions {
    /// 颜色贴图（sRGB + mipmap + 线性过滤）。
    pub fn color() -> Self {
        Self::default()
    }

    /// 数据贴图（法线/粗糙度等，非 sRGB）。
    pub fn data() -> Self {
        Self {
            srgb: false,
            ..Default::default()
        }
    }
}

/// GPU 纹理（含视图与采样器）。
#[derive(Debug)]
pub struct GpuTexture {
    /// 纹理。
    pub texture: wgpu::Texture,
    /// 视图。
    pub view: wgpu::TextureView,
    /// 采样器。
    pub sampler: wgpu::Sampler,
    /// 尺寸。
    pub size: (u32, u32),
    /// mip 级数。
    pub mip_levels: u32,
}

fn texture_sampler(device: &wgpu::Device, options: TextureOptions) -> wgpu::Sampler {
    let (min_filter, mag_filter, mipmap_filter) = match options.filter {
        Filter::Nearest => (
            wgpu::FilterMode::Nearest,
            wgpu::FilterMode::Nearest,
            wgpu::MipmapFilterMode::Nearest,
        ),
        Filter::Linear => (
            wgpu::FilterMode::Linear,
            wgpu::FilterMode::Linear,
            if options.mipmaps {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
        ),
    };
    let address = if options.repeat {
        wgpu::AddressMode::Repeat
    } else {
        wgpu::AddressMode::ClampToEdge
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("quench3d-sampler"),
        address_mode_u: address,
        address_mode_v: address,
        address_mode_w: address,
        mag_filter,
        min_filter,
        mipmap_filter,
        // 各向异性过滤要求所有过滤模式都是线性
        anisotropy_clamp: if matches!(options.filter, Filter::Linear) && options.mipmaps {
            8
        } else {
            1
        },
        ..Default::default()
    })
}

impl GpuTexture {
    /// 上传浮点图像（`Rgba16Float`，线性，不做 sRGB 转换）。
    ///
    /// 用于 HDR/EXR 贴图。`Rgba16Float` 在 WebGPU 核心里可过滤；WebGL2 后端
    /// 若缺少半浮点线性过滤，纹理会退化为最近邻（采样器由 wgpu 校验）。
    pub fn upload_float(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &crate::hdr_image::FloatImage,
        options: TextureOptions,
    ) -> Self {
        let format = wgpu::TextureFormat::Rgba16Float;
        let chain = if options.mipmaps {
            image.generate_mip_chain()
        } else {
            vec![(image.width, image.height, image.rgba.clone())]
        };
        let mip_levels = chain.len() as u32;
        let (w, h) = (image.width.max(1), image.height.max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(options.label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, (lw, lh, pixels)) in chain.iter().enumerate() {
            let mut bytes = Vec::with_capacity(pixels.len() * 8);
            for p in pixels {
                for c in p {
                    bytes.extend_from_slice(
                        &half::f16::from_f32(c.clamp(-65504.0, 65504.0)).to_le_bytes(),
                    );
                }
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(lw * 8),
                    rows_per_image: Some(*lh),
                },
                wgpu::Extent3d {
                    width: *lw,
                    height: *lh,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = texture_sampler(device, options);
        Self {
            texture,
            view,
            sampler,
            size: (w, h),
            mip_levels,
        }
    }

    /// 上传图像到 GPU。
    pub fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
        options: TextureOptions,
    ) -> Self {
        let format = if options.srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };

        let chain = if options.mipmaps {
            image.generate_mip_chain()
        } else {
            vec![(image.width, image.height, image.rgba.clone())]
        };
        let mip_levels = chain.len() as u32;
        let (w, h) = (image.width.max(1), image.height.max(1));

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(options.label),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        for (level, (lw, lh, data)) in chain.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(lw * 4),
                    rows_per_image: Some(*lh),
                },
                wgpu::Extent3d {
                    width: *lw,
                    height: *lh,
                    depth_or_array_layers: 1,
                },
            );
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = texture_sampler(device, options);

        Self {
            texture,
            view,
            sampler,
            size: (w, h),
            mip_levels,
        }
    }
}

/// 创建 1x1 默认纹理（用于材质中没有贴图的槽位）。
pub fn default_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &ImageData,
    label: &'static str,
) -> GpuTexture {
    GpuTexture::upload(
        device,
        queue,
        image,
        TextureOptions {
            mipmaps: false,
            label,
            ..Default::default()
        },
    )
}

/// 用 `DeviceExt` 简化缓冲区创建的辅助函数。
pub fn buffer_with_data(
    device: &wgpu::Device,
    label: &str,
    contents: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_halves_until_one() {
        let img = ImageData::checker(8, 2, [255, 0, 0, 255], [0, 0, 255, 255]);
        let chain = img.generate_mip_chain();
        let sizes: Vec<(u32, u32)> = chain.iter().map(|(w, h, _)| (*w, *h)).collect();
        assert_eq!(
            sizes,
            vec![(8, 8), (4, 4), (2, 2), (1, 1)],
            "mip 链尺寸错误: {sizes:?}"
        );
        for (w, h, d) in &chain {
            assert_eq!(d.len(), (*w as usize) * (*h as usize) * 4);
        }
    }

    fn ktx2_header(vk_format: u32, width: u32, height: u32, payload: &[u8]) -> Vec<u8> {
        const ID: [u8; 12] = [
            0xab, b'K', b'T', b'X', b' ', b'2', b'0', 0xbb, 0x0d, 0x0a, 0x1a, 0x0a,
        ];
        let mut out = Vec::new();
        out.extend_from_slice(&ID);
        for v in [
            vk_format, 1, // typeSize
            width, height, 0, // depth
            0, // layers
            1, // faces
            1, // levels
            0, // supercompression
            0, 0, // dfd
            0, 0, // kvd
        ] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&0u64.to_le_bytes()); // sgd offset
        out.extend_from_slice(&0u64.to_le_bytes()); // sgd length
        let header_len = (out.len() + 24) as u64; // level index + one level
        out.extend_from_slice(&header_len.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn ktx2_uncompressed_rgba8_is_decoded() {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
        ];
        let bytes = ktx2_header(37, 2, 2, &pixels);
        let img = ImageData::from_encoded(&bytes).expect("KTX2 RGBA8");
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.rgba, pixels);
    }

    #[test]
    fn ktx2_bc1_block_is_decoded() {
        // 4x4 BC1：color0 = 红色 0xF800，color1 = 0，索引全 0。
        let block = [0x00, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let bytes = ktx2_header(131, 4, 4, &block);
        let img = ImageData::from_encoded(&bytes).expect("KTX2 BC1");
        assert_eq!((img.width, img.height), (4, 4));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(img.rgba.len(), 4 * 4 * 4);
        for i in (0..img.rgba.len()).step_by(4) {
            assert_eq!(&img.rgba[i..i + 4], &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn ktx2_rejects_truncated_and_unknown_formats() {
        let bytes = ktx2_header(37, 2, 2, &[0; 16]);
        assert!(ImageData::from_encoded(&bytes[..40]).is_err());
        let bytes = ktx2_header(999, 2, 2, &[0; 16]);
        let err = ImageData::from_encoded(&bytes).expect_err("未知 VkFormat 要报错");
        assert!(format!("{err}").contains("KTX2"));
    }

    #[test]
    fn flip_vertical_swaps_rows() {
        let mut img = ImageData {
            width: 1,
            height: 2,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        img.flip_vertical();
        assert_eq!(img.rgba, vec![5, 6, 7, 8, 1, 2, 3, 4]);
    }

    #[test]
    fn size_mismatch_is_rejected() {
        assert!(ImageData::from_rgba8(2, 2, vec![0; 8]).is_err());
        assert!(ImageData::from_rgba8(1, 1, vec![0; 4]).is_ok());
    }
}
