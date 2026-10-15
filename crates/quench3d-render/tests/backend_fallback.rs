//! 渲染后端**回退**的验证：候选列表怎么排、排错了会怎样。
//!
//! 背景（浏览器里真实发生过的故障）：
//!   1. wgpu 只要发现请求里带 `BROWSER_WEBGPU` 且 `navigator.gpu` 存在，就直接走纯
//!      WebGPU 上下文，**连 WebGL2 实例都不创建**——而"navigator.gpu 存在"不等于
//!      "有适配器"（Linux Chrome 没开 Vulkan 就是 `requestAdapter()` 返回 null），
//!      于是报 `No available adapters`，说好的 WebGL2 回退永远轮不到；
//!   2. 更隐蔽的是：一旦对着画布要过 `webgpu` 上下文，同一张画布的
//!      `getContext("webgl2")` 会返回 null（一张画布只能有一种上下文，且不可逆）。
//!      所以"先试 WebGPU、失败再换"这条路在同一张画布上是**走不通**的，
//!      必须先把后端探清楚（页面 `navigator.gpu.requestAdapter()`，不碰画布）再排候选。
//!
//! 这里锁住的是引擎侧的契约：候选解析 + "拿不到就换下一个"真的会换。

use quench3d_render::prelude::*;

/// 编译进来的后端集合（`wgpu::Instance::enabled_backend_features`）。
fn compiled() -> wgpu::Backends {
    wgpu::Instance::enabled_backend_features()
}

#[test]
fn hint_gl_means_gl_only() {
    // 页面探到"没有 WebGPU"时必须**只**给 GL：多带一个 WebGPU 候选会让 wgpu
    // 去要画布的 webgpu 上下文，把回退的路堵死。
    let c = GpuContext::candidates_from_hint(Some("gl"));
    if compiled().contains(wgpu::Backends::GL) {
        assert_eq!(c, vec![wgpu::Backends::GL], "提示 gl 时应当只有 GL");
    } else {
        assert!(!c.is_empty(), "没编进 GL 时至少要给个默认候选");
    }
}

#[test]
fn hint_webgpu_prefers_webgpu_but_keeps_gl_as_backstop() {
    let c = GpuContext::candidates_from_hint(Some("webgpu"));
    assert!(!c.is_empty());
    if compiled().contains(wgpu::Backends::BROWSER_WEBGPU) {
        assert_eq!(c[0], wgpu::Backends::BROWSER_WEBGPU, "WebGPU 要排第一");
    }
    if compiled().contains(wgpu::Backends::GL) {
        assert!(c.contains(&wgpu::Backends::GL), "WebGL2 要留着兜底");
    }
}

#[test]
fn unknown_or_missing_hint_falls_back_to_default_candidates() {
    assert_eq!(
        GpuContext::candidates_from_hint(None),
        GpuContext::backend_candidates()
    );
    assert_eq!(
        GpuContext::candidates_from_hint(Some("这是一个不存在的后端")),
        GpuContext::backend_candidates()
    );
}

#[test]
fn hinted_candidates_are_compiled_in() {
    // 由提示解析出来的候选必须是"这份构建真的有"的后端，否则只会白跑一轮、
    // 还把错误信息搅浑。（默认候选不适用这条：桌面端故意交 `Backends::all()`
    // 给 wgpu 自己挑，里面本就有该平台用不到的后端。）
    for hint in ["gl", "webgpu", "webgl2", "wgpu"] {
        let c = GpuContext::candidates_from_hint(Some(hint));
        // 这台机器一个候选都编不出来时（macOS 就没有 GL），提示会被过滤空、
        // 退回**默认候选**——而默认候选在桌面端故意是 `Backends::all()`，
        // 交给 wgpu 自己挑，里面本就带该平台用不到的后端。那条豁免。
        if c == GpuContext::backend_candidates() {
            continue;
        }
        for b in c {
            assert!(
                compiled().contains(b),
                "候选 {b:?} 没编进这份构建（hint={hint}）"
            );
        }
    }
}

#[test]
fn empty_candidate_list_does_not_panic_and_uses_defaults() {
    // 门面层如果传了个空列表，引擎要退回默认候选，而不是报"没有后端可试"。
    let ctx = match pollster::block_on(GpuContext::from_candidates_with(
        &[],
        32,
        32,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
        |_| Ok(None),
    )) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    assert_eq!(ctx.size(), (32, 32));
}

#[test]
fn unusable_first_candidate_falls_through_to_the_next() {
    // 核心回归：第一个候选**注定拿不到适配器**（空 `Backends`，一个后端都不请求），
    // 引擎必须继续试下一个并成功。这条测试就是"回退真的会发生"的证据。
    let all = compiled() & wgpu::Backends::all();
    if all.is_empty() {
        eprintln!("跳过：这份构建没有任何后端");
        return;
    }
    let candidates = [wgpu::Backends::empty(), all];
    let ctx = match pollster::block_on(GpuContext::from_candidates_with(
        &candidates,
        48,
        32,
        GpuOptions {
            msaa_samples: 1,
            ..Default::default()
        },
        |_| Ok(None),
    )) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return;
        }
    };
    assert_eq!(ctx.size(), (48, 32));
    // 拿到的必须是**第二个**候选里的后端（第一个是空的）。
    assert!(
        all.contains(ctx.adapter_info.backend.into()),
        "适配器后端 {:?} 不在第二个候选里",
        ctx.adapter_info.backend
    );
}
