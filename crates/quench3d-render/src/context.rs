//! GPU 上下文：实例 / 适配器 / 设备 / 交换链的创建与生命周期管理。
//!
//! 桌面端由上层（`quench3d` 的窗口层）创建 `wgpu::Surface` 后传入；
//! Web 端由 canvas 创建。渲染器本身不依赖任何窗口库，因此同一份代码
//! 既能跑桌面也能跑 wasm，还能离屏渲染（截图 / 无头测试）。

use std::sync::Arc;

use crate::mesh::Vertex;

/// 初始化错误。
#[derive(Debug)]
pub enum GpuInitError {
    /// 没有找到可用的图形适配器。
    NoAdapter(String),
    /// 请求设备失败。
    DeviceRequest(String),
    /// 创建 Surface 失败。
    Surface(String),
    /// 面配置不受支持。
    UnsupportedSurface,
}

impl std::fmt::Display for GpuInitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(m) => write!(f, "找不到可用的 GPU 适配器: {m}"),
            Self::DeviceRequest(m) => write!(f, "请求 GPU 设备失败: {m}"),
            Self::Surface(m) => write!(f, "创建渲染面失败: {m}"),
            Self::UnsupportedSurface => write!(f, "当前适配器不支持该渲染面配置"),
        }
    }
}

impl std::error::Error for GpuInitError {}

/// 取帧失败的原因（对应交换链状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// 取帧超时：跳过本帧，稍后重试。
    Timeout,
    /// 窗口被遮挡（最小化等）：跳过本帧。
    Occluded,
    /// 交换链配置过期：需要 `resize()` / 重新配置。
    Outdated,
    /// 交换链丢失：需要重建渲染面。
    Lost,
    /// 取帧时的验证错误。
    Validation,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Timeout => "取帧超时",
            Self::Occluded => "窗口被遮挡",
            Self::Outdated => "交换链配置过期",
            Self::Lost => "交换链丢失",
            Self::Validation => "取帧验证错误",
        };
        f.write_str(s)
    }
}

impl std::error::Error for FrameError {}

/// 渲染器初始化选项。
#[derive(Clone, Copy, Debug)]
pub struct GpuOptions {
    /// 多重采样数（1 / 2 / 4 / 8）。
    pub msaa_samples: u32,
    /// 垂直同步（桌面端有效）。
    pub vsync: bool,
    /// 是否优先选择低功耗适配器。
    pub low_power: bool,
    /// 强制指定的交换链格式。
    pub format: Option<wgpu::TextureFormat>,
}

impl Default for GpuOptions {
    fn default() -> Self {
        Self {
            msaa_samples: 4,
            vsync: true,
            low_power: false,
            format: None,
        }
    }
}

/// GPU 上下文。
pub struct GpuContext {
    /// wgpu 实例。
    pub instance: wgpu::Instance,
    /// 适配器。
    pub adapter: wgpu::Adapter,
    /// 逻辑设备。
    pub device: wgpu::Device,
    /// 命令队列。
    pub queue: wgpu::Queue,
    /// 渲染面（离屏模式为 `None`）。
    pub surface: Option<wgpu::Surface<'static>>,
    /// 渲染面配置。
    pub config: Option<wgpu::SurfaceConfiguration>,
    /// 当前尺寸（像素）。
    pub size: (u32, u32),
    /// 颜色格式。
    pub format: wgpu::TextureFormat,
    /// 多重采样数。
    pub msaa_samples: u32,
    /// 适配器信息（日志用）。
    pub adapter_info: wgpu::AdapterInfo,
    /// 是否为软件光栅化后端。
    pub is_software: bool,
    /// 交换链被报告为次优，下一帧前需要重新配置。
    needs_reconfigure: bool,
}

impl std::fmt::Debug for GpuContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuContext")
            .field("adapter", &self.adapter_info.name)
            .field("backend", &self.adapter_info.backend)
            .field("size", &self.size)
            .field("format", &self.format)
            .field("msaa", &self.msaa_samples)
            .field("software", &self.is_software)
            .finish()
    }
}

/// 离屏渲染目标（用于无头测试与截图）。
pub struct OffscreenTarget {
    /// 颜色纹理。
    pub texture: wgpu::Texture,
    /// 颜色视图。
    pub view: wgpu::TextureView,
    /// 尺寸。
    pub size: (u32, u32),
    /// 格式。
    pub format: wgpu::TextureFormat,
}

impl GpuContext {
    /// 环境变量 `WGPU_BACKEND` 指定的后端（没写就是 `None`）。
    fn env_backends() -> Option<wgpu::Backends> {
        let env = std::env::var("WGPU_BACKEND").ok()?;
        Some(Self::parse_backend_hint(&env).unwrap_or_else(wgpu::Backends::all))
    }

    /// 把后端名字解析成 `Backends`（不认识的返回 `None`）。名字与 `WGPU_BACKEND`
    /// 以及页面的 `__quench3d_backend` 共用。
    fn parse_backend_hint(hint: &str) -> Option<wgpu::Backends> {
        Some(match hint.to_ascii_lowercase().as_str() {
            "vulkan" | "vk" => wgpu::Backends::VULKAN,
            "metal" | "mtl" => wgpu::Backends::METAL,
            "dx12" | "d3d12" => wgpu::Backends::DX12,
            "gl" | "gles" | "opengl" | "webgl2" => wgpu::Backends::GL,
            "webgpu" | "wgpu" => wgpu::Backends::BROWSER_WEBGPU,
            "primary" => wgpu::Backends::PRIMARY,
            "secondary" => wgpu::Backends::SECONDARY,
            "all" => wgpu::Backends::all(),
            _ => return None,
        })
    }

    /// 由"后端提示"（页面探测结果 / `WGPU_BACKEND`）排出候选列表。
    ///
    /// - `Some("webgpu")` —— 探测到真有 WebGPU 适配器：WebGPU 优先，WebGL2 兜底；
    /// - `Some("gl")` —— 没有 WebGPU（或探测失败）：**只**试 WebGL2。这一点很关键，
    ///   详见 [`GpuContext::from_candidates_with`]：对着画布要过 `webgpu` 上下文之后，
    ///   同一张画布的 `getContext("webgl2")` 会返回 null，回退就没机会了；
    /// - `None` / 不认识 —— 用默认顺序 [`GpuContext::backend_candidates`]。
    ///
    /// 返回结果按"编译进来的后端"过滤，不会给出该构建根本没有的后端。
    pub fn candidates_from_hint(hint: Option<&str>) -> Vec<wgpu::Backends> {
        let compiled = wgpu::Instance::enabled_backend_features();
        let mapped = match hint.and_then(Self::parse_backend_hint) {
            Some(wgpu::Backends::BROWSER_WEBGPU) => {
                vec![wgpu::Backends::BROWSER_WEBGPU, wgpu::Backends::GL]
            }
            Some(b) => vec![b],
            None => Self::backend_candidates(),
        };
        let filtered: Vec<wgpu::Backends> = mapped
            .into_iter()
            .filter(|b| compiled.contains(*b))
            .collect();
        if filtered.is_empty() {
            Self::backend_candidates()
        } else {
            filtered
        }
    }

    /// **依次尝试**的后端清单（按优先级）。调用方要挨个试，直到某个后端真的给得出适配器。
    ///
    /// 为什么要"挨个试"而不是一把传 `Backends::all()`：在 Web 上，只要
    /// `InstanceDescriptor::backends` 里带了 `BROWSER_WEBGPU` **且 `navigator.gpu` 存在**，
    /// wgpu 就会直接走纯 WebGPU 上下文（见 `wgpu::Instance::new` 的 `requested_webgpu &&
    /// support_webgpu` 分支），**连 WebGL2 实例都不创建**。可是 `navigator.gpu` 存在不等于
    /// 有可用适配器：Linux Chrome 没开 Vulkan（或软件光栅的 SwiftShader 环境）就是
    /// "navigator.gpu 有、requestAdapter() 返回 null"，于是报
    /// `No available adapters`，而我们承诺的 WebGL2 回退永远轮不到。
    /// 这里把 WebGPU 与 WebGL2 拆成两次独立的尝试，回退才真的会发生。
    ///
    /// 桌面端只有一个候选（`Backends::all()`），行为与以前完全一致。
    pub fn backend_candidates() -> Vec<wgpu::Backends> {
        if let Some(b) = Self::env_backends() {
            return vec![b];
        }
        let compiled = wgpu::Instance::enabled_backend_features();
        #[cfg(target_arch = "wasm32")]
        {
            // WebGPU 优先（功能更全：计算、storage、浮点混合……），拿不到再退 WebGL2。
            let mut out = Vec::new();
            for want in [wgpu::Backends::BROWSER_WEBGPU, wgpu::Backends::GL] {
                if compiled.contains(want) {
                    out.push(want);
                }
            }
            if out.is_empty() {
                out.push(compiled);
            }
            out
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // 桌面端只有一个候选：一把交给 wgpu 自己按优先级挑（Vulkan/Metal/DX12/GL），
            // 行为与改造前完全一致。`compiled` 只是为了让 wasm 分支外的变量不未使用。
            let _ = compiled;
            vec![wgpu::Backends::all()]
        }
    }

    /// 用指定后端创建 wgpu 实例。
    pub fn create_instance_with(backends: wgpu::Backends) -> wgpu::Instance {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        wgpu::Instance::new(desc)
    }

    /// 按环境变量 `WGPU_BACKEND`（或全部候选里的第一个）创建 wgpu 实例。
    ///
    /// 想要"拿不到适配器就换个后端再试"，请用 [`GpuContext::backend_candidates`]。
    pub fn create_instance() -> wgpu::Instance {
        let backends = Self::env_backends()
            .or_else(|| Self::backend_candidates().into_iter().next())
            .unwrap_or(wgpu::Backends::all());
        Self::create_instance_with(backends)
    }

    /// 列出所有可用适配器（诊断用）。
    pub async fn list_adapters() -> Vec<wgpu::AdapterInfo> {
        let instance = Self::create_instance();
        instance
            .enumerate_adapters(wgpu::Backends::all())
            .await
            .into_iter()
            .map(|a| a.get_info())
            .collect()
    }

    async fn request_device(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
        options: GpuOptions,
    ) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue, wgpu::AdapterInfo), GpuInitError> {
        let power = if options.low_power {
            wgpu::PowerPreference::LowPower
        } else {
            wgpu::PowerPreference::HighPerformance
        };
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: power,
                compatible_surface,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| {
                // 回退：有时独显不可用，尝试默认适配器与软件适配器。
                GpuInitError::NoAdapter(format!("{e:?}"))
            })?;
        let info = adapter.get_info();

        // 限制档位要按适配器能力挑，挑高了 `request_device` 会**直接失败**。
        //
        // 这里踩过一个真实的坑：浏览器里的 WebGL2 后端，适配器类型是 `Other`
        // 而不是 `Cpu`，于是原来的判断会给它请求 `Limits::default()`——
        // WebGL2 顶不住（例如 max_storage_buffers_per_shader_stage、
        // 纹理尺寸、binding 数量），`request_device` 失败，页面表现就是
        //「启动成功但一块黑屏」。现在按后端先挑一档，失败再依次降档重试，
        // 每档都打日志，方便定位。
        let ladder: Vec<(&str, wgpu::Limits)> = match info.backend {
            // 浏览器 WebGL2（以及原生 GL/GLES）：必须用 webgl2 档
            wgpu::Backend::Gl => vec![
                (
                    "downlevel_webgl2",
                    wgpu::Limits::downlevel_webgl2_defaults(),
                ),
                ("downlevel", wgpu::Limits::downlevel_defaults()),
            ],
            // 软件光栅（llvmpipe 之类）：downlevel 更稳
            _ if info.device_type == wgpu::DeviceType::Cpu => vec![
                ("downlevel", wgpu::Limits::downlevel_defaults()),
                (
                    "downlevel_webgl2",
                    wgpu::Limits::downlevel_webgl2_defaults(),
                ),
            ],
            // 正常独显/集显/浏览器 WebGPU：先用默认档，失败再降
            _ => vec![
                ("default", wgpu::Limits::default()),
                ("downlevel", wgpu::Limits::downlevel_defaults()),
                (
                    "downlevel_webgl2",
                    wgpu::Limits::downlevel_webgl2_defaults(),
                ),
            ],
        };

        let mut last_error = None;
        for (label, limits) in ladder {
            let result = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("quench3d-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    ..Default::default()
                })
                .await;
            match result {
                Ok((device, queue)) => {
                    log::info!(
                        "渲染设备就绪：{} / {:?}（限制档位 {label}）",
                        info.name,
                        info.backend
                    );
                    return Ok((adapter, device, queue, info));
                }
                Err(e) => {
                    log::warn!("请求设备失败（限制档位 {label}）：{e:?}，降档重试");
                    last_error = Some(e);
                }
            }
        }

        Err(GpuInitError::DeviceRequest(format!(
            "所有限制档位都失败：{:?}",
            last_error
        )))
    }

    /// 由已创建的渲染面初始化（桌面窗口 / Web canvas）。
    ///
    /// 单次尝试：只用 `instance` 带来的后端。想要"拿不到适配器就换后端重试"，
    /// 用 [`GpuContext::from_candidates`]。
    pub async fn from_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
        options: GpuOptions,
    ) -> Result<Self, GpuInitError> {
        let (adapter, device, queue, adapter_info) =
            Self::request_device(instance.clone(), Some(&surface), options).await?;
        Self::build(
            instance,
            adapter,
            device,
            queue,
            adapter_info,
            Some(surface),
            width,
            height,
            options,
        )
    }

    /// **依次尝试候选后端**建实例，直到某个后端真的给出适配器。
    ///
    /// `make_surface` 负责"用这个实例创建渲染面"：窗口/画布那条路要真的建面，
    /// 无头那条路直接返回 `Ok(None)`。Web 上这是 WebGPU → WebGL2 回退的落点
    /// （原因见 [`GpuContext::backend_candidates`]）。
    pub async fn from_candidates<F>(
        width: u32,
        height: u32,
        options: GpuOptions,
        make_surface: F,
    ) -> Result<Self, GpuInitError>
    where
        F: FnMut(&wgpu::Instance) -> Result<Option<wgpu::Surface<'static>>, GpuInitError>,
    {
        let candidates = Self::backend_candidates();
        Self::from_candidates_with(&candidates, width, height, options, make_surface).await
    }

    /// 与 [`GpuContext::from_candidates`] 相同，但由调用方给出候选后端顺序。
    ///
    /// Web 门面层要用它：浏览器里"WebGPU 能不能用"只有页面探得到（`navigator.gpu`
    /// 存在 ≠ 有适配器），而且**一旦对着画布要过 `webgpu` 上下文，同一张画布就再也要不到
    /// `webgl2`**（`canvas.getContext()` 返回 null）。所以顺序必须先探清楚再决定，
    /// 不能埋着头先试 WebGPU。
    pub async fn from_candidates_with<F>(
        candidates: &[wgpu::Backends],
        width: u32,
        height: u32,
        options: GpuOptions,
        mut make_surface: F,
    ) -> Result<Self, GpuInitError>
    where
        F: FnMut(&wgpu::Instance) -> Result<Option<wgpu::Surface<'static>>, GpuInitError>,
    {
        let candidates = if candidates.is_empty() {
            Self::backend_candidates()
        } else {
            candidates.to_vec()
        };
        let mut failures: Vec<String> = Vec::new();
        for backends in &candidates {
            let instance = Self::create_instance_with(*backends);
            let surface = match make_surface(&instance) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("后端 {backends:?} 建渲染面失败：{e}");
                    failures.push(format!("{backends:?}: {e}"));
                    continue;
                }
            };
            match Self::request_device(instance.clone(), surface.as_ref(), options).await {
                Ok((adapter, device, queue, adapter_info)) => {
                    log::info!(
                        "GPU 就绪：{} ({:?})，候选 {:?}",
                        adapter_info.name,
                        adapter_info.backend,
                        candidates
                    );
                    return Self::build(
                        instance,
                        adapter,
                        device,
                        queue,
                        adapter_info,
                        surface,
                        width,
                        height,
                        options,
                    );
                }
                Err(e) => {
                    log::warn!("后端 {backends:?} 拿不到设备：{e}");
                    failures.push(format!("{backends:?}: {e}"));
                }
            }
        }
        Err(GpuInitError::NoAdapter(format!(
            "所有候选后端都不可用 —— {}",
            failures.join("；")
        )))
    }

    /// 把已经拿到手的 wgpu 对象装配成 `GpuContext`（有渲染面就顺手配置好）。
    #[allow(clippy::too_many_arguments)]
    fn build(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
        surface: Option<wgpu::Surface<'static>>,
        width: u32,
        height: u32,
        options: GpuOptions,
    ) -> Result<Self, GpuInitError> {
        let (format, config) = match &surface {
            Some(surface) => {
                let mut config = surface
                    .get_default_config(&adapter, width.max(1), height.max(1))
                    .ok_or(GpuInitError::UnsupportedSurface)?;
                if let Some(f) = options.format {
                    config.format = f;
                }
                config.present_mode = if options.vsync {
                    wgpu::PresentMode::AutoVsync
                } else {
                    wgpu::PresentMode::AutoNoVsync
                };
                config.desired_maximum_frame_latency = 2;
                surface.configure(&device, &config);
                (config.format, Some(config))
            }
            None => (
                options
                    .format
                    .unwrap_or(wgpu::TextureFormat::Rgba8UnormSrgb),
                None,
            ),
        };
        let is_software = adapter_info.device_type == wgpu::DeviceType::Cpu;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            surface,
            format,
            config,
            size: (width.max(1), height.max(1)),
            msaa_samples: options.msaa_samples.clamp(1, 8),
            adapter_info,
            is_software,
            needs_reconfigure: false,
        })
    }

    /// 离屏初始化（无窗口；用于无头测试、截图与 CI）。
    ///
    /// 同样会遍历候选后端（Web 上是 WebGPU → WebGL2），全部失败才报错。
    pub async fn headless(
        width: u32,
        height: u32,
        options: GpuOptions,
    ) -> Result<Self, GpuInitError> {
        Self::from_candidates(width, height, options, |_| Ok(None)).await
    }

    /// 创建离屏渲染目标（带 `COPY_SRC`，可回读为图片）。
    pub fn create_offscreen(&self, width: u32, height: u32) -> OffscreenTarget {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-offscreen"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        OffscreenTarget {
            texture,
            view,
            size: (width.max(1), height.max(1)),
            format: self.format,
        }
    }

    /// 调整渲染面尺寸（窗口 resize 时调用）。
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if self.size == (w, h) {
            return;
        }
        self.size = (w, h);
        if let (Some(surface), Some(config)) = (&self.surface, &mut self.config) {
            config.width = w;
            config.height = h;
            surface.configure(&self.device, config);
        }
    }

    /// 宽高比。
    #[inline]
    pub fn aspect(&self) -> f32 {
        self.size.0 as f32 / self.size.1.max(1) as f32
    }

    /// 尺寸。
    #[inline]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// 取当前帧的交换链纹理（仅带渲染面时可用）。
    pub fn acquire_frame(&mut self) -> Result<wgpu::SurfaceTexture, FrameError> {
        let surface = self
            .surface
            .as_ref()
            .expect("acquire_frame 需要渲染面；离屏模式请使用 create_offscreen");
        match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => Ok(t),
            // 仍然可用，只是配置不再最优：本帧照常渲染，并在下一帧前重配。
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.needs_reconfigure = true;
                Ok(t)
            }
            wgpu::CurrentSurfaceTexture::Timeout => Err(FrameError::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => Err(FrameError::Occluded),
            wgpu::CurrentSurfaceTexture::Outdated => Err(FrameError::Outdated),
            wgpu::CurrentSurfaceTexture::Lost => Err(FrameError::Lost),
            wgpu::CurrentSurfaceTexture::Validation => Err(FrameError::Validation),
        }
    }

    /// 交换链是否需要重新配置。
    pub fn take_reconfigure_flag(&mut self) -> bool {
        std::mem::take(&mut self.needs_reconfigure)
    }

    /// 强制重新配置渲染面（尺寸或格式变化后调用）。
    pub fn reconfigure(&self) {
        if let (Some(surface), Some(config)) = (&self.surface, &self.config) {
            surface.configure(&self.device, config);
        }
    }

    /// 是否使用软件光栅化（lavapipe / llvmpipe 等）。
    #[inline]
    pub fn is_software(&self) -> bool {
        self.is_software
    }

    /// 创建一个带初始数据的缓冲区。
    pub fn create_buffer_init(&self, desc: &wgpu::util::BufferInitDescriptor<'_>) -> wgpu::Buffer {
        use wgpu::util::DeviceExt;
        self.device.create_buffer_init(desc)
    }

    /// 创建着色器模块（WGSL）。
    pub fn create_shader(&self, label: &str, source: &str) -> wgpu::ShaderModule {
        self.device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
    }
}

/// 顶点缓冲布局（位置 / 法线 / UV / 切线）。
pub fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Float32x2,
        3 => Float32x4
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// 共享的渲染面句柄类型别名。
pub type SharedSurface = Arc<wgpu::Surface<'static>>;
