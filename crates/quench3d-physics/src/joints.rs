//! 关节（约束）定义与求解。
//!
//! 支持 5 种关节：
//! * `Fixed`：点约束 + 全 3 自由度角度约束（完全刚性连接）；
//! * `Ball`：只锁 3 个平移自由度（自由转动）；
//! * `Hinge`：点约束 + 2 自由度角度约束（两轴保持平行），可带角度限位与马达；
//! * `Distance`：沿两锚点连线的 1 自由度约束，`stiffness = 0` 时为刚性杆；
//! * `Slider`：锁垂直于滑轴的两个平移自由度 + 2 自由度角度约束，可带轴向限位。
//!
//! 平移约束的位置误差用**伪速度**（分裂冲量）修正，不会给系统注入能量；
//! 角度约束用很小的 Baumgarte 偏置在速度层修正（角度约束是刚性的，不会像接触
//! 那样因为偏置而「弹跳」）。

use quench3d_math::*;

use crate::body::BodyHandle;
use crate::solver::{apply_impulse, apply_pseudo_impulse, BodyView};
use crate::world::JointSlot;

/// 铰链马达：以给定最大冲量驱动相对角速度趋向目标。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HingeMotor {
    /// 目标相对角速度（rad/s）。
    pub target_velocity: f32,
    /// 单步最大冲量。
    pub max_impulse: f32,
}

/// 关节类型。
///
/// 约定：`local_anchor_*` 是相对各自刚体原点的**本地点**，创建时两端锚点在
/// 世界坐标下应重合（否则会在第一步被强行拉到一起）。
#[derive(Clone, Debug)]
pub enum Joint {
    /// 固定关节：两刚体完全刚性连接（点约束 + 3 自由度角速度约束）。
    ///
    /// 注意：`local_frame_a` / `local_frame_b` 仅作为参考姿态保存，当前实现
    /// 的角约束是**速度级**的（锁定创建时的相对姿态），不会把两刚体强行
    /// 扭转对齐到参考姿态；需要指定目标相对姿态时应自行设置初始朝向。
    Fixed {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
        /// A 的本地参考姿态。
        local_frame_a: Quat,
        /// B 的本地参考姿态。
        local_frame_b: Quat,
    },
    /// 球窝关节：只约束两锚点重合。
    Ball {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
    },
    /// 铰链关节。
    Hinge {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
        /// A 的本地转轴。
        local_axis_a: Vec3,
        /// B 的本地转轴。
        local_axis_b: Vec3,
        /// 角度限位（弧度：下界, 上界）。
        limits: Option<(f32, f32)>,
        /// 马达。
        motor: Option<HingeMotor>,
    },
    /// 距离关节（`stiffness = 0` 即刚性杆）。
    Distance {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
        /// 静止长度。
        rest_length: f32,
        /// 弹簧刚度（0 = 刚性）。
        stiffness: f32,
        /// 阻尼系数。
        damping: f32,
    },
    /// 滑动关节。
    Slider {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
        /// A 的本地滑轴。
        local_axis_a: Vec3,
        /// B 的本地滑轴。
        local_axis_b: Vec3,
        /// 轴向限位（下界, 上界）。
        limits: Option<(f32, f32)>,
    },
}

impl Joint {
    /// 关节两端的刚体句柄。
    pub fn bodies(&self) -> (BodyHandle, BodyHandle) {
        match self {
            Joint::Fixed { a, b, .. }
            | Joint::Ball { a, b, .. }
            | Joint::Hinge { a, b, .. }
            | Joint::Distance { a, b, .. }
            | Joint::Slider { a, b, .. } => (*a, *b),
        }
    }

    /// A 端本地锚点。
    pub fn local_anchor_a(&self) -> Vec3 {
        match self {
            Joint::Fixed { local_anchor_a, .. }
            | Joint::Ball { local_anchor_a, .. }
            | Joint::Hinge { local_anchor_a, .. }
            | Joint::Distance { local_anchor_a, .. }
            | Joint::Slider { local_anchor_a, .. } => *local_anchor_a,
        }
    }

    /// B 端本地锚点。
    pub fn local_anchor_b(&self) -> Vec3 {
        match self {
            Joint::Fixed { local_anchor_b, .. }
            | Joint::Ball { local_anchor_b, .. }
            | Joint::Hinge { local_anchor_b, .. }
            | Joint::Distance { local_anchor_b, .. }
            | Joint::Slider { local_anchor_b, .. } => *local_anchor_b,
        }
    }
}

/// 关节句柄（带代数，删除后旧句柄失效）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct JointHandle {
    /// 槽位下标。
    pub index: u32,
    /// 代数。
    pub generation: u32,
}

/// 跨步保留的关节累积冲量（热启动用）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JointImpulses {
    /// 点约束冲量。
    pub point: Vec3,
    /// 3 自由度角度冲量。
    pub ang3: Vec3,
    /// 2 自由度（角度或平移）冲量。
    pub uv: [f32; 2],
    /// 滑块的角度 2 自由度冲量。
    pub uv_ang: [f32; 2],
    /// 标量（距离）冲量。
    pub scalar: f32,
    /// 限位冲量。
    pub limit: f32,
    /// 马达冲量。
    pub motor: f32,
}

/// 关节在求解器里的具体形态。
#[derive(Clone, Copy, Debug)]
pub(crate) enum JointKind {
    /// 球窝。
    Ball,
    /// 固定。
    Fixed,
    /// 铰链。
    Hinge {
        /// A 的世界轴。
        axis_a: Vec3,
        /// B 的世界轴。
        axis_b: Vec3,
    },
    /// 距离。
    Distance,
    /// 滑块。
    Slider {
        /// A 的世界轴。
        axis_a: Vec3,
        /// B 的世界轴。
        axis_b: Vec3,
    },
}

/// 求解器内部的关节状态。
#[derive(Clone, Debug)]
pub(crate) struct JointState {
    /// A 下标。
    pub a: usize,
    /// B 下标。
    pub b: usize,
    /// A 本地锚点。
    pub local_a: Vec3,
    /// B 本地锚点。
    pub local_b: Vec3,
    /// A 世界力臂。
    pub ra: Vec3,
    /// B 世界力臂。
    pub rb: Vec3,
    /// 形态。
    pub kind: JointKind,
    /// 点约束有效质量。
    pub point: Option<Mat3>,
    /// 点约束累积冲量。
    pub point_impulse: Vec3,
    /// 点约束伪冲量。
    pub point_pseudo: Vec3,
    /// 3 自由度角度有效质量。
    pub ang3: Option<Mat3>,
    /// 3 自由度角度累积冲量。
    pub ang3_impulse: Vec3,
    /// 2 自由度有效质量（u/v 基：铰链角度 或 滑块平移）。
    pub uv2: Option<[[f32; 2]; 2]>,
    /// 2 自由度累积冲量。
    pub uv_impulse: [f32; 2],
    /// 滑块的角度 2 自由度有效质量。
    pub uv_ang: Option<[[f32; 2]; 2]>,
    /// 滑块的角度 2 自由度累积冲量。
    pub uv_ang_impulse: [f32; 2],
    /// 参考轴。
    pub axis: Vec3,
    /// 垂直基 u。
    pub u: Vec3,
    /// 垂直基 v。
    pub v: Vec3,
    /// 标量有效质量倒数。
    pub scalar_k: f32,
    /// 标量方向。
    pub scalar_dir: Vec3,
    /// 标量偏置。
    pub scalar_bias: f32,
    /// 标量累积冲量。
    pub scalar_impulse: f32,
    /// 刚性标量约束的目标长度（距离关节）。
    pub rest_length: f32,
    /// 标量约束是否刚性（`stiffness <= 0`），刚性的用伪速度严格修正位置。
    pub scalar_rigid: bool,
    /// 是否启用标量约束。
    pub use_scalar: bool,
    /// 限位。
    pub limits: Option<(f32, f32)>,
    /// 当前坐标（铰链角度 / 滑块位移）。
    pub coord: f32,
    /// 限位有效质量倒数。
    pub k_limit: f32,
    /// 限位累积冲量。
    pub limit_impulse: f32,
    /// 马达。
    pub motor: Option<HingeMotor>,
    /// 马达有效质量倒数。
    pub k_motor: f32,
    /// 马达累积冲量。
    pub motor_impulse: f32,
}

/// 关节求解的输入项。
pub(crate) struct JointEntry {
    /// A 下标。
    pub a: usize,
    /// B 下标。
    pub b: usize,
    /// 关节在关节表里的下标（**不持有引用**，这样条目缓冲可以跨步复用，不必每步分配）。
    pub joint_index: usize,
    /// 上一步的累积冲量。
    pub impulses: JointImpulses,
}

/// 关节求解器。
#[derive(Clone, Debug, Default)]
pub(crate) struct JointSolver {
    /// 所有关节状态。
    pub states: Vec<JointState>,
}

/// 2x2 线性方程组求解（奇异返回 `None`）。
fn solve2(k: [[f32; 2]; 2], b: [f32; 2]) -> Option<[f32; 2]> {
    let det = k[0][0] * k[1][1] - k[0][1] * k[1][0];
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        (k[1][1] * b[0] - k[0][1] * b[1]) * inv,
        (k[0][0] * b[1] - k[1][0] * b[0]) * inv,
    ])
}

/// 角度约束有效质量 `Ia⁻¹ + Ib⁻¹` 作用于 `c`。
fn ang_vec(va: &BodyView, vb: &BodyView, c: Vec3) -> Vec3 {
    va.inv_i_world(c) + vb.inv_i_world(c)
}

/// 点约束矩阵的一列：`(1/mA + 1/mB)·c + (IA⁻¹(rA×c))×rA + (IB⁻¹(rB×c))×rB`。
fn point_col(va: &BodyView, vb: &BodyView, ra: Vec3, rb: Vec3, c: Vec3) -> Vec3 {
    c * (va.inv_m + vb.inv_m)
        + va.inv_i_world(ra.cross(c)).cross(ra)
        + vb.inv_i_world(rb.cross(c)).cross(rb)
}

/// 组装 3x3 点约束有效质量矩阵。
fn point_mass(va: &BodyView, vb: &BodyView, ra: Vec3, rb: Vec3) -> Mat3 {
    Mat3::from_cols(
        point_col(va, vb, ra, rb, Vec3::X),
        point_col(va, vb, ra, rb, Vec3::Y),
        point_col(va, vb, ra, rb, Vec3::Z),
    )
}

/// 矩阵是否可用于求逆（有限且非零）。
fn usable(m: &Mat3) -> bool {
    m.cols[0].is_finite() && m.cols[1].is_finite() && m.cols[2].is_finite()
}

impl JointSolver {
    /// 准备阶段：计算力臂、有效质量与热启动冲量。
    pub fn prepare(&mut self, views: &[BodyView], joints: &[JointSlot], entries: &[JointEntry]) {
        self.states.clear();
        for e in entries {
            let joint = match joints.get(e.joint_index).and_then(|s| s.joint.as_ref()) {
                Some(j) => j,
                None => continue,
            };
            let va = views[e.a];
            let vb = views[e.b];
            let local_a = joint.local_anchor_a();
            let local_b = joint.local_anchor_b();
            let ra = va.rot.mul_vec3(local_a);
            let rb = vb.rot.mul_vec3(local_b);
            let mut st = JointState {
                a: e.a,
                b: e.b,
                local_a,
                local_b,
                ra,
                rb,
                kind: JointKind::Ball,
                point: None,
                point_impulse: e.impulses.point,
                point_pseudo: Vec3::ZERO,
                ang3: None,
                ang3_impulse: e.impulses.ang3,
                uv2: None,
                uv_impulse: e.impulses.uv,
                uv_ang: None,
                uv_ang_impulse: e.impulses.uv_ang,
                axis: Vec3::Y,
                u: Vec3::X,
                v: Vec3::Z,
                scalar_k: 0.0,
                scalar_dir: Vec3::Y,
                scalar_bias: 0.0,
                scalar_impulse: e.impulses.scalar,
                rest_length: 0.0,
                scalar_rigid: false,
                use_scalar: false,
                limits: None,
                coord: 0.0,
                k_limit: 0.0,
                limit_impulse: e.impulses.limit,
                motor: None,
                k_motor: 0.0,
                motor_impulse: e.impulses.motor,
            };

            // 点约束（距离关节与滑块关节不使用：前者只有轴向标量约束，
            // 后者只锁垂直于滑轴的平移自由度）。
            let wants_point = !matches!(joint, Joint::Distance { .. } | Joint::Slider { .. });
            let m = point_mass(&va, &vb, ra, rb);
            if wants_point && usable(&m) && m.determinant().abs() > 1e-14 {
                st.point = Some(m);
            }

            match joint {
                Joint::Ball { .. } => {}
                Joint::Fixed { .. } => {
                    st.kind = JointKind::Fixed;
                    let k = Mat3::from_cols(
                        ang_vec(&va, &vb, Vec3::X),
                        ang_vec(&va, &vb, Vec3::Y),
                        ang_vec(&va, &vb, Vec3::Z),
                    );
                    if usable(&k) && k.determinant().abs() > 1e-14 {
                        st.ang3 = Some(k);
                    }
                }
                Joint::Hinge {
                    local_axis_a,
                    local_axis_b,
                    limits,
                    motor,
                    ..
                } => {
                    let axis_a = va.rot.mul_vec3(*local_axis_a).normalize_or_zero();
                    let axis_b = vb.rot.mul_vec3(*local_axis_b).normalize_or_zero();
                    let axis = if (axis_a + axis_b).length_squared() > 1e-8 {
                        (axis_a + axis_b).normalize_or_zero()
                    } else {
                        axis_a
                    };
                    let (u, v) = axis.any_orthonormal_pair();
                    let k2 = |ci: Vec3, cj: Vec3| ci.dot(ang_vec(&va, &vb, cj));
                    let k = [[k2(u, u), k2(u, v)], [k2(v, u), k2(v, v)]];
                    if k[0][0] > 0.0 || k[1][1] > 0.0 {
                        st.uv2 = Some(k);
                    }
                    st.kind = JointKind::Hinge { axis_a, axis_b };
                    st.axis = axis;
                    st.u = u;
                    st.v = v;
                    st.limits = *limits;
                    st.motor = *motor;
                    st.coord = hinge_angle(*local_axis_a, *local_axis_b, &va, &vb, axis);
                    let k_axis = axis.dot(ang_vec(&va, &vb, axis));
                    if k_axis > 1e-9 {
                        st.k_limit = 1.0 / k_axis;
                        st.k_motor = st.k_limit;
                    }
                }
                Joint::Distance {
                    rest_length,
                    stiffness,
                    damping,
                    ..
                } => {
                    st.kind = JointKind::Distance;
                    st.use_scalar = true;
                    st.point = None;
                    let wa = va.pos + ra;
                    let wb = vb.pos + rb;
                    let d = wb - wa;
                    let len = d.length();
                    let n = if len > 1e-6 { d / len } else { Vec3::Y };
                    st.scalar_dir = n;
                    st.scalar_k = va.k_axis(ra, n) + vb.k_axis(rb, n);
                    st.rest_length = *rest_length;
                    // `stiffness <= 0` 视为刚性杆：位置误差交给伪速度严格修正，
                    // 弹簧模式则把误差转成速度偏置（软约束）。
                    st.scalar_rigid = *stiffness <= 0.0;
                    let bias = if st.scalar_rigid {
                        0.0
                    } else {
                        stiffness * (len - rest_length) / (1.0 + damping.max(0.0))
                    };
                    st.scalar_bias = clamp(bias, -20.0, 20.0);
                }
                Joint::Slider {
                    local_axis_a,
                    local_axis_b,
                    limits,
                    ..
                } => {
                    let axis_a = va.rot.mul_vec3(*local_axis_a).normalize_or_zero();
                    let axis_b = vb.rot.mul_vec3(*local_axis_b).normalize_or_zero();
                    let axis = if (axis_a + axis_b).length_squared() > 1e-8 {
                        (axis_a + axis_b).normalize_or_zero()
                    } else {
                        axis_a
                    };
                    let (u, v) = axis.any_orthonormal_pair();
                    // 平移：只锁垂直于轴的两个方向。这里约束的是**质心**相对速度；
                    // 配合角约束后，滑轴上任意点的垂直相对速度同样为零，且与旋转
                    // 解耦，因此一次迭代即可收敛。
                    let kt = |ci: Vec3, cj: Vec3| ci.dot(cj) * (va.inv_m + vb.inv_m);
                    let k = [[kt(u, u), kt(u, v)], [kt(v, u), kt(v, v)]];
                    if k[0][0] > 0.0 || k[1][1] > 0.0 {
                        st.uv2 = Some(k);
                    }
                    // 角向：两轴保持平行。
                    let ka = |ci: Vec3, cj: Vec3| ci.dot(ang_vec(&va, &vb, cj));
                    let ka2 = [[ka(u, u), ka(u, v)], [ka(v, u), ka(v, v)]];
                    if ka2[0][0] > 0.0 || ka2[1][1] > 0.0 {
                        st.uv_ang = Some(ka2);
                    }
                    st.kind = JointKind::Slider { axis_a, axis_b };
                    st.axis = axis;
                    st.u = u;
                    st.v = v;
                    st.limits = *limits;
                    let wa = va.pos + ra;
                    let wb = vb.pos + rb;
                    st.coord = (wb - wa).dot(axis);
                }
            }
            self.states.push(st);
        }
    }

    /// 热启动。
    pub fn warm_start(&self, views: &mut [BodyView]) {
        for st in &self.states {
            if st.point_impulse.length_squared() > 0.0 {
                apply_impulse(views, st.a, st.b, st.ra, st.rb, st.point_impulse);
            }
            if st.ang3_impulse.length_squared() > 0.0 {
                angular_impulse(views, st.a, st.b, st.ang3_impulse);
            }
            if st.uv_impulse[0] != 0.0 || st.uv_impulse[1] != 0.0 {
                let l = st.u * st.uv_impulse[0] + st.v * st.uv_impulse[1];
                match st.kind {
                    JointKind::Hinge { .. } => angular_impulse(views, st.a, st.b, l),
                    // 滑块平移冲量作用在质心（无附加力矩）。
                    _ => apply_impulse(views, st.a, st.b, Vec3::ZERO, Vec3::ZERO, l),
                }
            }
            if st.use_scalar && st.scalar_impulse != 0.0 {
                apply_impulse(
                    views,
                    st.a,
                    st.b,
                    st.ra,
                    st.rb,
                    st.scalar_dir * st.scalar_impulse,
                );
            }
            if st.uv_ang_impulse[0] != 0.0 || st.uv_ang_impulse[1] != 0.0 {
                let l = st.u * st.uv_ang_impulse[0] + st.v * st.uv_ang_impulse[1];
                angular_impulse(views, st.a, st.b, l);
            }
            if st.limit_impulse != 0.0 {
                match st.kind {
                    JointKind::Slider { .. } => {
                        apply_impulse(views, st.a, st.b, st.ra, st.rb, st.axis * st.limit_impulse)
                    }
                    _ => angular_impulse(views, st.a, st.b, st.axis * st.limit_impulse),
                }
            }
            if st.motor_impulse != 0.0 {
                angular_impulse(views, st.a, st.b, st.axis * st.motor_impulse);
            }
        }
    }

    /// 一轮速度迭代。
    pub fn solve_velocity(&mut self, views: &mut [BodyView], dt: f32) {
        for st in self.states.iter_mut() {
            // --- 标量（距离）约束 ---
            if st.use_scalar && st.scalar_k > 1e-12 {
                let va = views[st.a];
                let vb = views[st.b];
                let wa = va.pos + st.ra;
                let wb = vb.pos + st.rb;
                let d = wb - wa;
                let len = d.length();
                if len > 1e-6 {
                    st.scalar_dir = d / len;
                }
                // 每步重算一次偏置（几何在变）。
                let vrel = vb.velocity_at(st.rb) - va.velocity_at(st.ra);
                let vn = vrel.dot(st.scalar_dir);
                let d_lambda = -(vn + st.scalar_bias) / st.scalar_k;
                if d_lambda.is_finite() {
                    st.scalar_impulse += d_lambda;
                    apply_impulse(views, st.a, st.b, st.ra, st.rb, st.scalar_dir * d_lambda);
                }
            }

            // --- 点约束 ---
            if let Some(k) = st.point {
                if let Some(inv) = k.try_inverse() {
                    let va = views[st.a];
                    let vb = views[st.b];
                    let vrel = vb.velocity_at(st.rb) - va.velocity_at(st.ra);
                    let lambda = inv.mul_vec3(-vrel);
                    if lambda.is_finite() {
                        st.point_impulse += lambda;
                        apply_impulse(views, st.a, st.b, st.ra, st.rb, lambda);
                    }
                }
            }

            // --- 2 自由度约束（铰链角度 / 滑块平移）---
            if let Some(k2) = st.uv2 {
                match st.kind {
                    JointKind::Slider { .. } => {
                        if let Some(dl) = solve2(k2, slider_rhs(st, views)) {
                            if dl[0].is_finite() && dl[1].is_finite() {
                                st.uv_impulse[0] += dl[0];
                                st.uv_impulse[1] += dl[1];
                                let l = st.u * dl[0] + st.v * dl[1];
                                apply_impulse(views, st.a, st.b, Vec3::ZERO, Vec3::ZERO, l);
                            }
                        }
                    }
                    _ => {
                        let (axis_a, axis_b) = match st.kind {
                            JointKind::Hinge { axis_a, axis_b } => (axis_a, axis_b),
                            _ => (st.axis, st.axis),
                        };
                        if let Some(dl) = solve2(k2, angular_rhs(st, views, dt, axis_a, axis_b)) {
                            if dl[0].is_finite() && dl[1].is_finite() {
                                st.uv_impulse[0] += dl[0];
                                st.uv_impulse[1] += dl[1];
                                let l = st.u * dl[0] + st.v * dl[1];
                                angular_impulse(views, st.a, st.b, l);
                            }
                        }
                    }
                }
            }

            // --- 滑块的角向 2 自由度约束 ---
            if let Some(ka) = st.uv_ang {
                let (axis_a, axis_b) = match st.kind {
                    JointKind::Slider { axis_a, axis_b } => (axis_a, axis_b),
                    _ => (st.axis, st.axis),
                };
                let rhs = angular_rhs(st, views, dt, axis_a, axis_b);
                if let Some(dl) = solve2(ka, rhs) {
                    if dl[0].is_finite() && dl[1].is_finite() {
                        st.uv_ang_impulse[0] += dl[0];
                        st.uv_ang_impulse[1] += dl[1];
                        let l = st.u * dl[0] + st.v * dl[1];
                        angular_impulse(views, st.a, st.b, l);
                    }
                }
            }

            // --- 3 自由度角度约束 ---
            if let Some(k) = st.ang3 {
                if let Some(inv) = k.try_inverse() {
                    let va = views[st.a];
                    let vb = views[st.b];
                    let wrel = vb.ang - va.ang;
                    let lambda = inv.mul_vec3(-wrel);
                    if lambda.is_finite() {
                        st.ang3_impulse += lambda;
                        angular_impulse(views, st.a, st.b, lambda);
                    }
                }
            }

            // --- 限位 ---
            if let Some((lo, hi)) = st.limits {
                let (target, positive) = if st.coord < lo {
                    (lo - st.coord, true)
                } else if st.coord > hi {
                    (hi - st.coord, false)
                } else {
                    (0.0, true)
                };
                if target != 0.0 {
                    match st.kind {
                        JointKind::Slider { .. } => {
                            // 滑块的平移约束建立在**质心**上（见 `prepare` 中的
                            // `kt`），因此轴向限位也要在质心施加纯平动冲量，
                            // 否则会额外引入力矩、破坏「一次迭代收敛」的性质。
                            let va = views[st.a];
                            let vb = views[st.b];
                            let k_ax = va.inv_m + vb.inv_m;
                            if k_ax > 1e-9 {
                                let vrel = (vb.lin - va.lin).dot(st.axis);
                                let bias = clamp((0.2 / dt.max(1e-4)) * target, -20.0, 20.0);
                                let mut d = (bias - vrel) / k_ax;
                                let new = if positive {
                                    (st.limit_impulse + d).max(0.0)
                                } else {
                                    (st.limit_impulse + d).min(0.0)
                                };
                                d = new - st.limit_impulse;
                                st.limit_impulse = new;
                                apply_impulse(
                                    views,
                                    st.a,
                                    st.b,
                                    Vec3::ZERO,
                                    Vec3::ZERO,
                                    st.axis * d,
                                );
                            }
                        }
                        _ => {
                            if st.k_limit > 0.0 {
                                let va = views[st.a];
                                let vb = views[st.b];
                                let w_axis = (vb.ang - va.ang).dot(st.axis);
                                let bias = clamp((0.2 / dt.max(1e-4)) * target, -20.0, 20.0);
                                let mut d = (bias - w_axis) * st.k_limit;
                                let new = if positive {
                                    (st.limit_impulse + d).max(0.0)
                                } else {
                                    (st.limit_impulse + d).min(0.0)
                                };
                                d = new - st.limit_impulse;
                                st.limit_impulse = new;
                                angular_impulse(views, st.a, st.b, st.axis * d);
                            }
                        }
                    }
                }
            }

            // --- 马达 ---
            if let Some(m) = st.motor {
                if st.k_motor > 0.0 {
                    let va = views[st.a];
                    let vb = views[st.b];
                    let w_axis = (vb.ang - va.ang).dot(st.axis);
                    let max_i = m.max_impulse.abs();
                    let mut d = (m.target_velocity - w_axis) * st.k_motor;
                    let new = clamp(st.motor_impulse + d, -max_i, max_i);
                    d = new - st.motor_impulse;
                    st.motor_impulse = new;
                    angular_impulse(views, st.a, st.b, st.axis * d);
                }
            }
        }
    }

    /// 位置修正：平移约束用伪速度（分裂冲量）。
    pub fn solve_position(
        &mut self,
        views: &mut [BodyView],
        dt: f32,
        iterations: usize,
        beta: f32,
        max_speed: f32,
    ) {
        if dt <= 0.0 || iterations == 0 {
            return;
        }
        for st in self.states.iter_mut() {
            st.point_pseudo = Vec3::ZERO;
        }
        for _ in 0..iterations {
            for st in self.states.iter_mut() {
                if let Some(k) = st.point {
                    if let Some(inv) = k.try_inverse() {
                        let va = views[st.a];
                        let vb = views[st.b];
                        let wa = va.pos + va.rot.mul_vec3(st.local_a);
                        let wb = vb.pos + vb.rot.mul_vec3(st.local_b);
                        let c = wb - wa;
                        if c.length_squared() > 1e-12 {
                            let bias = (c * (beta / dt)).clamp_length(max_speed);
                            let vrel = vb.pseudo_velocity_at(st.rb) - va.pseudo_velocity_at(st.ra);
                            let lambda = inv.mul_vec3(-(vrel + bias));
                            if lambda.is_finite() {
                                st.point_pseudo += lambda;
                                apply_pseudo_impulse(views, st.a, st.b, st.ra, st.rb, lambda);
                            }
                        }
                    }
                }
                // 刚性距离约束：用伪速度把长度误差收敛掉。
                if st.use_scalar && st.scalar_rigid && st.scalar_k > 0.0 && st.rest_length > 0.0 {
                    let va = views[st.a];
                    let vb = views[st.b];
                    let wa = va.pos + va.rot.mul_vec3(st.local_a);
                    let wb = vb.pos + vb.rot.mul_vec3(st.local_b);
                    let d = wb - wa;
                    let len = d.length();
                    if len > 1e-6 {
                        let n = d / len;
                        let err = len - st.rest_length;
                        let target = clamp(-(beta / dt) * err, -max_speed, max_speed);
                        let vrel = vb.pseudo_velocity_at(st.rb) - va.pseudo_velocity_at(st.ra);
                        let k = va.k_axis(st.ra, n) + vb.k_axis(st.rb, n);
                        if k > 0.0 {
                            let dl = (target - vrel.dot(n)) / k;
                            if dl.is_finite() {
                                apply_pseudo_impulse(views, st.a, st.b, st.ra, st.rb, n * dl);
                            }
                        }
                    }
                }
                // 滑块的垂直平移约束也要做位置修正。
                if let (JointKind::Slider { .. }, Some(k2)) = (st.kind, st.uv2) {
                    let va = views[st.a];
                    let vb = views[st.b];
                    // 与速度约束一致：修正质心在垂直方向的相对偏移。
                    let c = vb.pos - va.pos;
                    let target = [-(c.dot(st.u)) * (beta / dt), -(c.dot(st.v)) * (beta / dt)];
                    let target = [
                        clamp(target[0], -max_speed, max_speed),
                        clamp(target[1], -max_speed, max_speed),
                    ];
                    let vrel = vb.pseudo_velocity_at(st.rb) - va.pseudo_velocity_at(st.ra);
                    let rhs = [target[0] - vrel.dot(st.u), target[1] - vrel.dot(st.v)];
                    if let Some(dl) = solve2(k2, rhs) {
                        if dl[0].is_finite() && dl[1].is_finite() {
                            let l = st.u * dl[0] + st.v * dl[1];
                            apply_pseudo_impulse(views, st.a, st.b, Vec3::ZERO, Vec3::ZERO, l);
                        }
                    }
                }
            }
        }
    }

    /// 收集本步的累积冲量，写回关节槽位。
    pub fn store_impulses(&self, out: &mut [JointImpulses]) {
        for (i, st) in self.states.iter().enumerate() {
            if let Some(slot) = out.get_mut(i) {
                *slot = JointImpulses {
                    point: st.point_impulse,
                    ang3: st.ang3_impulse,
                    uv: st.uv_impulse,
                    uv_ang: st.uv_ang_impulse,
                    scalar: st.scalar_impulse,
                    limit: st.limit_impulse,
                    motor: st.motor_impulse,
                };
            }
        }
    }
}

/// 铰链当前角度（绕轴、以 A 的垂直参考向量为基准）。
fn hinge_angle(
    local_axis_a: Vec3,
    local_axis_b: Vec3,
    va: &BodyView,
    vb: &BodyView,
    axis: Vec3,
) -> f32 {
    let (ref_a, _) = local_axis_a.normalize_or_zero().any_orthonormal_pair();
    let (ref_b, _) = local_axis_b.normalize_or_zero().any_orthonormal_pair();
    let wa = va.rot.mul_vec3(ref_a);
    let wb = vb.rot.mul_vec3(ref_b);
    let pa = (wa - axis * wa.dot(axis)).normalize_or_zero();
    let pb = (wb - axis * wb.dot(axis)).normalize_or_zero();
    if pa.length_squared() < 0.5 || pb.length_squared() < 0.5 {
        return 0.0;
    }
    axis.dot(pa.cross(pb)).atan2(pa.dot(pb))
}

/// 角度 2 自由度约束右端项（铰链 / 滑块共用）。
fn angular_rhs(
    st: &JointState,
    views: &[BodyView],
    dt: f32,
    axis_a: Vec3,
    axis_b: Vec3,
) -> [f32; 2] {
    let va = views[st.a];
    let vb = views[st.b];
    let err = axis_a.cross(axis_b);
    let wrel = vb.ang - va.ang;
    let gain = clamp(0.2 / dt.max(1e-4), 0.0, 60.0);
    [
        -(wrel.dot(st.u) + gain * err.dot(st.u)),
        -(wrel.dot(st.v) + gain * err.dot(st.v)),
    ]
}

/// 滑块垂直平移约束右端项。
fn slider_rhs(st: &JointState, views: &[BodyView]) -> [f32; 2] {
    let va = views[st.a];
    let vb = views[st.b];
    let vrel = vb.lin - va.lin;
    [-vrel.dot(st.u), -vrel.dot(st.v)]
}

/// 施加纯角冲量。
pub(crate) fn angular_impulse(views: &mut [BodyView], a: usize, b: usize, l: Vec3) {
    if !l.is_finite() {
        return;
    }
    if let Some(va) = views.get_mut(a) {
        if va.inv_m > 0.0 {
            va.ang -= va.inv_i_world(l);
        }
    }
    if let Some(vb) = views.get_mut(b) {
        if vb.inv_m > 0.0 {
            vb.ang += vb.inv_i_world(l);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solve2_solves_identity() {
        let r = solve2([[2.0, 0.0], [0.0, 4.0]], [2.0, 8.0]).unwrap();
        assert!((r[0] - 1.0).abs() < 1e-6);
        assert!((r[1] - 2.0).abs() < 1e-6);
    }

    #[test]
    fn solve2_rejects_singular() {
        assert!(solve2([[1.0, 2.0], [2.0, 4.0]], [1.0, 1.0]).is_none());
    }

    #[test]
    fn joint_reports_its_bodies() {
        let a = BodyHandle {
            index: 3,
            generation: 1,
        };
        let b = BodyHandle {
            index: 7,
            generation: 2,
        };
        let j = Joint::Ball {
            a,
            b,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: vec3(0.0, -1.0, 0.0),
        };
        assert_eq!(j.bodies(), (a, b));
        assert_eq!(j.local_anchor_b(), vec3(0.0, -1.0, 0.0));
    }
}
