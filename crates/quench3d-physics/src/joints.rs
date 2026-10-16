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

/// 铰链马达：以给定最大冲量驱动相对角速度（**速度模式**）或相对角度（**伺服模式**）。
///
/// 两种模式的区别：
/// * 速度模式（`target_angle: None`）：`target_velocity` 是"目标相对角速度"，给多少转多少，
///   像电机的恒速档；
/// * 伺服模式（`target_angle: Some(θ)`）：把它当**位置伺服**——目标速度由
///   `stiffness × (θ - 当前角) - damping × 当前相对角速度` 现场算出来，
///   再用 `max_impulse` 限幅。用来做"门自动开到 90°""机械臂摆到指定角度"。
///
/// 伺服增益不是随便填的：`stiffness` 单位 1/s（"离目标 1 弧度时想要多大角速度"），
/// `damping` 无量纲。缺省 `stiffness = 8`、`damping = 1`，对小门/机械臂够用；
/// `max_impulse` 仍然决定"推得动多重的门"。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HingeMotor {
    /// 目标相对角速度（rad/s）；伺服模式下这一项被忽略。
    pub target_velocity: f32,
    /// 单步最大冲量。
    pub max_impulse: f32,
    /// 伺服模式的目标角度（rad）；`None` = 速度模式。
    pub target_angle: Option<f32>,
    /// 伺服比例增益（1/s）："离目标 1 弧度时想要多大角速度"。
    pub stiffness: f32,
    /// 伺服求解平滑系数（≥ 0，缺省 1）。
    ///
    /// 它**不改变最终停在哪个角度**，只改变逼近得有多"急"：内部按
    /// `1/(1+damping)` 的松弛系数分多次迭代逼近目标角速度。
    /// `0` = 一次迭代到位（最急），`≥10` = 很柔和。
    pub damping: f32,
}

impl HingeMotor {
    /// 速度模式（最常用的一种，写起来短一点）。
    pub fn velocity(target_velocity: f32, max_impulse: f32) -> Self {
        Self {
            target_velocity,
            max_impulse,
            target_angle: None,
            stiffness: 8.0,
            damping: 1.0,
        }
    }

    /// 伺服（位置）模式：转到 `target_angle`（弧度）并停在那里。
    pub fn servo(target_angle: f32, max_impulse: f32) -> Self {
        Self {
            target_angle: Some(target_angle),
            ..Self::velocity(0.0, max_impulse)
        }
    }

    /// 改伺服增益（比例 / 阻尼）。
    pub fn gains(mut self, stiffness: f32, damping: f32) -> Self {
        self.stiffness = stiffness;
        self.damping = damping;
        self
    }

    /// 当前这一子步想要达到的相对角速度。
    ///
    /// 三个容易踩的坑都在这里处理掉：
    /// * **角度回绕**：铰链坐标是 `(-π, π]` 的。如果直接算 `target - coord`，
    ///   刚从 179° 转过去的一瞬间坐标跳到 -179°，误差变成 +358° → 伺服会"接着往前推"，
    ///   再也追不上目标（表现为门疯狂旋转）。所以误差必须先绕回 `(-π, π]`。
    /// * **速度上限**：误差很大时比例项会要求几百度/秒的角速度，`max_impulse` 又允许多大，
    ///   结果是一次猛冲。这里把目标速度夹在 `MAX_SERVO_SPEED` 以内。
    /// * **非有限值**：一个 NaN 目标角不该让整个世界变成 NaN。
    ///
    /// 注意这里**只用位置环**（`kp·误差`），不做 `-kd·ω` 的显式阻尼：那个写法在
    /// 序列冲量求解器里会把"求解一行"变成"w 在 0 与目标之间来回跳"，迭代次数是偶数时
    /// 甚至原地不动、是奇数时猛冲（实测：同样的场景在 60 N·m·s 限幅下冲到 81° 停住）。
    /// 位置环本身就把速度当成"误差的线性函数"，天然是临界阻尼的：`dθ/dt = kp·(目标-θ)`
    /// 收敛不过冲。`damping` 改成"松弛系数"，只影响每次迭代逼近的快慢。
    fn desired_velocity(&self, coord: f32) -> f32 {
        match self.target_angle {
            Some(target) if target.is_finite() && coord.is_finite() => {
                let kp = self.stiffness.clamp(0.0, 60.0);
                let error = wrap_pi(target - coord);
                (error * kp).clamp(-MAX_SERVO_SPEED, MAX_SERVO_SPEED)
            }
            Some(_) => 0.0,
            None if self.target_velocity.is_finite() => self.target_velocity,
            None => 0.0,
        }
    }

    /// 每次速度迭代吸收多少"目标速度 - 当前速度"（0~1）。
    ///
    /// 速度模式下是 1（一条纯速度约束，一步到位）；伺服模式下是 `1/(1+damping)`，
    /// 让逼近过程可以被调柔。两种模式最终稳态一致。
    fn velocity_relaxation(&self) -> f32 {
        if self.target_angle.is_some() {
            1.0 / (1.0 + self.damping.clamp(0.0, 10.0))
        } else {
            1.0
        }
    }
}

/// 伺服马达允许的目标角速度上限（rad/s，≈ 24 转/秒）。
///
/// 存在的理由：伺服是"位置控制"，用户给的是角度和增益，不该因为 `max_impulse` 给大了
/// 就变成"瞬间甩过去"。有了这个上限，伺服的行为只由目标角与增益决定。
const MAX_SERVO_SPEED: f32 = 150.0;

/// 把角度绕回 `(-π, π]`。
fn wrap_pi(mut x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let tau = core::f32::consts::TAU;
    x = (x + core::f32::consts::PI).rem_euclid(tau) - core::f32::consts::PI;
    x
}

/// 球窝关节的**锥形限位**：两端各给一根局部轴，两轴夹角不超过 `half_angle`（弧度）。
///
/// 典型用法：`local_axis_a = [0,1,0]`（躯干朝上）、`local_axis_b` 是手臂的朝上方向、
/// `half_angle = 60°` → 手臂最多摆到 60° 的锥面内。它约束的是"两根轴的方向差"，
/// 绕轴自转仍然自由（球窝本来就该这样）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BallConeLimit {
    /// 允许的最大夹角（弧度，0~π）。
    pub half_angle: f32,
    /// A 端局部轴。
    pub local_axis_a: Vec3,
    /// B 端局部轴。
    pub local_axis_b: Vec3,
}

/// 关节类型。
///
/// 约定：`local_anchor_*` 是相对各自刚体原点的**本地点**，创建时两端锚点在
/// 世界坐标下应重合（否则会在第一步被强行拉到一起）。
#[derive(Clone, Debug)]
pub enum Joint {
    /// 固定关节：两刚体完全刚性连接（点约束 + 3 自由度角速度约束）。
    ///
    /// 角约束有两种模式，由 `local_frame_a` / `local_frame_b` **是否都是单位四元数**决定：
    ///
    /// * 都是 `Quat::IDENTITY`（缺省）：**历史行为** —— 速度级角约束锁定**创建那一刻**的
    ///   相对姿态，求解器不看参考姿态。老场景因此逐位不变。
    /// * 至少一个不是单位四元数：求解器把两端扭到**参考姿态重合**，即驱动
    ///   `q_a ⊗ local_frame_a == q_b ⊗ local_frame_b`（见 [`fixed_frame_error`]）。
    ///   用它表达"让 B 的某个特征朝向跟 A 的某个特征朝向对齐"，不必先手算世界朝向。
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
    /// 球窝关节：只约束两锚点重合（转动自由；可选锥形限位限制摆动方向）。
    Ball {
        /// 刚体 A。
        a: BodyHandle,
        /// 刚体 B。
        b: BodyHandle,
        /// A 的本地锚点。
        local_anchor_a: Vec3,
        /// B 的本地锚点。
        local_anchor_b: Vec3,
        /// 锥形限位（`None` = 随便转）。
        cone_limit: Option<BallConeLimit>,
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

    /// 这个关节是否有**活动的马达**（速度或伺服模式）。
    ///
    /// 世界用它决定"被驱动的刚体这一步不许睡"（见 `update_sleeping`）。
    /// 马达存在就算活动——包括 `target_velocity = 0`：那表示"用马达把轴按住"，
    /// 松手（把马达删掉）之后才该恢复正常睡眠。
    pub fn has_active_motor(&self) -> bool {
        matches!(self, Joint::Hinge { motor: Some(_), .. })
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

    /// 固定关节的**参考姿态是否被显式给出**（即至少一个不是单位四元数）。
    ///
    /// 这就是求解器选哪条角约束模式的开关：`false` = 锁定创建时的相对姿态（历史行为，
    /// 逐位不变），`true` = 把两端扭到参考姿态重合。写成纯函数是为了让校验层、编辑器和
    /// 求解器**用同一份判据**（少一份就多一处"预览和实跑不一致"）。
    pub fn fixed_uses_reference_frames(&self) -> bool {
        match self {
            Joint::Fixed {
                local_frame_a,
                local_frame_b,
                ..
            } => *local_frame_a != Quat::IDENTITY || *local_frame_b != Quat::IDENTITY,
            _ => false,
        }
    }

    /// 这个关节是否**每一步都在主动施加修正**：马达，或固定关节的参考姿态对齐。
    ///
    /// 与 [`Joint::has_active_motor`] 是两件事：那个只说"有马达"。世界用本方法（而不是
    /// `has_active_motor`）决定"被驱动的刚体这一步不许睡"。理由：睡眠体在求解器里是
    /// **完全冻结**的（`BodyView::from_body` 把 `inv_m` / `inv_I` 清零），一旦睡着，
    /// 参考姿态修正会停在半路，而刚体再被推歪也不会被拉回来。
    /// 缺省（两端参考姿态都是单位四元数）的固定关节不在其列 —— 老场景的睡眠行为不变。
    pub fn drives_bodies(&self) -> bool {
        self.has_active_motor() || self.fixed_uses_reference_frames()
    }
}

/// 固定关节的**参考姿态误差**（世界系旋转向量，弧度；长度 ≤ π）。
///
/// 两端参考姿态在世界中的朝向分别是 `q_a ⊗ local_frame_a` 与 `q_b ⊗ local_frame_b`
/// （`q_*` 是刚体世界朝向，`⊗` 是 [`Quat`] 的 Hamilton 积）。返回值是"把 B 端参考姿态
/// 转到 A 端参考姿态所需的那次旋转"（轴 × 角）：长度 0 = 已经重合。
///
/// 两个细节值得写下来：
/// * `-q` 与 `q` 是同一个旋转，这里取 `w ≥ 0` 的那一支，所以返回的是**最短**的那次旋转
///   （不翻转的话，`w < 0` 时 `to_axis_angle` 会给出去 191° 而不是 -169° 的"绕远路"解）。
/// * 参考姿态都是单位四元数时，本函数**不**等于"把两个刚体扭到同朝向"：那一支走的是
///   历史行为（锁定创建时的相对姿态），求解器根本不算它（见
///   [`Joint::fixed_uses_reference_frames`]）。
pub fn fixed_frame_error(q_a: Quat, local_frame_a: Quat, q_b: Quat, local_frame_b: Quat) -> Vec3 {
    let mut e = q_b * local_frame_b * (q_a * local_frame_a).inverse();
    if e.w < 0.0 {
        e = e * -1.0;
    }
    let (axis, angle) = e.to_axis_angle();
    axis * angle
}

/// 固定关节参考姿态误差的**夹角**（度，0~180）。
///
/// 给校验与编辑器用：`> 0` 就是"这一段会真的扭过去"，数值本身可以直接进人话诊断。
pub fn fixed_frame_error_deg(
    q_a: Quat,
    local_frame_a: Quat,
    q_b: Quat,
    local_frame_b: Quat,
) -> f32 {
    fixed_frame_error(q_a, local_frame_a, q_b, local_frame_b)
        .length()
        .to_degrees()
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
    /// 固定关节的参考姿态误差（世界系旋转向量，弧度；`Vec3::ZERO` = 走历史行为那支，
    /// 求解器不做任何偏置，老场景逐位不变）。
    pub ang3_error: Vec3,
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
                ang3_error: Vec3::ZERO,
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
                Joint::Ball { cone_limit, .. } => {
                    // 锥形限位复用铰链"限位"那一行状态（`limits` / `coord` / `axis` /
                    // `k_limit`），只是把坐标换成"两轴夹角"、修正轴换成"两轴叉积"。
                    // 好处是不必新写一条约束行：限位那套已经测过的收敛行为直接复用。
                    if let Some(cone) = cone_limit {
                        let wa = va.rot.mul_vec3(cone.local_axis_a).normalize_or_zero();
                        let wb = vb.rot.mul_vec3(cone.local_axis_b).normalize_or_zero();
                        if wa.length_squared() > 0.5 && wb.length_squared() > 0.5 {
                            let angle = wa.dot(wb).clamp(-1.0, 1.0).acos();
                            // 修正轴 = 把 B 轴转回 A 轴的旋转轴。两轴几乎平行/反平行时
                            // 叉积退化（没有确定的平面），此时限位本来"没在限"，跳过。
                            let axis = wa.cross(wb);
                            if axis.length_squared() > 1e-10 {
                                let axis = axis.normalize_or_zero();
                                let k_axis = axis.dot(ang_vec(&va, &vb, axis));
                                if k_axis > 1e-9 {
                                    st.kind = JointKind::Ball;
                                    st.limits = Some((
                                        0.0,
                                        cone.half_angle.clamp(0.0, core::f32::consts::PI),
                                    ));
                                    st.coord = angle;
                                    st.axis = axis;
                                    st.k_limit = 1.0 / k_axis;
                                }
                            }
                        }
                    }
                }
                Joint::Fixed {
                    local_frame_a,
                    local_frame_b,
                    ..
                } => {
                    st.kind = JointKind::Fixed;
                    let k = Mat3::from_cols(
                        ang_vec(&va, &vb, Vec3::X),
                        ang_vec(&va, &vb, Vec3::Y),
                        ang_vec(&va, &vb, Vec3::Z),
                    );
                    if usable(&k) && k.determinant().abs() > 1e-14 {
                        st.ang3 = Some(k);
                    }
                    // 参考姿态：两端都是单位四元数 = 老行为（误差保持 ZERO，下面的速度
                    // 迭代走原来那条"锁相对角速度"的路径，逐位不变）；只要有一端给了
                    // 参考姿态，就把误差算出来当这一步的目标相对角速度（见
                    // `frame_target_velocity`）。
                    if *local_frame_a != Quat::IDENTITY || *local_frame_b != Quat::IDENTITY {
                        st.ang3_error =
                            fixed_frame_error(va.rot, *local_frame_a, vb.rot, *local_frame_b);
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
                    // 固定关节给了参考姿态时，这一步的目标不是"相对角速度 = 0"，而是
                    // "能把参考姿态误差拉回 0 的那个相对角速度"；误差为 ZERO（缺省 /
                    // 两端参考姿态都是单位四元数）时走**原来那条表达式**，老场景逐位不变。
                    let rhs = if st.ang3_error.length_squared() > 0.0 {
                        frame_target_velocity(st.ang3_error, dt) - wrel
                    } else {
                        -wrel
                    };
                    let lambda = inv.mul_vec3(rhs);
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
                    let target = m.desired_velocity(st.coord);
                    let mut d = (target - w_axis) * st.k_motor * m.velocity_relaxation();
                    let new = clamp(st.motor_impulse + d, -max_i, max_i);
                    d = new - st.motor_impulse;
                    st.motor_impulse = new;
                    angular_impulse(views, st.a, st.b, st.axis * d);
                }
            }
        }
    }

    /// 每个关节本步的**约束力量级**（`(力 N, 力矩 N·m)`），顺序与 `prepare` 的
    /// entries 一致（也就是世界按槽位顺序数过来的第 k 个关节）。
    ///
    /// 算法是"累积冲量 / dt"的**量级估计**，不是精确的约束力：
    /// 它把点约束、标量约束、滑块限位算进力，把角度约束、铰链/球窝限位与马达算进力矩。
    /// 断裂阈值本来就是"用户给的工程值"，量级够用比精确更重要——文档里也这么写。
    pub fn break_magnitudes(&self, dt: f32) -> Vec<(f32, f32)> {
        let dt = if dt.is_finite() && dt > 1e-6 {
            dt
        } else {
            1e-6
        };
        let mut out = Vec::with_capacity(self.states.len());
        for st in &self.states {
            let is_slider = matches!(st.kind, JointKind::Slider { .. });
            let is_hinge_like = matches!(st.kind, JointKind::Hinge { .. } | JointKind::Ball);
            let linear = st.point_impulse.length()
                + st.scalar_impulse.abs()
                + if is_slider {
                    st.limit_impulse.abs() + st.uv_impulse[0].abs() + st.uv_impulse[1].abs()
                } else {
                    0.0
                };
            let angular = st.ang3_impulse.length()
                + if is_slider {
                    st.uv_ang_impulse[0].abs() + st.uv_ang_impulse[1].abs()
                } else {
                    st.uv_impulse[0].abs() + st.uv_impulse[1].abs()
                }
                + if is_hinge_like {
                    st.limit_impulse.abs() + st.motor_impulse.abs()
                } else {
                    0.0
                };
            out.push((linear / dt, angular / dt));
        }
        out
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

/// 固定关节参考姿态的修正增益（无量纲）：与铰链/滑块的轴对齐偏置同源（`0.2/dt`），
/// 每步把误差收掉约 20%。
const FRAME_GAIN: f32 = 0.2;

/// 参考姿态修正的目标角速度上限（rad/s）：误差很大（例如差 180°）时也不一步甩过去。
const MAX_FRAME_SPEED: f32 = 20.0;

/// 把固定关节的参考姿态误差（世界系旋转向量，弧度）转成"这一步想让 B 相对 A
/// 达到的角速度（世界系）"。
///
/// 为什么带负号：误差是"把 B 端参考姿态转到 A 端参考姿态的那次旋转"，要让误差变小
/// 就得反着转，所以目标相对角速度 = `-增益 × 误差`。增益写成 `0.2/dt`（与
/// [`angular_rhs`] 的轴对齐偏置一致）：一步只收 20%，是**一阶**收敛，不振荡。
///
/// 速度上限用 `clamp_length` 而不是逐分量夹紧：夹紧后的方向必须还是误差的反方向，
/// 否则关节会朝错误的方向扭。
fn frame_target_velocity(error: Vec3, dt: f32) -> Vec3 {
    let gain = clamp(FRAME_GAIN / dt.max(1e-4), 0.0, 60.0);
    (-error * gain).clamp_length(MAX_FRAME_SPEED)
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
            cone_limit: None,
        };
        assert_eq!(j.bodies(), (a, b));
        assert_eq!(j.local_anchor_b(), vec3(0.0, -1.0, 0.0));
    }

    /// 参考姿态误差：`q_a ⊗ f_a == q_b ⊗ f_b` 时必须是零向量（不管两端刚体各自转了多少）。
    #[test]
    fn fixed_frame_error_is_zero_when_frames_coincide() {
        let q_a = Quat::from_rotation_z(0.7);
        let q_b = Quat::from_rotation_x(-1.3);
        // 先随便给一个参考姿态，然后让 B 的参考姿态正好落在同一个世界朝向上。
        let f_a = Quat::from_rotation_y(0.4);
        let f_b = q_b.inverse() * q_a * f_a;
        let e = fixed_frame_error(q_a, f_a, q_b, f_b);
        assert!(e.length() < 1e-5, "参考姿态已重合，误差应为 0，实际 {e:?}");
        assert!(fixed_frame_error_deg(q_a, f_a, q_b, f_b) < 1e-3);
    }

    /// A 给了 90° 参考姿态、B 没给：误差应该正好是"把 B 反向转 90°"（轴 = -Z，长度 = π/2）。
    #[test]
    fn fixed_frame_error_points_from_b_back_to_a() {
        let f_a = Quat::from_rotation_z(core::f32::consts::FRAC_PI_2);
        let e = fixed_frame_error(Quat::IDENTITY, f_a, Quat::IDENTITY, Quat::IDENTITY);
        assert!(
            (e.length() - core::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "{e:?}"
        );
        assert!(e.z < -1.5, "修正方向应该是 -Z，实际 {e:?}");
        assert!(e.x.abs() < 1e-4 && e.y.abs() < 1e-4, "{e:?}");
    }

    /// **最短路径**不变式：`-q` 与 `q` 是同一个旋转，误差长度永远 ≤ 180°。
    /// 少了 `w < 0` 那一支的翻转，190° 会被报成 190°（绕远路），这里就红了。
    #[test]
    fn fixed_frame_error_never_exceeds_half_turn() {
        let pi = core::f32::consts::PI;
        for deg in [10.0f32, 90.0, 179.0, 181.0, 190.0, 270.0, 350.0] {
            let f_a = Quat::from_rotation_z(deg.to_radians());
            let e = fixed_frame_error(Quat::IDENTITY, f_a, Quat::IDENTITY, Quat::IDENTITY);
            assert!(
                e.length() <= pi + 1e-4,
                "{deg}° 的误差长度 {} 超过半圈（没走最短路径）",
                e.length()
            );
            // 误差的方向 × 长度必须还原成"把 B 转到 A 参考姿态"那次旋转。
            let want = f_a.to_axis_angle();
            let got_axis = e.normalize_or_zero();
            let axis_ok = got_axis.dot(want.0).abs() > 0.999 || want.1 < 1e-3;
            assert!(axis_ok, "{deg}° 的误差轴不对：{e:?} vs {:?}", want.0);
        }
    }

    /// 模式开关：两端参考姿态都是单位四元数 = 老行为（求解器不看参考姿态）。
    #[test]
    fn fixed_uses_reference_frames_only_when_a_frame_is_given() {
        let a = BodyHandle {
            index: 0,
            generation: 0,
        };
        let b = BodyHandle {
            index: 1,
            generation: 0,
        };
        let mk = |fa: Quat, fb: Quat| Joint::Fixed {
            a,
            b,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: Vec3::ZERO,
            local_frame_a: fa,
            local_frame_b: fb,
        };
        assert!(!mk(Quat::IDENTITY, Quat::IDENTITY).fixed_uses_reference_frames());
        assert!(mk(Quat::from_rotation_x(0.1), Quat::IDENTITY).fixed_uses_reference_frames());
        assert!(mk(Quat::IDENTITY, Quat::from_rotation_y(0.1)).fixed_uses_reference_frames());
        // 老固定关节不该因为"参考姿态"被算成每步驱动（否则睡眠行为会变）。
        assert!(!mk(Quat::IDENTITY, Quat::IDENTITY).drives_bodies());
        assert!(mk(Quat::from_rotation_x(0.1), Quat::IDENTITY).drives_bodies());
    }

    /// 修正速度：方向必须是误差的反方向，长度受 `MAX_FRAME_SPEED` 限制。
    #[test]
    fn frame_target_velocity_opposes_the_error_and_is_bounded() {
        let v = frame_target_velocity(vec3(0.0, 0.0, -0.5), 1.0 / 60.0);
        assert!(v.z > 0.0, "误差在 -Z 上，修正速度应该朝 +Z：{v:?}");
        assert!((v.length() - 0.5 * 12.0).abs() < 1e-3, "{v:?}");
        // 180° 的误差按增益算要 37.7 rad/s，必须被夹到上限以内（且方向不变）。
        let big = frame_target_velocity(vec3(0.0, 0.0, -core::f32::consts::PI), 1.0 / 60.0);
        assert!((big.length() - MAX_FRAME_SPEED).abs() < 1e-3, "{big:?}");
        assert!(big.z > 0.0, "{big:?}");
        // 零误差 → 零速度（也就是"老路径"那个 `-wrel`）。
        assert_eq!(frame_target_velocity(Vec3::ZERO, 1.0 / 60.0), Vec3::ZERO);
    }
}
