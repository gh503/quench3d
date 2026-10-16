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
//!
//! 其中**两条要真的创建 wgpu 上下文**（`empty_candidate_list_does_not_panic_and_uses_defaults`
//! 与 `unusable_first_candidate_falls_through_to_the_next`），它们是**显式 opt-in** 的：
//! 只有设了 `QUENCH3D_GPU_TESTS=1` 才执行，否则整条测试体不跑并打印跳过原因。理由见
//! [`with_gpu_tests`]：在没有 GPU 的 Windows runner 上创建上下文会**原生崩溃**
//! （`STATUS_ACCESS_VIOLATION` / `0xc0000005`），进程直接死，`Result::Err` 接不住。
//! 其余各条（含下面那条不需要显卡的守卫）是纯 CPU 的，任何机器、任何开关状态下都会跑。

use quench3d_render::context::with_gpu_tests;
use quench3d_render::prelude::*;

/// 编译进来的后端集合（`wgpu::Instance::enabled_backend_features`）。
fn compiled() -> wgpu::Backends {
    wgpu::Instance::enabled_backend_features()
}

/// 守卫（**不需要显卡**）：证明"没显式打开开关时，一个 wgpu 上下文都不建"。
///
/// 这条测试刻意**不读写进程级环境变量**：`cargo test` 的测试跑在同一进程的多个线程里，
/// 在这里改 `QUENCH3D_GPU_TESTS` 会污染同进程的其它测试 —— 在无 GPU 的 Windows 上，
/// 那等于把原生崩溃放出来。所以判定内核 `run_if_gpu_tests_enabled` 把"开/关"做成参数，
/// 守卫直接喂 flag；这仍然钉住真实路径，因为 `with_gpu_tests` 只是"取一次环境变量 → 转交"。
#[test]
fn gpu_gate_builds_nothing_unless_explicitly_enabled() {
    use quench3d_render::context::{
        gpu_tests_enabled, gpu_tests_enabled_from, run_if_gpu_tests_enabled, GPU_TESTS_ENV,
    };

    // ① 开关的解析：只有字面量 "1" 算打开。
    assert!(!gpu_tests_enabled_from(None), "没设变量必须是关");
    assert!(!gpu_tests_enabled_from(Some("")), "空值必须是关");
    assert!(!gpu_tests_enabled_from(Some("0")), "\"0\" 必须是关");
    assert!(
        !gpu_tests_enabled_from(Some("true")),
        "只认 \"1\"：\"true\" 不算打开"
    );
    assert!(gpu_tests_enabled_from(Some("1")), "\"1\" 必须打开");

    // ② 真实读取路径必须读的就是这个变量名（防"读了另一个变量"）。
    assert_eq!(
        gpu_tests_enabled(),
        gpu_tests_enabled_from(std::env::var(GPU_TESTS_ENV).ok().as_deref()),
        "gpu_tests_enabled 必须读 {GPU_TESTS_ENV}"
    );

    // ③ 关着的时候：闭包（这里就是"建 wgpu 上下文"的替身）**一次都不执行**。
    let mut built = 0;
    let off = run_if_gpu_tests_enabled(false, "守卫：关", || built += 1);
    assert!(off.is_none(), "关着的时候必须返回 None（= 跳过）");
    assert_eq!(built, 0, "关着的时候不许建上下文");

    // ④ 打开的时候：闭包执行且恰好一次，结果透传。
    let on = run_if_gpu_tests_enabled(true, "守卫：开", || {
        built += 1;
        "ctx"
    });
    assert_eq!(on, Some("ctx"), "打开时必须真的执行并把结果透传");
    assert_eq!(built, 1, "打开时应当恰好建一次");
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
    //
    // 整条测试体（含创建上下文那一步）都放进 opt-in 闭包：没设 `QUENCH3D_GPU_TESTS=1`
    // 时它**一次都不执行**。"建上下文失败再 return"是接不住原生崩溃的。
    let _ = with_gpu_tests(
        "空候选列表要退回默认候选（真的创建 wgpu 上下文）",
        || {
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
        },
    );
}

#[test]
fn unusable_first_candidate_falls_through_to_the_next() {
    // 核心回归：第一个候选**注定拿不到适配器**（空 `Backends`，一个后端都不请求），
    // 引擎必须继续试下一个并成功。这条测试就是"回退真的会发生"的证据。
    let _ = with_gpu_tests(
        "第一个候选不可用时回退到下一个（真的创建 wgpu 上下文）",
        || {
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
        },
    );
}
