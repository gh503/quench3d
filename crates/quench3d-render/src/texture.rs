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

impl GpuTexture {
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
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
        });

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
