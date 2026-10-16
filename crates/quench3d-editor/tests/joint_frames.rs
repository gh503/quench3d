//! 编辑器与 CLI **共用同一份关节装配**的结构守卫（纯 CPU，无 GPU，CI 每条腿都会跑）。
//!
//! 为什么需要它：编辑器现有的往返测试都要 `GpuContext::headless`，没有显卡时直接 skip ——
//! 也就是说 CI 上**没有任何测试**盯着"编辑器的物理预览和 `quench3d simulate` 是不是同一套
//! 语义"。而这条差别是静默的：预览里门被扭过去、CLI 里没被扭（或者反过来），两边都不报错。
//!
//! 这里钉的是**结构性事实**（这份代码只有一份关节装配）：
//! * 编辑器的预览/播放走 `runner::build_world` —— 也就是 CLI 用的那份装配，
//!   于是 `joints[].frame_a` / `frame_b` / `axis_a` 这些字段的语义天然一致；
//! * 编辑器自己**不**构造 `Joint`、也不碰 `local_frame_*`（一旦有人在 `ui.rs` 里"顺手
//!   补一个预览用的固定关节"，这条测试就会红，提醒他改用共享的那份）。
//!
//! 它不证明"预览的画面正确"（那要真窗口，见 docs/production_readiness.md 的诚实边界），
//! 只证明"两边不会各写一套"。

/// 编辑器源码里与"关节装配"有关的文件。
const EDITOR_SOURCES: &[(&str, &str)] = &[
    ("src/scene_io.rs", include_str!("../src/scene_io.rs")),
    ("src/doc.rs", include_str!("../src/doc.rs")),
    ("src/ui.rs", include_str!("../src/ui.rs")),
    ("src/app.rs", include_str!("../src/app.rs")),
    ("src/gizmo.rs", include_str!("../src/gizmo.rs")),
];

#[test]
fn editor_preview_builds_joints_through_the_cli_assembly() {
    let scene_io = EDITOR_SOURCES
        .iter()
        .find(|(name, _)| *name == "src/scene_io.rs")
        .map(|(_, src)| *src)
        .expect("必须包含 scene_io.rs");
    assert!(
        scene_io.contains("runner::build_world"),
        "编辑器的预览/播放必须走 CLI 那份世界装配（`runner::build_world`）—— \
         自己另写一份关节装配，就是「预览和 CLI 分叉」的起点"
    );
    assert!(
        scene_io.contains("runner::joint_frame_quat") || !scene_io.contains("frame_a"),
        "如果编辑器将来要**显示**参考姿态，请用 `runner::joint_frame_quat` 换算（度→四元数），\
         不要在 scene_io.rs 里再抄一份欧拉角公式：顺序抄错时单轴场景看不出来"
    );
}

#[test]
fn editor_does_not_hand_roll_joint_constraints() {
    for (name, src) in EDITOR_SOURCES {
        for needle in [
            "Joint::Fixed",
            "Joint::Hinge",
            "local_frame_a",
            "local_frame_b",
        ] {
            assert!(
                !src.contains(needle),
                "{name} 里出现了 `{needle}`：关节约束只有一个来源（runner + 物理引擎），\
                 编辑器再加一处就会分叉"
            );
        }
        // 物理世界也只有一个来源：编辑器不自己 `PhysicsWorld::new()`。
        assert!(
            !src.contains("PhysicsWorld::new"),
            "{name} 自己建了物理世界：应当用 `runner::build_world` 的返回值"
        );
    }
}

#[test]
fn shared_joint_frame_mapping_uses_the_documented_units() {
    // 格式里的 `frame_a` / `frame_b` 是**度**（与 rotation_deg 一致）。
    // 这条钉住共享换算的语义：90 度 → 绕对应轴的四分之一圈（弧度 = π/2）。
    // 如果哪天有人把字段改成弧度（或抄错 57.2958 的方向），CLI 与编辑器的预览会一起错，
    // 这里是最早红的地方。
    use quench3d::prelude::Quat;
    use quench3d_cli::runner::joint_frame_quat;
    let half = core::f32::consts::FRAC_PI_2;
    assert_eq!(
        joint_frame_quat(Some([90.0, 0.0, 0.0])),
        Quat::from_rotation_x(half)
    );
    assert_eq!(
        joint_frame_quat(Some([0.0, 90.0, 0.0])),
        Quat::from_rotation_y(half)
    );
    assert_eq!(
        joint_frame_quat(Some([0.0, 0.0, 90.0])),
        Quat::from_rotation_z(half)
    );
    // 不写 = 单位旋转（= 老行为那一支）。
    assert_eq!(joint_frame_quat(None), Quat::IDENTITY);
    assert_eq!(joint_frame_quat(Some([0.0, 0.0, 0.0])), Quat::IDENTITY);
}
