//! robot-modkit M1 机械臂验收：3 铰链串链伺服、遥测、运行时 joint_target。
//!
//! 场景 fixture 的几何来自 `robot-modkit/sim/quench_scene.py`，但修掉了生成器里
//! 三处锚点与刚体中心不重合的问题（见 `docs/robot_arm_sim.md`）。上游原文件也原样
//! 收在 `modkit_arm.upstream.scene.json`，校验器会对它的锚点错位报警。

use quench3d_cli::runner::run_simulate;
use quench3d_cli::scene_doc::{ActionDoc, ActionKind, SceneDoc};

fn fixture() -> SceneDoc {
    let text = include_str!("fixtures/modkit_arm.scene.json");
    SceneDoc::from_json(text).expect("fixture 要能解析")
}

fn joint<'a>(
    report: &'a quench3d_cli::runner::SimReport,
    name: &str,
) -> &'a quench3d_cli::runner::JointReport {
    report
        .joints
        .iter()
        .find(|j| j.name == name)
        .unwrap_or_else(|| panic!("报告里没有关节 {name}"))
}

/// 绕局部 +Y 的世界方向与 +Y 的夹角（度）——偏航块倾角。
fn tilt_deg(report: &quench3d_cli::runner::SimReport, name: &str) -> f32 {
    let s = report.states.iter().find(|s| s.name == name).unwrap();
    let [x, y, z, w] = s.rotation;
    // q * (0,1,0) 的 y 分量。
    let up_y = 1.0 - 2.0 * (x * x + z * z);
    let _ = y;
    let _ = w;
    up_y.clamp(-1.0, 1.0).acos().to_degrees()
}

#[test]
fn serial_chain_servo_converges_in_five_seconds_across_gains_and_dt() {
    // 三档：低/中/高。每档都跑 1/500、1/120、1/60（后者用 32 子步 ≈ 1.9 kHz）。
    for (kp, kd_scale) in [(10.0, 0.85), (12.0, 1.0), (16.0, 1.3)] {
        for (dt, steps, substeps) in [
            (1.0 / 500.0, 2500u32, 8u32),
            (1.0 / 120.0, 600, 8),
            (1.0 / 60.0, 300, 32),
        ] {
            let mut doc = fixture();
            for j in &mut doc.joints {
                j.motor_kp = Some(kp);
                j.motor_kd = Some(
                    match j.name.as_deref() {
                        Some("yaw") => 0.08,
                        Some("shoulder") => 0.4,
                        Some("elbow") => 0.15,
                        _ => 0.2,
                    } * kd_scale,
                );
            }
            doc.simulate.dt = dt;
            doc.simulate.steps = steps;
            doc.physics.substeps = Some(substeps);
            let report = run_simulate(&doc).expect("仿真要能跑");
            let shoulder = joint(&report, "shoulder");
            let elbow = joint(&report, "elbow");
            let yaw = joint(&report, "yaw");
            assert!(
                (shoulder.angle_deg - 35.0).abs() <= 1.0,
                "kp={kp} dt={dt:.5} substeps={substeps}: 肩 {:.3}° 不在 35°±1°",
                shoulder.angle_deg
            );
            assert!(
                (elbow.angle_deg + 55.0).abs() <= 1.0,
                "kp={kp} dt={dt:.5} substeps={substeps}: 肘 {:.3}° 不在 -55°±1°",
                elbow.angle_deg
            );
            assert!(
                yaw.angle_deg.abs() <= 0.5,
                "kp={kp} dt={dt:.5}: 偏航 {:.3}° 超 0.5°",
                yaw.angle_deg
            );
            assert!(
                tilt_deg(&report, "yaw") <= 0.5,
                "kp={kp} dt={dt:.5}: 偏航块倾角 {:.3}° 超 0.5°",
                tilt_deg(&report, "yaw")
            );
            assert!(!shoulder.broken && !elbow.broken && !yaw.broken);
            assert_eq!(shoulder.limit_state, "ok");
            assert_eq!(elbow.limit_state, "ok");
        }
    }
}

#[test]
fn shoulder_holding_torque_matches_the_static_gravity_load() {
    // 解析静力矩：两节连杆质量 × 到肩的水平距离。
    // 肩 35°、肘 -55° → link1 与水平 35°，link2 与水平 -20°。
    let mut doc = fixture();
    doc.simulate.steps = 2500;
    let report = run_simulate(&doc).expect("仿真要能跑");
    let sh = joint(&report, "shoulder");
    let el = joint(&report, "elbow");
    let sh_r = sh.angle_deg.to_radians();
    let el_r = el.angle_deg.to_radians();
    // 关节几何：肩在 x=0.05；link1 质心 +0.06；link2 质心 +0.175。
    let torque = 9.81 * (0.09 * 0.06 * sh_r.cos() + 0.07 * 0.175 * (sh_r + el_r).cos());
    assert!(
        (sh.torque_nm.abs() - torque.abs()).abs() <= 0.15 * torque.abs(),
        "肩力矩 {:.4} N·m 与解析 {:.4} N·m 差超过 15%",
        sh.torque_nm,
        torque
    );
    // 肘只承受小臂自身重力：质量 0.07、质心 0.055。
    let elbow_torque = 9.81 * 0.07 * 0.055 * (sh_r + el_r).cos();
    assert!(
        (el.torque_nm.abs() - elbow_torque.abs()).abs() <= 0.15 * elbow_torque.abs(),
        "肘力矩 {:.4} N·m 与解析 {:.4} N·m 差超过 15%",
        el.torque_nm,
        elbow_torque
    );
}

#[test]
fn joint_target_action_changes_the_target_at_runtime() {
    let mut doc = fixture();
    doc.simulate.steps = 1500; // 3 s
    doc.actions.push(ActionDoc {
        kind: ActionKind::JointTarget,
        target: String::new(),
        joint: Some("shoulder".into()),
        target_deg: Some(0.0),
        target_m: None,
        speed_deg_s: None,
        speed_m_s: None,
        other: None,
        at_step: Some(500), // 1 s：肩从 35° 转到 0°
        at_time: None,
        value: None,
        point: None,
        position: None,
        rotation_deg: None,
        steps: 1,
    });
    let report = run_simulate(&doc).expect("仿真要能跑");
    let sh = joint(&report, "shoulder");
    assert_eq!(sh.target_deg, Some(0.0), "运行时目标要写回报告");
    assert!(
        sh.angle_deg.abs() <= 1.5,
        "改目标后 2 s 肩应回到 0°±1.5°，实际 {:.3}°",
        sh.angle_deg
    );
    assert!(!sh.broken, "2.5 N·m 限幅下不该触发 2.94 N·m 的断裂阈值");
}

fn grasp_fixture() -> SceneDoc {
    SceneDoc::from_json(include_str!("fixtures/modkit_arm_grasp.scene.json"))
        .expect("grasp fixture")
}

#[test]
fn grasp_script_lifts_the_cube_ten_centimetres_without_slip() {
    let doc = grasp_fixture();
    let report = run_simulate(&doc).expect("仿真要能跑");
    let pos = |n: &str| {
        report
            .states
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("没有 {n}"))
            .position
    };
    let cube0 = [0.2727631, 0.061, 0.0];
    let cube = pos("cube");
    let grip = pos("gripper");
    assert!(
        cube[1] - 0.02 >= 0.10,
        "方块只抬起了 {:.3} m（要求 ≥0.10 m，从桌面 y=0.02 起算）",
        cube[1] - 0.02
    );
    let slip =
        ((cube[0] - grip[0]).powi(2) + (cube[1] - grip[1]).powi(2) + (cube[2] - grip[2]).powi(2))
            .sqrt();
    assert!(slip <= 1.0e-3, "方块相对夹爪滑移 {slip:.4} m");
    let _ = cube0;
    // 两个手指都合拢（从 ±0.04 到约 ±0.028）。
    for name in ["finger_l_slide", "finger_r_slide"] {
        let j = report.joints.iter().find(|j| j.name == name).unwrap();
        assert!(j.displacement_m.unwrap() < 0.04);
    }
    assert!(report.joints.iter().all(|j| !j.broken));
}
