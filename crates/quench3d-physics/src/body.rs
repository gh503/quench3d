//! 刚体、世代句柄与构建器。

use quench3d_math::*;

use crate::shape::{MassProperties, Shape};

/// 刚体类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyType {
    /// 静态：永不移动，质量为无限大。
    Static,
    /// 动态：受重力与冲量驱动。
    Dynamic,
    /// 运动学：不受力，但按给定速度移动（可推动动态体）。
    Kinematic,
}

/// 碰撞过滤分组（层 / 掩码）。
///
/// 一对刚体**只有互相"看得上"才会产生接触**：
/// `A.memberships & B.filter != 0 && B.memberships & A.filter != 0`。
/// 两个条件都要满足（对称），所以"我想碰你、你不想碰我"的结果是**不碰**。
///
/// 缺省是"都在**第 0 层**、和所有层碰"（`memberships = 1 << 0`、`filter = ALL`），
/// 也就是**不设置就等于没有这个功能**——老场景逐位不变。
///
/// 注意 `memberships` 缺省**不是** `ALL`：`ALL` 表示"32 个层都是我的成员"，
/// 那样一个只想碰第 5 层的过滤体（`filter = 1 << 5`）反而会把它当成同类。
/// 想要"在所有层里"就显式用 [`InteractionGroups::all`]。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InteractionGroups {
    /// 自己属于哪些层（位掩码）。
    pub memberships: u32,
    /// 愿意和哪些层碰（位掩码）。
    pub filter: u32,
}

impl InteractionGroups {
    /// 所有位（32 层都在内）。
    pub const ALL: u32 = u32::MAX;
    /// 不碰任何东西的分组（`filter = 0`）。
    pub const NONE: Self = Self {
        memberships: Self::ALL,
        filter: 0,
    };

    /// 显式给出成员层与过滤层。
    #[inline]
    pub const fn new(memberships: u32, filter: u32) -> Self {
        Self {
            memberships,
            filter,
        }
    }

    /// 只在第 `layer` 层，且和所有层碰。
    #[inline]
    pub const fn layer(layer: u8) -> Self {
        Self {
            memberships: 1u32 << (layer & 31),
            filter: Self::ALL,
        }
    }

    /// 一对刚体是否互相参与碰撞。
    #[inline]
    pub const fn interacts(self, other: Self) -> bool {
        (self.memberships & other.filter) != 0 && (other.memberships & self.filter) != 0
    }
}

impl Default for InteractionGroups {
    /// 第 0 层 + 和所有层碰（**不是** `memberships = ALL`，见类型文档）。
    #[inline]
    fn default() -> Self {
        Self {
            memberships: 1,
            filter: Self::ALL,
        }
    }
}

/// 世代句柄：对象销毁后自增世代，旧句柄随即失效。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BodyHandle {
    /// 槽位下标。
    pub index: u32,
    /// 世代号。
    pub generation: u32,
}

impl BodyHandle {
    /// 构造。
    #[inline]
    pub const fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// 槽位下标（同一槽位被复用后世代会变）。
    #[inline]
    pub const fn index(self) -> u32 {
        self.index
    }
}

/// 速度/角速度的安全上限，避免数值爆炸。
pub(crate) const MAX_LINEAR_SPEED: f32 = 500.0;
/// 角速度安全上限。
pub(crate) const MAX_ANGULAR_SPEED: f32 = 200.0;
/// 进入睡眠的线速度阈值。
pub(crate) const SLEEP_LINEAR_TOL: f32 = 0.05;
/// 进入睡眠的角速度阈值。
pub(crate) const SLEEP_ANGULAR_TOL: f32 = 0.12;
/// 低于阈值持续多久后进入睡眠。
pub(crate) const SLEEP_TIME: f32 = 0.5;

/// 安全求倒数：非正/非有限一律返回 0（表示无限大）。
#[inline]
pub(crate) fn safe_inv(v: f32) -> f32 {
    if v.is_finite() && v > 1e-9 {
        1.0 / v
    } else {
        0.0
    }
}

/// 两个材质接触时，摩擦/恢复系数怎么合成。
///
/// 为什么必须有它：`friction` 是**每端各写一个**的，接触时到底听谁的？
/// 没有显式规则时，引擎只能替你决定（本引擎缺省是几何平均，与改这一版之前的行为
/// 逐位相同）。可一旦要和别的工具对齐（PhysX 的 `eAVERAGE`、Unity 的
/// `Average`/`Min`/`Maximum`/`Multiply`），"我写了 0.5 和 0.5 却得到 0.5"这类
/// 问题就会反复出现——所以把规则摆到台面上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CombineRule {
    /// 几何平均 `sqrt(a*b)`（**缺省**，与历史行为一致）。
    #[default]
    GeometricMean,
    /// 算术平均 `(a+b)/2`（PhysX 的 `eAVERAGE`）。
    Average,
    /// 取小 `min(a,b)`（"软的说了算"，最保守）。
    Min,
    /// 取大 `max(a,b)`（"硬的说了算"，最容易打滑/弹跳）。
    Max,
    /// 相乘 `a*b`（Unity 的 `Multiply`）。
    Multiply,
}

impl CombineRule {
    /// 全部规则（UI 下拉框、文档与测试都靠它遍历）。
    pub const ALL: [CombineRule; 5] = [
        CombineRule::GeometricMean,
        CombineRule::Average,
        CombineRule::Min,
        CombineRule::Max,
        CombineRule::Multiply,
    ];

    /// 场景文件 / 报告里用的名字（与 serde 的 `rename_all = "snake_case"` 一致）。
    pub fn name(self) -> &'static str {
        match self {
            CombineRule::GeometricMean => "geometric_mean",
            CombineRule::Average => "average",
            CombineRule::Min => "min",
            CombineRule::Max => "max",
            CombineRule::Multiply => "multiply",
        }
    }

    /// 从场景文件/报告里的名字解析（与 [`Self::name`] 互逆）。
    pub fn from_name(name: &str) -> Option<Self> {
        CombineRule::ALL.into_iter().find(|r| r.name() == name)
    }

    /// 按规则合成两个系数。
    ///
    /// 负数被当作 0（摩擦/恢复没有负值）；非有限值被当作 0，
    /// 免得一个 NaN 材质把整个世界变成 NaN 速度。
    pub fn combine(self, a: f32, b: f32) -> f32 {
        let a = if a.is_finite() { a.max(0.0) } else { 0.0 };
        let b = if b.is_finite() { b.max(0.0) } else { 0.0 };
        match self {
            CombineRule::GeometricMean => (a * b).sqrt(),
            CombineRule::Average => 0.5 * (a + b),
            CombineRule::Min => a.min(b),
            CombineRule::Max => a.max(b),
            CombineRule::Multiply => a * b,
        }
    }
}

/// 刚体。
#[derive(Clone, Debug, PartialEq)]
pub struct RigidBody {
    /// 刚体类型。
    pub body_type: BodyType,
    /// 形状。
    pub shape: Shape,
    /// 位姿（`position` 为形状本地原点在世界中的位置）。
    pub transform: Transform,
    /// 线速度（质心速度）。
    pub linear_velocity: Vec3,
    /// 角速度。
    pub angular_velocity: Vec3,
    /// 恢复系数 [0, 1]。
    pub restitution: f32,
    /// 动摩擦系数（已经在滑动时用；`friction` 这个老名字就是它）。
    pub friction: f32,
    /// 静摩擦系数（切向相对速度 < `SolverConfig::static_friction_speed` 时用）。
    ///
    /// 缺省等于 `friction`（只用老字段的行为完全不变）；想表达"推得动但推不住"
    /// 就把它设得比 `friction` 大（真实世界里木头/橡胶就是这么干的）。
    pub static_friction: f32,
    /// 摩擦系数的合成规则；`None` = 没指定，用世界的缺省规则。
    ///
    /// 为什么要区分"没指定"和"指定成几何平均"：两端都写了规则时得有个确定的
    /// 优先级，`None` 让"我只在一端写了 min"能生效（见 `ContactMaterial` 的解析）。
    pub friction_combine: Option<CombineRule>,
    /// 恢复系数的合成规则；`None` = 没指定，用世界的缺省规则。
    ///
    /// 世界的缺省是 `Max`：旧实现写死"取两个里大的"（`max(ea, eb)`），
    /// 换成平均会让所有球都跳得不一样，缺省必须保住逐位一致。
    pub restitution_combine: Option<CombineRule>,
    /// 线速度阻尼（每秒比例）。
    pub linear_damping: f32,
    /// 角速度阻尼。
    pub angular_damping: f32,
    /// 重力缩放。
    pub gravity_scale: f32,
    /// 上层挂载数据（例如实体 id）。
    pub user_data: u64,
    /// 碰撞过滤分组（缺省"和所有层碰"）。
    pub groups: InteractionGroups,
    /// 传感器（触发区）：参与接触检测与事件，但**不产生任何冲量**。
    ///
    /// 用它做"有人/物进入这个区域"的探测。传感器不会唤醒睡眠体（否则一个巨大的
    /// 触发区会把整个世界唤醒），这是刻意的取舍。
    pub sensor: bool,
    /// 是否对这一个刚体开启连续碰撞检测（CCD）。
    ///
    /// 实现是**中心射线 TOI**：每子步从刚体中心沿这一步的位移方向打一条射线，
    /// 命中就把朝表面的法向位移截到接触点前（切向保留）。不是精确 TOI：
    /// 只扫中心线、没有旋转扫掠，极快 + 极薄仍可能漏（见文档）。
    /// 代价是每个开了 CCD 的物体每子步一次射线查询，与速度无关。
    pub ccd: bool,

    /// 质量。
    pub(crate) mass: f32,
    /// 质量倒数（0 表示无限大）。
    pub(crate) inv_mass: f32,
    /// 绕质心的本地对角惯量。
    pub(crate) inertia_local: Vec3,
    /// 绕质心的本地对角惯量倒数（0 表示该轴无限大）。
    pub(crate) inv_inertia_local: Vec3,
    /// 质心在形状本地坐标系中的位置。
    pub(crate) local_center: Vec3,

    /// 本步累计的外力（积分后清空）。
    pub(crate) force: Vec3,
    /// 本步累计的外力矩。
    pub(crate) torque: Vec3,
    /// 是否处于睡眠。
    pub(crate) sleeping: bool,
    /// 低速持续时间计时器。
    pub(crate) sleep_timer: f32,
    /// 是否允许睡眠。
    pub(crate) allow_sleep: bool,
    /// 分裂冲量使用的伪线速度（只影响位置，不影响真实速度）。
    pub(crate) pseudo_linear_velocity: Vec3,
    /// 分裂冲量使用的伪角速度。
    pub(crate) pseudo_angular_velocity: Vec3,
}

impl RigidBody {
    /// 位姿。
    #[inline]
    pub fn transform(&self) -> Transform {
        self.transform
    }

    /// 设置位姿（会唤醒刚体）。
    pub fn set_transform(&mut self, t: Transform) {
        self.transform.position = t.position;
        self.transform.rotation = t.rotation.normalize_or_identity();
        self.transform.scale = Vec3::ONE;
        self.wake_up();
    }

    /// 世界位置（形状原点）。
    #[inline]
    pub fn position(&self) -> Vec3 {
        self.transform.position
    }

    /// 世界旋转。
    #[inline]
    pub fn rotation(&self) -> Quat {
        self.transform.rotation
    }

    /// 线速度。
    #[inline]
    pub fn linear_velocity(&self) -> Vec3 {
        self.linear_velocity
    }

    /// 角速度。
    #[inline]
    pub fn angular_velocity(&self) -> Vec3 {
        self.angular_velocity
    }

    /// 设置线速度并唤醒。
    pub fn set_linear_velocity(&mut self, v: Vec3) {
        if v.is_finite() {
            self.linear_velocity = v.clamp_length(MAX_LINEAR_SPEED);
            self.wake_up();
        }
    }

    /// 设置角速度并唤醒。
    pub fn set_angular_velocity(&mut self, w: Vec3) {
        if w.is_finite() {
            self.angular_velocity = w.clamp_length(MAX_ANGULAR_SPEED);
            self.wake_up();
        }
    }

    /// 施加持续外力（累积到本步结束，随后被积分并清空）。
    pub fn apply_force(&mut self, force: Vec3) {
        if force.is_finite() {
            self.force += force;
            self.wake_up();
        }
    }

    /// 施加冲量（作用于质心）。
    pub fn apply_impulse(&mut self, impulse: Vec3) {
        if !impulse.is_finite() {
            return;
        }
        self.linear_velocity =
            (self.linear_velocity + impulse * self.inv_mass).clamp_length(MAX_LINEAR_SPEED);
        self.wake_up();
    }

    /// 在世界点 `world_point` 施加冲量（产生力矩）。
    pub fn apply_impulse_at_point(&mut self, impulse: Vec3, world_point: Vec3) {
        if !impulse.is_finite() || !world_point.is_finite() {
            return;
        }
        self.apply_impulse(impulse);
        if self.inv_mass > 0.0 {
            let r = world_point - self.com_world();
            self.angular_velocity = (self.angular_velocity
                + self.apply_inv_inertia_world(r.cross(impulse)))
            .clamp_length(MAX_ANGULAR_SPEED);
        }
    }

    /// 施加角冲量。
    pub fn apply_torque_impulse(&mut self, t: Vec3) {
        if !t.is_finite() {
            return;
        }
        self.angular_velocity = (self.angular_velocity + self.apply_inv_inertia_world(t))
            .clamp_length(MAX_ANGULAR_SPEED);
        self.wake_up();
    }

    /// 质量。
    #[inline]
    pub fn mass(&self) -> f32 {
        self.mass
    }

    /// 质量倒数（0 表示无限大）。
    #[inline]
    pub fn inv_mass(&self) -> f32 {
        self.inv_mass
    }

    /// 绕质心的本地对角惯量。
    #[inline]
    pub fn inertia_local(&self) -> Vec3 {
        self.inertia_local
    }

    /// 质心的世界位置。
    #[inline]
    pub fn com_world(&self) -> Vec3 {
        self.transform.position + self.transform.rotation.mul_vec3(self.local_center)
    }

    /// 是否为动态刚体。
    #[inline]
    pub fn is_dynamic(&self) -> bool {
        self.body_type == BodyType::Dynamic
    }

    /// 是否为静态刚体。
    #[inline]
    pub fn is_static(&self) -> bool {
        self.body_type == BodyType::Static
    }

    /// 是否处于睡眠状态。
    #[inline]
    pub fn is_sleeping(&self) -> bool {
        self.sleeping
    }

    /// 是否允许睡眠。
    #[inline]
    pub fn can_sleep(&self) -> bool {
        self.allow_sleep
    }

    /// 设置是否允许睡眠。
    #[inline]
    pub fn set_can_sleep(&mut self, allow: bool) {
        self.allow_sleep = allow;
        if !allow {
            self.wake_up();
        }
    }

    /// 唤醒。
    pub fn wake_up(&mut self) {
        self.sleeping = false;
        self.sleep_timer = 0.0;
    }

    /// 立即进入睡眠（**速度清零**）。
    ///
    /// 语义与"自动睡眠"略有不同，写清楚免得踩：
    /// * 它是**立即**的，不看 `sleep_timer`、也不看同岛的其他刚体——而自动睡眠
    ///   要求"整岛都慢够时间"（逐体入睡会让堆叠慢慢渗进去，见 `solver::update_sleeping`）；
    /// * 它把线速度与角速度**清零**：一个正在飞的物体被叫去睡觉，醒来时是停着的。
    ///   只想"别再算了"但保留速度，请用 [`Self::set_linear_velocity`] 先记下来；
    /// * `allow_sleep == false` 的刚体**睡不住**：下一次 `step` 就会被 `update_sleeping`
    ///   重新置为清醒（"不允许睡眠"是硬承诺，手动 `sleep()` 也推翻不了它）；
    /// * 别人撞上来（或 [`PhysicsWorld::wake_up_all`]）会把它唤醒。
    pub fn sleep(&mut self) {
        self.sleeping = true;
        self.sleep_timer = 0.0;
        self.linear_velocity = Vec3::ZERO;
        self.angular_velocity = Vec3::ZERO;
    }

    /// 世界空间 AABB。
    pub fn world_aabb(&self) -> Aabb {
        let local = self.shape.local_aabb();
        if !local.is_valid() {
            return local; // 半空间
        }
        local.transformed_conservative(&self.transform.matrix())
    }

    /// 世界空间某点的速度：`v + ω × (p - com)`。
    pub fn velocity_at_point(&self, world_point: Vec3) -> Vec3 {
        let r = world_point - self.com_world();
        self.linear_velocity + self.angular_velocity.cross(r)
    }

    /// 用世界逆惯量张量作用一个向量：`I⁻¹v = R · diag(inv) · Rᵀ · v`。
    #[inline]
    pub(crate) fn apply_inv_inertia_world(&self, v: Vec3) -> Vec3 {
        let r = self.transform.rotation;
        let local = r.inverse().mul_vec3(v) * self.inv_inertia_local;
        r.mul_vec3(local)
    }

    /// NaN / 无穷 防御：把非法状态复位。
    pub(crate) fn sanitize(&mut self) {
        if !self.transform.position.is_finite() {
            self.transform.position = Vec3::ZERO;
        }
        if !self.transform.rotation.is_finite() || self.transform.rotation.length_squared() < 1e-8 {
            self.transform.rotation = Quat::IDENTITY;
        } else {
            self.transform.rotation = self.transform.rotation.normalize_or_identity();
        }
        if !self.linear_velocity.is_finite() {
            self.linear_velocity = Vec3::ZERO;
        } else {
            self.linear_velocity = self.linear_velocity.clamp_length(MAX_LINEAR_SPEED);
        }
        if !self.angular_velocity.is_finite() {
            self.angular_velocity = Vec3::ZERO;
        } else {
            self.angular_velocity = self.angular_velocity.clamp_length(MAX_ANGULAR_SPEED);
        }
        if !self.force.is_finite() {
            self.force = Vec3::ZERO;
        }
        if !self.torque.is_finite() {
            self.torque = Vec3::ZERO;
        }
        if !self.pseudo_linear_velocity.is_finite() {
            self.pseudo_linear_velocity = Vec3::ZERO;
        }
        if !self.pseudo_angular_velocity.is_finite() {
            self.pseudo_angular_velocity = Vec3::ZERO;
        }
    }
}

/// 刚体构建器。
pub struct BodyBuilder {
    shape: Shape,
    body_type: BodyType,
    transform: Transform,
    linear_velocity: Vec3,
    angular_velocity: Vec3,
    density: f32,
    mass_override: Option<f32>,
    restitution: f32,
    friction: f32,
    /// `None` = 跟随 `friction`（在 `build()` 里定下来）。
    static_friction: Option<f32>,
    friction_combine: Option<CombineRule>,
    restitution_combine: Option<CombineRule>,
    linear_damping: f32,
    angular_damping: f32,
    gravity_scale: f32,
    user_data: u64,
    allow_sleep: bool,
    groups: InteractionGroups,
    sensor: bool,
    ccd: bool,
}

impl BodyBuilder {
    fn new(shape: Shape, body_type: BodyType) -> Self {
        Self {
            shape,
            body_type,
            transform: Transform::IDENTITY,
            linear_velocity: Vec3::ZERO,
            angular_velocity: Vec3::ZERO,
            density: 1000.0,
            mass_override: None,
            restitution: 0.0,
            friction: 0.5,
            static_friction: None,
            friction_combine: None,
            restitution_combine: None,
            linear_damping: 0.0,
            angular_damping: 0.0,
            gravity_scale: 1.0,
            user_data: 0,
            allow_sleep: true,
            groups: InteractionGroups::default(),
            sensor: false,
            ccd: false,
        }
    }

    /// 动态刚体。
    pub fn dynamic(shape: Shape) -> Self {
        Self::new(shape, BodyType::Dynamic)
    }

    /// 静态刚体。
    pub fn r#static(shape: Shape) -> Self {
        Self::new(shape, BodyType::Static)
    }

    /// 运动学刚体。
    pub fn kinematic(shape: Shape) -> Self {
        Self::new(shape, BodyType::Kinematic)
    }

    /// 位置。
    pub fn position(mut self, p: Vec3) -> Self {
        if p.is_finite() {
            self.transform.position = p;
        }
        self
    }

    /// 旋转。
    pub fn rotation(mut self, q: Quat) -> Self {
        if q.is_finite() {
            self.transform.rotation = q.normalize_or_identity();
        }
        self
    }

    /// 位姿。
    pub fn transform(mut self, t: Transform) -> Self {
        if t.position.is_finite() {
            self.transform.position = t.position;
        }
        if t.rotation.is_finite() {
            self.transform.rotation = t.rotation.normalize_or_identity();
        }
        self
    }

    /// 初始线速度。
    pub fn linear_velocity(mut self, v: Vec3) -> Self {
        if v.is_finite() {
            self.linear_velocity = v;
        }
        self
    }

    /// 初始角速度。
    pub fn angular_velocity(mut self, w: Vec3) -> Self {
        if w.is_finite() {
            self.angular_velocity = w;
        }
        self
    }

    /// 密度（默认 1000，即水的密度）。
    pub fn density(mut self, d: f32) -> Self {
        if d.is_finite() && d >= 0.0 {
            self.density = d;
        }
        self
    }

    /// 直接指定质量（会按比例缩放惯量）。
    pub fn mass(mut self, m: f32) -> Self {
        if m.is_finite() && m > 0.0 {
            self.mass_override = Some(m);
        }
        self
    }

    /// 恢复系数，夹紧到 [0, 1]。
    pub fn restitution(mut self, e: f32) -> Self {
        if e.is_finite() {
            self.restitution = clamp(e, 0.0, 1.0);
        }
        self
    }

    /// 摩擦系数（非负）。
    pub fn friction(mut self, f: f32) -> Self {
        if f.is_finite() {
            self.friction = f.max(0.0);
        }
        self
    }

    /// 静摩擦系数（非负）；不给就跟随 [`Self::friction`]。
    ///
    /// 只在"切向相对速度足够小"时生效（阈值见 `SolverConfig::static_friction_speed`）。
    /// 表现为"推得动、但自己不会慢慢滑"。
    pub fn static_friction(mut self, f: f32) -> Self {
        if f.is_finite() {
            self.static_friction = Some(f.max(0.0));
        }
        self
    }

    /// 指定摩擦系数的合成规则（不指定就用世界的缺省）。
    pub fn friction_combine(mut self, rule: CombineRule) -> Self {
        self.friction_combine = Some(rule);
        self
    }

    /// 指定恢复系数的合成规则（不指定就用世界的缺省）。
    pub fn restitution_combine(mut self, rule: CombineRule) -> Self {
        self.restitution_combine = Some(rule);
        self
    }

    /// 线速度阻尼。
    pub fn linear_damping(mut self, d: f32) -> Self {
        if d.is_finite() {
            self.linear_damping = d.max(0.0);
        }
        self
    }

    /// 角速度阻尼。
    pub fn angular_damping(mut self, d: f32) -> Self {
        if d.is_finite() {
            self.angular_damping = d.max(0.0);
        }
        self
    }

    /// 重力缩放。
    pub fn gravity_scale(mut self, s: f32) -> Self {
        if s.is_finite() {
            self.gravity_scale = s;
        }
        self
    }

    /// 挂载数据。
    pub fn user_data(mut self, v: u64) -> Self {
        self.user_data = v;
        self
    }

    /// 是否允许自动睡眠。
    pub fn can_sleep(mut self, allow: bool) -> Self {
        self.allow_sleep = allow;
        self
    }

    /// 碰撞过滤分组。
    pub fn groups(mut self, groups: InteractionGroups) -> Self {
        self.groups = groups;
        self
    }

    /// 只在第 `layer` 层，且和所有层碰。
    pub fn layer(mut self, layer: u8) -> Self {
        self.groups.memberships = 1u32 << (layer & 31);
        self
    }

    /// 只和 `filter` 掩码里的层碰。
    pub fn collides_with(mut self, filter: u32) -> Self {
        self.groups.filter = filter;
        self
    }

    /// 传感器（触发区）：参与接触检测与事件，但不产生冲量。
    pub fn sensor(mut self, sensor: bool) -> Self {
        self.sensor = sensor;
        self
    }

    /// 开启连续碰撞检测（中心射线 TOI）。
    pub fn ccd(mut self, ccd: bool) -> Self {
        self.ccd = ccd;
        self
    }

    /// 构建刚体。
    pub fn build(self) -> RigidBody {
        let mut body_type = self.body_type;
        // 半空间/三角网格只能是静态体。
        if self.shape.is_static_only() {
            body_type = BodyType::Static;
        }
        let mp: MassProperties = self.shape.mass_properties(self.density);
        let mut mass = mp.mass;
        let mut inertia = mp.inertia;
        if let Some(m) = self.mass_override {
            let scale = if mp.mass > 1e-9 { m / mp.mass } else { 0.0 };
            if scale > 0.0 {
                mass = m;
                inertia = mp.inertia * scale;
            }
        }
        let (inv_mass, inv_inertia_local) = match body_type {
            BodyType::Dynamic => (
                safe_inv(mass),
                Vec3::new(
                    safe_inv(inertia.x),
                    safe_inv(inertia.y),
                    safe_inv(inertia.z),
                ),
            ),
            _ => (0.0, Vec3::ZERO),
        };
        RigidBody {
            body_type,
            shape: self.shape,
            transform: Transform::new(
                self.transform.position,
                self.transform.rotation.normalize_or_identity(),
                Vec3::ONE,
            ),
            linear_velocity: self.linear_velocity.clamp_length(MAX_LINEAR_SPEED),
            angular_velocity: self.angular_velocity.clamp_length(MAX_ANGULAR_SPEED),
            restitution: self.restitution,
            friction: self.friction,
            static_friction: self.static_friction.unwrap_or(self.friction),
            friction_combine: self.friction_combine,
            restitution_combine: self.restitution_combine,
            linear_damping: self.linear_damping,
            angular_damping: self.angular_damping,
            gravity_scale: self.gravity_scale,
            user_data: self.user_data,
            groups: self.groups,
            sensor: self.sensor,
            ccd: self.ccd,
            mass,
            inv_mass,
            inertia_local: inertia,
            inv_inertia_local,
            local_center: if mp.center.is_finite() {
                mp.center
            } else {
                Vec3::ZERO
            },
            force: Vec3::ZERO,
            torque: Vec3::ZERO,
            sleeping: false,
            sleep_timer: 0.0,
            allow_sleep: self.allow_sleep && body_type == BodyType::Dynamic,
            pseudo_linear_velocity: Vec3::ZERO,
            pseudo_angular_velocity: Vec3::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere() -> Shape {
        Shape::Sphere { radius: 0.5 }
    }

    #[test]
    fn builder_sets_mass_from_density() {
        let b = BodyBuilder::dynamic(sphere()).density(1000.0).build();
        let expected = 4.0 / 3.0 * core::f32::consts::PI * 0.125 * 1000.0;
        assert!((b.mass() - expected).abs() < 1e-2);
        assert!((b.inv_mass() - 1.0 / expected).abs() < 1e-5);
        assert!(b.is_dynamic());
    }

    #[test]
    fn mass_override_scales_inertia() {
        let a = BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .density(1000.0)
        .build();
        let b = BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .mass(10.0)
        .build();
        assert!((b.mass() - 10.0).abs() < 1e-5);
        let ratio = b.inertia_local().x / a.inertia_local().x;
        assert!((ratio - 10.0 / a.mass()).abs() < 1e-3);
    }

    #[test]
    fn static_body_has_infinite_mass() {
        let b = BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build();
        assert_eq!(b.body_type, BodyType::Static);
        assert_eq!(b.inv_mass(), 0.0);
        assert_eq!(b.apply_inv_inertia_world(Vec3::Y), Vec3::ZERO);
    }

    #[test]
    fn dynamic_plane_is_forced_static() {
        let b = BodyBuilder::dynamic(Shape::Plane { normal: Vec3::Y }).build();
        assert_eq!(b.body_type, BodyType::Static);
    }

    #[test]
    fn impulse_changes_velocity() {
        let mut b = BodyBuilder::dynamic(sphere()).mass(2.0).build();
        b.apply_impulse(vec3(4.0, 0.0, 0.0));
        assert!((b.linear_velocity().x - 2.0).abs() < 1e-5);
        b.apply_impulse_at_point(vec3(0.0, 4.0, 0.0), vec3(0.5, 0.0, 0.0));
        // 力臂 +X × +Y → +Z
        assert!(b.angular_velocity().z > 0.0);
    }

    #[test]
    fn velocity_at_point_uses_angular_term() {
        let mut b = BodyBuilder::dynamic(sphere()).mass(1.0).build();
        b.set_angular_velocity(vec3(0.0, 0.0, 2.0));
        let v = b.velocity_at_point(vec3(1.0, 0.0, 0.0));
        // ω × r = (0,0,2) × (1,0,0) = (0,2,0)
        assert!((v.y - 2.0).abs() < 1e-5, "{v:?}");
    }
}
