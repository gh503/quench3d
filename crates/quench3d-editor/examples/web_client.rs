//! Web 端入口：`tools/build_web.sh web_client` 之后打开 `web/index.html`。
//!
//! 页面与桌面端跑的是**同一套代码**（`quench3d_editor::run`），区别只有：
//! 页面在 `start()` 之前先探好后端（见 `web/index.html` 的 `pickBackend`），
//! 并且用 `on_scene_file` 把浏览器选中的场景文件交回 Rust。
//!
//! 另外还导出几个自检探针给 `tools/check_web.mjs`：它们是「Web 产物真的能用」
//! 的硬证据 —— `cargo check --target wasm32` 只能证明能编译。

/// 启动编辑器（页面在探好后端之后调用）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    quench3d_editor::run(quench3d_editor::EditorConfig::default())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))
}

/// 页面把用户选中的场景文件交回 Rust（由 `web/index.html` 调用）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn on_scene_file(name: String, text: String) {
    quench3d_editor::scene_io::on_scene_file(name, text);
}

/// 外部姿态注入入口：页面脚本（例如 MediaPipe Tasks Vision）把 2D 骨架交给编辑器。
///
/// JSON 形如 `{"present":true,"confidence":0.9,"landmarks":[{"x":0.5,"y":0.5},…]}`；
/// 归一化图像坐标，至少 2 个点。内置推理仍不在浏览器里。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_camera_pose(json: String) {
    quench3d_editor::camera::set_web_pose(&json);
}

/// 清空外部姿态。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clear_camera_pose() {
    quench3d_editor::camera::clear_web_pose();
}

/// 外部深度注入：把一帧 u8 相对深度（Uint8Array）按针孔内参反投影成点云。
///
/// 浏览器普通 RGB 摄像头没有深度；这个入口给 WebXR Depth Sensing / 服务端深度流
/// 之类的源用。参数与桌面 `latest.depth` 同一套约定：u8=255 → `z_near`，
/// u8=0 → `z_far`；`origin` 是反投影后加的引擎坐标（长度不足 3 时补 0）。
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_camera_depth(
    width: u32,
    height: u32,
    depth: Vec<u8>,
    fx: f32,
    fy: f32,
    cx: f32,
    cy: f32,
    z_near: f32,
    z_far: f32,
    step: u32,
    origin: Vec<f32>,
) -> Result<(), wasm_bindgen::JsValue> {
    let mut o = [0.0f32; 3];
    for (i, v) in origin.iter().take(3).enumerate() {
        o[i] = *v;
    }
    quench3d_editor::camera::set_web_depth(
        width, height, &depth, fx, fy, cx, cy, z_near, z_far, step, o,
    )
    .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
}

/// 开关 Web 深度时间累积（最近约 90 帧、2 cm 体素平均）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_camera_depth_accumulation(enabled: bool) {
    quench3d_editor::camera::set_web_accumulation(enabled);
}

/// 开关 Web 深度时间累积之上的 ICP 位姿跟踪。
///
/// 打开会自动打开时间累积；浏览器普通 RGB 摄像头没有深度，仍需先
/// `set_camera_depth` 注入深度帧。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_camera_depth_tracking(enabled: bool) {
    quench3d_editor::camera::set_web_tracking(enabled);
}

/// Web ICP 诊断：`开启,配准帧数,rms 米`。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn camera_depth_tracking_probe() -> String {
    let (enabled, frames, rms) = quench3d_editor::camera::web_tracking_status();
    format!("{},{frames},{rms}", u8::from(enabled))
}

/// 清空外部深度与重建表面。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clear_camera_depth() {
    quench3d_editor::camera::clear_web_depth();
}

/// 当前外部深度点/表面三角形计数（页面/自检探针）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn camera_depth_probe() -> String {
    let (points, triangles) = quench3d_editor::camera::web_depth_counts();
    format!("{points},{triangles}")
}

/// 用当前注入点云重建一次体素表面，返回 `点,三角形`（失败返回 `err:...`）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn camera_depth_surface_probe() -> String {
    match quench3d_editor::camera::rebuild_web_depth_surface(0.05) {
        Ok(_) => {
            let (points, triangles) = quench3d_editor::camera::web_depth_counts();
            format!("{points},{triangles}")
        }
        Err(e) => format!("err:{e}"),
    }
}

/// 时钟探针：读一次引擎时钟（秒）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clock_probe() -> f64 {
    quench3d::web_probe::clock_probe()
}

/// 路径探针：Web 端按路径加载模型必须给出能照着做的报错。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_path_probe() -> String {
    quench3d::web_probe::model_path_probe()
}

/// 字节探针：Web 端从字节加载模型必须真的能跑通。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_bytes_probe() -> String {
    quench3d::web_probe::model_bytes_probe()
}

/// 场景格式探针：wasm 里解析同一个 `quench3d.scene` 并建成物理世界（纯 CPU）。
///
/// 这是「Web 端用同一套场景格式」的硬证据（`cargo check` 只能证明能编译）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn scene_roundtrip_probe() -> String {
    quench3d_editor::scene_io::roundtrip_probe()
}

/// wasm 产物是 example，需要一个（Web 端不会调用的）`main`。
#[cfg(target_arch = "wasm32")]
fn main() {}

/// 桌面端也能直接跑这个 example（等价于 `cargo run -p quench3d-editor`）。
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d_editor::run(quench3d_editor::EditorConfig::default())
}

// ---------------------------------------------------------------------------
// 无窗口机械臂 API（P1-3）
// ---------------------------------------------------------------------------
//
// 浏览器里除了完整编辑器，还需要一个**不启动 winit/GPU** 的入口：页面自己画
// 模型（Three.js/Babylon/自绘），只把 quench3d 当关节/物理求解器。下面这几个
// 导出就是那条最小接口；`loadScene` 是配套的装载入口（没有场景就没有关节）。

/// wasm 里保存的物理世界 + 最近一次 step 的 dt。
#[cfg(target_arch = "wasm32")]
struct WebRobot {
    build: quench3d_cli::runner::WorldBuild,
    dt: f32,
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_ROBOT: std::cell::RefCell<Option<WebRobot>> = const { std::cell::RefCell::new(None) };
}

/// 装载一个 `quench3d.scene` 文本（不建 GPU、不开窗口）。
///
/// 页面随后用 [`setJointTarget`] / [`step`] / [`getJointStates`] / [`getBodyTransform`]。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = loadScene)]
pub fn load_scene(text: &str) -> Result<(), wasm_bindgen::JsValue> {
    let doc = quench3d_cli::scene_doc::SceneDoc::from_json(text)
        .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("场景解析失败: {e}")))?;
    let build = quench3d_cli::runner::build_world(&doc)
        .map_err(|e| wasm_bindgen::JsValue::from_str(&format!("建世界失败: {e}")))?;
    let dt = doc.simulate.dt;
    WEB_ROBOT.with(|slot| *slot.borrow_mut() = Some(WebRobot { build, dt }));
    Ok(())
}

/// 设置一个铰链关节的目标角（**度**）。找不到/不是铰链返回 `false`。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = setJointTarget)]
pub fn set_joint_target(joint: &str, target_deg: f64) -> bool {
    if !target_deg.is_finite() {
        return false;
    }
    WEB_ROBOT.with(|slot| {
        let mut guard = slot.borrow_mut();
        let Some(robot) = guard.as_mut() else {
            return false;
        };
        let Some(handle) = robot
            .build
            .joints
            .iter()
            .find(|j| j.name == joint)
            .map(|j| j.handle)
        else {
            return false;
        };
        robot
            .build
            .world
            .set_joint_target(handle, (target_deg as f32).to_radians())
    })
}

/// 推进一个外层步（秒）。内部按场景的 `physics.substeps` 再切子步。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = step)]
pub fn step(dt: f64) {
    if !dt.is_finite() || dt <= 0.0 {
        return;
    }
    WEB_ROBOT.with(|slot| {
        let mut guard = slot.borrow_mut();
        if let Some(robot) = guard.as_mut() {
            robot.dt = dt as f32;
            robot.build.step(1, robot.dt);
        }
    });
}

/// 所有关节的当前状态，JSON 数组字符串。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = getJointStates)]
pub fn get_joint_states() -> String {
    WEB_ROBOT.with(|slot| {
        let guard = slot.borrow();
        let Some(robot) = guard.as_ref() else {
            return "[]".to_string();
        };
        let sub_dt = robot.dt / robot.build.substeps.max(1) as f32;
        let broken = robot.build.world.broken_joints();
        let states: Vec<serde_json::Value> = robot
            .build
            .joints
            .iter()
            .map(|j| {
                let is_broken = broken.contains(&j.handle);
                match robot.build.world.joint_telemetry(j.handle, sub_dt) {
                    Some(t) if !is_broken => {
                        let error = t.target.map(|target| (target - t.coordinate).to_degrees());
                        serde_json::json!({
                            "name": j.name,
                            "kind": t.kind,
                            "angle_deg": t.coordinate.to_degrees(),
                            "target_deg": t.target.map(f32::to_degrees),
                            "error_deg": error,
                            "torque_nm": t.motor_torque,
                            "broken": false,
                        })
                    }
                    _ => serde_json::json!({
                        "name": j.name,
                        "kind": j.kind,
                        "angle_deg": 0.0,
                        "target_deg": j.target_deg,
                        "error_deg": null,
                        "torque_nm": 0.0,
                        "broken": is_broken,
                    }),
                }
            })
            .collect();
        serde_json::to_string(&states).unwrap_or_else(|_| "[]".to_string())
    })
}

/// 一个刚体的世界矩阵（列主序 16 个 f32，可直接喂 WebGL/Three.js）。
///
/// 名字不存在（或没有装场景）返回空数组，不抛异常 —— 页面每帧轮询时更好处理。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = getBodyTransform)]
pub fn get_body_transform(name: &str) -> Vec<f32> {
    WEB_ROBOT.with(|slot| {
        let guard = slot.borrow();
        let Some(robot) = guard.as_ref() else {
            return Vec::new();
        };
        let Some(handle) = robot.build.handles.get(name).copied() else {
            return Vec::new();
        };
        let Some(body) = robot.build.world.body(handle) else {
            return Vec::new();
        };
        body.transform().matrix().to_cols_array().to_vec()
    })
}
