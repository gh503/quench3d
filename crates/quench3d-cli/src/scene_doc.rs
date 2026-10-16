//! 场景文件：`quench3d.scene` —— 面向 agent / LLM 的声明式场景描述。
//!
//! 设计目标：
//!
//! * **能表达引擎的全部常用能力**：图元、材质、刚体、关节、模型文件、相机、光照、仿真步进、渲染参数。
//! * **人可读、LLM 可写**：字段名直白，单位明确（米、秒、角度用度），缺省值友好（`{}` 就是合法场景）。
//! * **可校验**：`validate` 返回带 JSON 路径的结构化诊断，agent 可以据此自我修正。
//!
//! 最小合法场景就是一个空对象：
//!
//! ```json
//! {}
//! ```
//!
//! 典型场景：
//!
//! ```json
//! {
//!   "name": "堆叠塔",
//!   "ground": { "size": 40 },
//!   "materials": { "wood": { "color": [0.75, 0.45, 0.25], "roughness": 0.6 } },
//!   "objects": [
//!     { "name": "塔", "shape": { "type": "box", "size": [1, 1, 1] },
//!       "position": [0, 0.5, 0], "material": "wood", "body": "dynamic" }
//!   ],
//!   "camera": { "position": [6, 4, 8], "look_at": [0, 1, 0] },
//!   "simulate": { "steps": 120 },
//!   "render": { "output": "tower.png", "width": 900, "height": 600 }
//! }
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use quench3d::prelude::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 单个场景允许的最大物体数（超过就给出诊断，而不是跑到 OOM）。
pub const MAX_OBJECTS: usize = 20_000;
/// 单次 `simulate` 允许的最大步数（按 1/60 秒算约 4.6 小时仿真）。
pub const MAX_STEPS: u32 = 1_000_000;
/// 同时参与着色的点光源上限（直接引用渲染器的常量，避免两处上限漂移）。
pub const MAX_POINT_LIGHTS: usize = quench3d::render::MAX_POINT_LIGHTS;
/// 单个场景允许的最大定时动作数。
pub const MAX_ACTIONS: usize = 10_000;
/// 单个场景允许的最大区域探针数（每步都要对每个探针做一次区域查询）。
pub const MAX_PROBES: usize = 256;

/// 场景文件格式标识。
pub const FORMAT: &str = "quench3d.scene";
/// 当前支持的格式版本。
///
/// 版本 2 起 `objects[].parent` 生效：`position` / `rotation_deg` / `scale` 的含义从
/// "世界"变成"相对父物体"（没有父物体时仍是世界）。**不做版本 1 的兼容** —— 这个格式
/// 还没有外部使用者，与其维护一个迁移器，不如让版本 1 的文件拿到一条明确的诊断，
/// 而不是被静默按新语义解释、摆出一堆位置错误的物体。
pub const VERSION: u32 = 2;

/// 诊断级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 错误：无法构建场景。
    Error,
    /// 警告：可以构建，但结果可能不是作者想要的。
    Warning,
}

/// 一条校验诊断。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 级别。
    pub severity: Severity,
    /// JSON 路径，例如 `objects[2].shape.size`。
    pub path: String,
    /// 人可读说明。
    pub message: String,
    /// 可选的修复建议。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Diagnostic {
    /// 造一条错误诊断。
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
            hint: None,
        }
    }

    /// 造一条警告诊断。
    pub fn warn(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
            hint: None,
        }
    }

    /// 附上修复建议。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 一个场景文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDoc {
    /// 格式标识，缺省 `quench3d.scene`。
    #[serde(default = "default_format")]
    pub format: String,
    /// 格式版本，当前为 1。
    #[serde(default = "default_version")]
    pub version: u32,
    /// 场景名（仅用于展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 重力加速度，缺省 `[0, -9.81, 0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity: Option<[f32; 3]>,
    /// 世界级物理参数（缺省即引擎缺省；**不写 = 行为与以前逐位相同**）。
    #[serde(default)]
    pub physics: PhysicsDoc,
    /// 地面：静态平面 + 可视平面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<GroundDoc>,
    /// 具名材质。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub materials: BTreeMap<String, MaterialDoc>,
    /// 图元物体。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<ObjectDoc>,
    /// 模板：可复用的一段物体描述（`{ "箱子": { "shape": {...}, "body": "dynamic" } }`）。
    ///
    /// 用与物体**完全相同的字段集**承载（多一个字段都会是拼写错误的隐患）；
    /// `name` / `parent` / `position` / `rotation_deg` 这几项在模板里没有意义，
    /// 写了会得到一条警告并被忽略。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub templates: BTreeMap<String, ObjectDoc>,
    /// 外部模型文件（glTF/GLB/OBJ）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelDoc>,
    /// 资源搜索路径（贴图与模型的"资源库"）。
    ///
    /// **不写 = 只按原有规则找**（场景文件所在目录 → 当前工作目录 → 引擎搜索路径），
    /// 与没有这一节时完全一致。写了就在"场景目录"之后、"当前工作目录"之前依次尝试。
    ///
    /// 相对路径以**场景文件所在目录**为基准（和 `models[].path`、贴图路径同一套规则），
    /// 绝对路径原样使用。这样把素材放在 `../shared/assets/` 里，多个场景可以共用，
    /// 且不管从哪个工作目录跑都找得到。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<AssetsDoc>,
    /// 关节。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<JointDoc>,
    /// 定时动作：在指定步/秒对刚体施加冲量、力、速度或瞬移。
    ///
    /// **不写这个字段 = 不施加任何作用**，物理结果与没有这个字段时逐位一致。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ActionDoc>,
    /// 关键帧动画：按时间推着物体走（缺省空 = 行为与今天逐位一致）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<AnimationDoc>,
    /// 区域探针：只读地记录"谁在什么时候进了这个区域"。
    ///
    /// 探针只观测、不参与物理（不会唤醒睡眠刚体、不会产生接触），
    /// 因此加了探针的场景与不加探针的场景物理结果逐位一致。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeDoc>,
    /// 相机。
    #[serde(default)]
    pub camera: CameraDoc,
    /// 命名相机：`--camera <名字>` 用来选一台渲染（多个视角一次说清）。
    ///
    /// 缺省空 = 行为与今天完全一致（只渲 `camera` 段这台）。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cameras: BTreeMap<String, CameraDoc>,
    /// 光照与天空。
    #[serde(default)]
    pub lighting: LightingDoc,
    /// 物理步进。
    #[serde(default)]
    pub simulate: SimulateDoc,
    /// 渲染输出。
    #[serde(default)]
    pub render: RenderDoc,
    /// 场景文件所在目录（**不参与序列化**，由 [`SceneDoc::from_file`] 填入）。
    ///
    /// 材质里的贴图相对路径以它为基准解析，这样 `render maps/level.json` 在任意
    /// 工作目录下都能找到 `maps/wood.png`。从字符串解析（`from_json`/`from_value`）
    /// 时是 `None`，解析退化为"相对当前工作目录 + 引擎搜索路径"。
    #[serde(skip)]
    pub base_dir: Option<PathBuf>,
}

/// 世界级物理参数。
///
/// 这里的每一项都有"引擎缺省"，**不写就是缺省**（且与这一节不存在时逐位相同）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicsDoc {
    /// 摩擦系数的**缺省**合成规则（两端都没显式指定时用），缺省 `geometric_mean`。
    ///
    /// 这个缺省是历史行为：`sqrt(fa × fb)`。想改成"取小/取大/平均/相乘"就在这里写，
    /// 或者只在某个物体上写 `friction_combine`（显式优先于这里的缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_combine: Option<CombineRuleDoc>,
    /// 恢复系数的**缺省**合成规则，缺省 `max`（历史行为是取两个里大的）。
    ///
    /// 换成 `average` 会让同一批球跳得不一样高——这是**观感变化**，不是 bug 修复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restitution_combine: Option<CombineRuleDoc>,
    /// 切向相对速度低于多少（m/s）算"静止接触"、改用静摩擦系数，缺省 0.02。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_friction_speed: Option<f32>,
    /// 接触余量（米）：分离但距离小于它的形状对也生成接触点，让高速物体"即将接触"时
    /// 就进入约束求解（少一帧穿透突跳）。缺省 0 = 只在重叠时生成接触（历史行为，结果
    /// 逐位不变）。只作用于**单形状**刚体：复合形状不膨胀（见文档边界）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speculative_margin: Option<f32>,
}

/// 摩擦/恢复系数的合成规则（场景文件里的写法）。
///
/// 与引擎里的 [`CombineRule`] 一一对应；分开定义是因为引擎 crate 不依赖 serde
/// （解析层的东西不该渗进物理内核）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineRuleDoc {
    /// 几何平均 `sqrt(a×b)`（缺省）。
    GeometricMean,
    /// 算术平均 `(a+b)/2`。
    Average,
    /// 取小。
    Min,
    /// 取大。
    Max,
    /// 相乘 `a×b`。
    Multiply,
}

impl From<CombineRule> for CombineRuleDoc {
    fn from(rule: CombineRule) -> Self {
        match rule {
            CombineRule::GeometricMean => CombineRuleDoc::GeometricMean,
            CombineRule::Average => CombineRuleDoc::Average,
            CombineRule::Min => CombineRuleDoc::Min,
            CombineRule::Max => CombineRuleDoc::Max,
            CombineRule::Multiply => CombineRuleDoc::Multiply,
        }
    }
}

impl From<CombineRuleDoc> for CombineRule {
    fn from(rule: CombineRuleDoc) -> Self {
        match rule {
            CombineRuleDoc::GeometricMean => CombineRule::GeometricMean,
            CombineRuleDoc::Average => CombineRule::Average,
            CombineRuleDoc::Min => CombineRule::Min,
            CombineRuleDoc::Max => CombineRule::Max,
            CombineRuleDoc::Multiply => CombineRule::Multiply,
        }
    }
}

impl MaterialDoc {
    /// 四个贴图槽位：`(JSON 字段名, 路径, 是否按 sRGB 采样)`。
    ///
    /// 校验与加载共用这一份清单，避免"校验看了 4 个、加载只读了 3 个"这种漂移。
    /// 基础色与自发光贴图是颜色贴图（sRGB），法线与金属度-粗糙度贴图是数据贴图（线性）。
    pub fn textures(&self) -> [(&'static str, Option<&String>, bool); 4] {
        [
            ("base_color_texture", self.base_color_texture.as_ref(), true),
            ("normal_texture", self.normal_texture.as_ref(), false),
            (
                "metallic_roughness_texture",
                self.metallic_roughness_texture.as_ref(),
                false,
            ),
            ("emissive_texture", self.emissive_texture.as_ref(), true),
        ]
    }
}

fn default_format() -> String {
    FORMAT.to_string()
}

fn default_version() -> u32 {
    VERSION
}
/// 一条关键帧轨道：`times`（秒，严格递增）与 `values` 一一对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackDoc {
    /// 关键帧时间（秒），严格递增。
    pub times: Vec<f32>,
    /// 关键帧取值：位置是米；旋转是欧拉角 XYZ（度）。
    pub values: Vec<[f32; 3]>,
}

/// 关键帧动画：按时间推着某个物体走（**运动学**：每步直接设变换，不是施加力）。
///
/// `position` / `rotation` 至少给一条。`rotation` 的关键帧是欧拉角 XYZ（度），
/// 采样时转成四元数做 slerp（所以不会出现"绕远路"的中间姿态）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnimationDoc {
    /// 名字（只用于诊断；场景里唯一）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 目标刚体名（`objects[].name`）。
    pub target: String,
    /// 从第几秒开始（缺省 0）。开始之前保持第一条关键帧的姿态。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<f32>,
    /// 播放倍速（缺省 1，必须 > 0）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// 是否循环（缺省 false：播完停在最后一帧）。
    #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
    pub looping: Option<bool>,
    /// 混合权重（0~1，缺省 1）。
    ///
    /// 语义是"从**当前姿态**朝这条动画的姿态走多远"（`0` = 完全不生效，`1` = 完全生效）。
    /// 同一个人物挂两条动画时，后一条的权重就是两条 clip 之间的混合比例：
    /// 前一条（权重 1）定下姿态，后一条权重 `w` 给出 `lerp(前, 后, w)`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
    /// 位置轨道（米）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<TrackDoc>,
    /// 朝向轨道（欧拉角 XYZ，度）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<TrackDoc>,
}

impl SceneDoc {
    /// 物体名（缺省 `objects[i]`）。
    pub fn object_name_at(&self, index: usize) -> String {
        self.objects
            .get(index)
            .and_then(|o| o.name.clone())
            .unwrap_or_else(|| format!("objects[{index}]"))
    }

    /// 展开模板与实例：返回一个新文档，其中每个实例都是**一个具体物体**。
    ///
    /// 展开规则（这三条是这一节的全部语义）：
    /// 1. `objects[].template` 把模板的字段并进来，**物体自己写的字段赢**；
    ///    物体只要写了 `shape` 或 `shapes` 中的任意一个，形状就整个用自己的
    ///    （不把"模板的 shape + 物体的 shapes"拼在一起，那必然报错且没人想要）。
    /// 2. `instances[k]` 的 `position` / `rotation_deg` / `scale` **覆盖**物体上的同名值
    ///    （"每个副本差在哪"写在副本上），没写的沿用物体。
    /// 3. 名字：实例写了 `name` 用它；只有一个实例且没写名字 → 用物体本名
    ///    （这样关节/动作引用老名字仍然有效）；多个实例 → `本名#1`、`本名#2`…
    ///
    /// 名字重复、模板不存在这类问题由 [`SceneDoc::validate`] 报错；这里是"能展开就展开"，
    /// 不 panic（漏掉模板 = 只展开物体自己的字段）。
    pub fn expanded(&self) -> SceneDoc {
        if self.templates.is_empty()
            && self
                .objects
                .iter()
                .all(|o| o.template.is_none() && o.instances.is_none())
        {
            return self.clone();
        }
        let mut out = self.clone();
        out.templates.clear();
        let mut objects: Vec<ObjectDoc> = Vec::with_capacity(self.objects.len());
        for (i, o) in self.objects.iter().enumerate() {
            let mut merged = match o.template.as_deref().and_then(|n| self.templates.get(n)) {
                Some(t) => merge_object(o, t),
                None => {
                    let mut m = o.clone();
                    m.template = None;
                    m
                }
            };
            match o.instances.as_deref() {
                Some(list) if !list.is_empty() => {
                    let base = self.object_name_at(i);
                    for (k, inst) in list.iter().enumerate() {
                        let mut copy = merged.clone();
                        copy.instances = None;
                        if inst.position.is_some() {
                            copy.position = inst.position;
                        }
                        if inst.rotation_deg.is_some() {
                            copy.rotation_deg = inst.rotation_deg;
                        }
                        if inst.scale.is_some() {
                            copy.scale = inst.scale;
                        }
                        copy.name = Some(match (&inst.name, list.len()) {
                            (Some(n), _) => n.clone(),
                            (None, 1) => base.clone(),
                            (None, _) => format!("{base}#{}", k + 1),
                        });
                        objects.push(copy);
                    }
                }
                _ => {
                    merged.instances = None;
                    merged.name = Some(self.object_name_at(i));
                    objects.push(merged);
                }
            }
        }
        out.objects = objects;
        out
    }
}

/// 模板合并（物体的显式字段赢）。字段与 [`ObjectDoc`] 一一对应，改这里时要一起看。
fn merge_object(object: &ObjectDoc, template: &ObjectDoc) -> ObjectDoc {
    let o = object;
    let t = template;
    // 形状整体二选一（详见 `expanded` 的说明）
    let (shape, shapes) = if o.shape.is_some() || o.shapes.is_some() {
        (o.shape.clone(), o.shapes.clone())
    } else {
        (t.shape.clone(), t.shapes.clone())
    };
    ObjectDoc {
        // 身份与位姿不继承模板：模板只描述"这类东西长什么样"
        name: o.name.clone(),
        template: None,
        instances: None,
        parent: o.parent.clone(),
        position: o.position,
        rotation_deg: o.rotation_deg,
        shape,
        shapes,
        // `body` 的缺省值就是 `none`（纯显示），"没写"和"写 none"分不出来：
        // 因此物体上的 `none` 覆盖不了模板的 dynamic —— 要覆盖就显式写别的刚体类型。
        body: if o.body == BodyKind::None {
            t.body
        } else {
            o.body
        },
        material: o.material.clone().or_else(|| t.material.clone()),
        density: o.density.or(t.density),
        restitution: o.restitution.or(t.restitution),
        friction: o.friction.or(t.friction),
        static_friction: o.static_friction.or(t.static_friction),
        friction_combine: o.friction_combine.or(t.friction_combine),
        restitution_combine: o.restitution_combine.or(t.restitution_combine),
        velocity: o.velocity.or(t.velocity),
        angular_velocity: o.angular_velocity.or(t.angular_velocity),
        linear_damping: o.linear_damping.or(t.linear_damping),
        angular_damping: o.angular_damping.or(t.angular_damping),
        gravity_scale: o.gravity_scale.or(t.gravity_scale),
        mass: o.mass.or(t.mass),
        allow_sleep: o.allow_sleep.or(t.allow_sleep),
        // 可见性取"与"：模板说不可见就不会被物体默认的 true 翻回来
        visible: o.visible && t.visible,
        scale: o.scale.or(t.scale),
        collision_layer: o.collision_layer.or(t.collision_layer),
        collides_with: o.collides_with.clone().or_else(|| t.collides_with.clone()),
        sensor: o.sensor.or(t.sensor),
        ccd: o.ccd.or(t.ccd),
    }
}

impl SceneDoc {
    /// 模板与实例的**结构**检查（不检查物体字段本身，那些在展开后的文档上做）。
    fn validate_template_layer(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        // 模板自身：不能带身份/位姿字段
        for (name, t) in &self.templates {
            let p = format!("templates.{name}");
            if t.name.is_some() {
                out.push(Diagnostic::warn(
                    format!("{p}.name"),
                    "模板里的 name 没有意义（名字属于物体/实例），会被忽略",
                ));
            }
            if t.parent.is_some() || t.position.is_some() || t.rotation_deg.is_some() {
                out.push(Diagnostic::warn(
                    &p,
                    "模板里的 parent / position / rotation_deg 没有意义（模板只描述\"长什么样\"），会被忽略；位置请写在 instances 或物体上",
                ));
            }
            if t.template.is_some() || t.instances.is_some() {
                out.push(Diagnostic::warn(
                    &p,
                    "模板里不能再写 template / instances（模板不支持嵌套），会被忽略",
                ));
            }
        }
        // 物体：模板引用、实例表
        for (i, o) in self.objects.iter().enumerate() {
            let p = format!("objects[{i}]");
            if let Some(t) = &o.template {
                if !self.templates.contains_key(t) {
                    let mut known: Vec<&str> = self.templates.keys().map(|k| k.as_str()).collect();
                    known.sort_unstable();
                    let hint = if known.is_empty() {
                        "这个场景还没有 templates 一节".to_string()
                    } else {
                        format!("已定义的模板：{}", known.join("、"))
                    };
                    out.push(
                        Diagnostic::error(format!("{p}.template"), format!("找不到模板 `{t}`"))
                            .with_hint(hint),
                    );
                }
            }
            match o.instances.as_deref() {
                Some([]) => out.push(Diagnostic::error(
                    format!("{p}.instances"),
                    "instances 不能是空数组（要么删掉，要么至少写一个副本）",
                )),
                Some(list) => {
                    // 实例多起来是实打实的物理开销（每个副本一个独立刚体，不是 GPU 实例化）
                    if list.len() > 64 {
                        out.push(Diagnostic::warn(
                            format!("{p}.instances"),
                            format!(
                                "{} 个副本会变成 {} 个独立刚体（每个都参与物理），确认这是想要的",
                                list.len(),
                                list.len()
                            ),
                        ));
                    }
                    for (k, inst) in list.iter().enumerate() {
                        let ip = format!("{p}.instances[{k}]");
                        if let Some(v) = &inst.position {
                            if !finite3(v) {
                                out.push(Diagnostic::error(
                                    format!("{ip}.position"),
                                    "实例位置必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &inst.rotation_deg {
                            if !finite3(v) {
                                out.push(Diagnostic::error(
                                    format!("{ip}.rotation_deg"),
                                    "实例欧拉角必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &inst.scale {
                            if !finite3(v) || v.iter().any(|x| *x <= 0.0) {
                                out.push(Diagnostic::error(
                                    format!("{ip}.scale"),
                                    "实例缩放必须有限且为正",
                                ));
                            }
                        }
                    }
                }
                None => {}
            }
        }
        // 未使用的模板
        let mut used: BTreeMap<&str, usize> = BTreeMap::new();
        for o in &self.objects {
            if let Some(t) = o.template.as_deref() {
                *used.entry(t).or_insert(0) += 1;
            }
        }
        for name in self.templates.keys() {
            if !used.contains_key(name.as_str()) {
                out.push(Diagnostic::warn(
                    format!("templates.{name}"),
                    "这个模板没有被任何物体引用（白写一节；确认不是漏了 template 字段）",
                ));
            }
        }
        // 展开后的名字必须唯一：实例名字缺省是 `本名#序号`，与本名或别的物体可能撞车
        let expanded = self.expanded();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (k, o) in expanded.objects.iter().enumerate() {
            let name = o.name.clone().unwrap_or_else(|| format!("objects[{k}]"));
            *seen.entry(name).or_insert(0) += 1;
        }
        let mut dups: Vec<&String> = seen
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(k, _)| k)
            .collect();
        dups.sort();
        for d in dups {
            let mut where_: Vec<String> = Vec::new();
            for (i, o) in self.objects.iter().enumerate() {
                let base = self.object_name_at(i);
                if o.instances.as_deref().is_some_and(|l| !l.is_empty()) {
                    for (k, inst) in o.instances.as_deref().unwrap_or(&[]).iter().enumerate() {
                        let n = match (&inst.name, o.instances.as_deref().map(|l| l.len())) {
                            (Some(n), _) => n.clone(),
                            (None, Some(1)) => base.clone(),
                            (None, _) => format!("{base}#{}", k + 1),
                        };
                        if &n == d {
                            where_.push(format!("objects[{i}].instances[{k}]"));
                        }
                    }
                } else if &base == d {
                    where_.push(format!("objects[{i}]"));
                }
            }
            out.push(
                Diagnostic::error(
                    where_.first().cloned().unwrap_or_else(|| "objects".into()),
                    format!("名字 `{d}` 重复（实例展开后每个物体的名字必须唯一）"),
                )
                .with_hint("给副本写 name，或改掉重名的物体"),
            );
        }
        out
    }
}

impl Default for SceneDoc {
    fn default() -> Self {
        Self {
            format: default_format(),
            cameras: BTreeMap::new(),
            animations: Vec::new(),
            version: VERSION,
            name: None,
            gravity: None,
            physics: PhysicsDoc::default(),
            ground: None,
            materials: BTreeMap::new(),
            objects: Vec::new(),
            templates: BTreeMap::new(),
            models: Vec::new(),
            assets: None,
            joints: Vec::new(),
            actions: Vec::new(),
            probes: Vec::new(),
            camera: CameraDoc::default(),
            lighting: LightingDoc::default(),
            simulate: SimulateDoc::default(),
            render: RenderDoc::default(),
            base_dir: None,
        }
    }
}

/// 地面配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundDoc {
    /// 边长（米），缺省 40。
    #[serde(default = "default_ground_size")]
    pub size: f32,
    /// 地面材质名（引用 `materials`），缺省用内置灰。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// 高度（米），缺省 0。
    #[serde(default)]
    pub y: f32,
    /// 地面摩擦系数，缺省 0.85。
    #[serde(default = "default_ground_friction")]
    pub friction: f32,
    /// 地面恢复系数（弹性），缺省 0。
    #[serde(default)]
    pub restitution: f32,
}

fn default_ground_size() -> f32 {
    40.0
}

fn default_ground_friction() -> f32 {
    0.85
}

/// 材质（线性或 sRGB 由 `srgb` 控制，缺省按 sRGB 解释，符合直觉）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDoc {
    /// 基础色 `[r, g, b]`，0~1，按 sRGB 解释。
    pub color: [f32; 3],
    /// 金属度 0~1，缺省 0。
    #[serde(default)]
    pub metallic: f32,
    /// 粗糙度 0~1，缺省 0.5。
    #[serde(default = "default_roughness")]
    pub roughness: f32,
    /// 自发光强度，缺省 0。
    #[serde(default)]
    pub emissive: f32,
    /// 不透明度 0~1，缺省 1（`< 1` 会自动走混合管线）。
    ///
    /// 贴图自带 alpha 时用 `alpha_blend` 显式开启，别指望这个标量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha: Option<f32>,
    /// 自发光颜色 `[r, g, b]`，缺省用 `color`。
    ///
    /// 与 `color` 一样按 **sRGB**（屏幕上看得到的颜色）给出，会转成线性空间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_color: Option<[f32; 3]>,
    /// UV 平铺次数（> 0），缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_scale: Option<f32>,
    /// 基础色贴图路径（相对场景文件，也支持绝对路径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color_texture: Option<String>,
    /// 切线空间法线贴图路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<String>,
    /// 金属度-粗糙度贴图路径（绿通道 = 粗糙度，蓝通道 = 金属度）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metallic_roughness_texture: Option<String>,
    /// 自发光贴图路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<String>,
    /// 法线强度，缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal_scale: Option<f32>,
    /// alpha 裁剪阈值（> 0 时改走裁剪管线），缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_cutoff: Option<f32>,
    /// 强制走 alpha 混合管线（贴图自带 alpha 时**必须**置 `true`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alpha_blend: Option<bool>,
    /// 无光照（自发光常量色，常用于面板/标记）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlit: Option<bool>,
    /// 双面渲染（不剔除背面），缺省 false。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_sided: Option<bool>,
}

fn default_roughness() -> f32 {
    0.5
}

/// 图元类型（`shape.type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    /// 立方体 / 长方体。
    Box,
    /// 球。
    Sphere,
    /// 胶囊。
    Capsule,
    /// 圆柱。
    Cylinder,
    /// 平面（无限大，仅物理用）。
    Plane,
    /// 凸包（用 `points` 指定顶点）。
    Convex,
}

/// 形状描述。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapeDoc {
    /// 形状种类。
    #[serde(rename = "type")]
    pub kind: ShapeKind,
    /// 立方体的**边长**（不是半长），缺省 `[1, 1, 1]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 3]>,
    /// 球的半径，缺省 0.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// 胶囊/圆柱的半高（不含两端半球），缺省 0.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_height: Option<f32>,
    /// 平面法线，缺省 `[0, 1, 0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f32; 3]>,
    /// 凸包顶点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f32; 3]>>,
}

impl Default for ShapeDoc {
    fn default() -> Self {
        Self {
            kind: ShapeKind::Box,
            size: None,
            radius: None,
            half_height: None,
            normal: None,
            points: None,
        }
    }
}

/// 刚体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    /// 纯显示，不参与物理。
    #[default]
    None,
    /// 静态：无限质量，可被碰撞但不移动。
    Static,
    /// 动态：受重力与碰撞影响。
    Dynamic,
    /// 运动学：由代码/动画驱动，不被碰撞推开。
    Kinematic,
}

/// 复合形状的一个部件：形状 + 相对刚体原点的局部位姿。
///
/// 写成 `{ "shape": {...}, "position": [...], "rotation_deg": [...] }` 而不是把形状字段
/// 摊平：`ShapeDoc` 是 `deny_unknown_fields` 的，摊平（serde flatten）与它冲突，
/// 而且摊平之后"`position` 是物体的还是部件的"会变得很难读。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShapePartDoc {
    /// 部件形状。
    pub shape: ShapeDoc,
    /// 相对刚体原点的位置（米），缺省原点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 相对刚体坐标系的欧拉角（**度**，XYZ），缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<[f32; 3]>,
}

impl ObjectDoc {
    /// 归一化后的形状部件：`shape` 视作一个单位位姿的部件，`shapes` 原样。
    ///
    /// 校验保证二者恰有一个（或都不写 = 缺省立方体），所以这里**不会**返回空表之外的东西；
    /// 真为空时调用方给缺省立方体（校验已经先报错了）。
    pub fn parts(&self) -> Vec<ShapePartDoc> {
        if let Some(list) = &self.shapes {
            return list.clone();
        }
        if let Some(shape) = &self.shape {
            return vec![ShapePartDoc {
                shape: shape.clone(),
                position: None,
                rotation_deg: None,
            }];
        }
        Vec::new()
    }

    /// 主形状（第一个部件；没有就退回缺省立方体）。
    pub fn primary_shape(&self) -> ShapeDoc {
        self.parts()
            .into_iter()
            .next()
            .map(|p| p.shape)
            .unwrap_or_default()
    }
}

/// 一个实例的局部位姿（`objects[].instances[]`）。
///
/// 只写"每个副本差在哪"：位置、朝向、显示缩放，可选名字。
/// 没写的字段沿用物体（或模板）上的值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceDoc {
    /// 这个实例的名字，缺省 `名字#序号`（只有一个实例时就是物体本名）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 位置（米）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 欧拉角（**度**，XYZ）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<[f32; 3]>,
    /// 显示缩放。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
}

/// 一个图元物体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectDoc {
    /// 复用 `templates` 里的模板（物体自己写的字段**覆盖**模板）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// 一次摆多个副本（每个副本一个独立物体/刚体）。
    ///
    /// 这是"把同一段描述写 N 遍"的替代品：JSON 短、token 省，**语义上仍然是 N 个
    /// 独立物体**（各自一个刚体、各自参与物理），不是共享同一个刚体。
    /// 名字缺省 `名字#1`、`名字#2`…；只写一个实例且没写 `name` 时保持物体本名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instances: Option<Vec<InstanceDoc>>,
    /// 名称（关节引用、报告输出都靠它），缺省自动编号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 形状（单形状物体的写法），缺省 1×1×1 立方体。
    ///
    /// **与 `shapes` 二选一**：两个都写会报错（不猜"以哪个为准"），
    /// 两个都不写等于缺省的 1×1×1 立方体（老行为）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<ShapeDoc>,
    /// 复合形状（一个刚体多个 Shape），与 `shape` 二选一。
    ///
    /// 每个部件的 `position` / `rotation_deg` 是**相对刚体原点**的局部位姿；
    /// 质量、质心、惯性张量按所有部件合成，碰撞/AABB/查询都逐部件处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shapes: Option<Vec<ShapePartDoc>>,
    /// 父物体的**名字**（`objects[].name`），缺省为根物体。
    ///
    /// 给了父物体之后，`position` / `rotation_deg` 都变成**相对父物体**的局部量：
    /// 父物体一动，子物体跟着走。子物体继承父物体的**位置与朝向，但不继承缩放** ——
    /// 这个格式里 `scale` 就是图元尺寸（`shape.size` 折算而来），乘法继承会让
    /// "挂在大地板下的树"被压成一块饼。
    /// 带刚体的物体不能有父物体 —— 它的位姿由物理决定，父物体只会被静默忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 位置（米），缺省原点；有 `parent` 时是相对父物体的局部位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 欧拉角（**度**，XYZ 顺序），缺省 0；有 `parent` 时是相对父物体的局部朝向。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<[f32; 3]>,
    /// 显示缩放倍率（只影响渲染，不影响碰撞），缺省 `[1, 1, 1]`；**不受父物体缩放影响**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
    /// 材质名（引用 `materials`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// 刚体类型，缺省 `none`（纯显示）。
    #[serde(default)]
    pub body: BodyKind,
    /// 密度（kg/m³），缺省 1000；质量为密度×体积。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    /// 恢复系数 0~1，缺省 0.2。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restitution: Option<f32>,
    /// 动摩擦系数，缺省 0.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction: Option<f32>,
    /// 静摩擦系数（切向速度很小时用），缺省**跟随 `friction`**。
    ///
    /// 想让"推得动但自己不会慢慢滑"就把它写得比 `friction` 大（真实材质大多如此，
    /// 木头 0.4/0.5、橡胶 0.6/1.0 之类）。写 0 = 放下就开始滑。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_friction: Option<f32>,
    /// 这个物体的摩擦系数合成规则（覆盖 `physics.friction_combine`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_combine: Option<CombineRuleDoc>,
    /// 这个物体的恢复系数合成规则（覆盖 `physics.restitution_combine`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restitution_combine: Option<CombineRuleDoc>,
    /// 初始线速度，缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<[f32; 3]>,
    /// 初始角速度（弧度/秒），缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angular_velocity: Option<[f32; 3]>,
    /// 线性阻尼（≥0，越大越"黏"），缺省 0（不衰减）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_damping: Option<f32>,
    /// 角阻尼（≥0），缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angular_damping: Option<f32>,
    /// 重力缩放：1 = 正常，0 = 失重，2 = 双倍，负值 = 反重力。缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity_scale: Option<f32>,
    /// 直接指定质量（kg），**覆盖** `density`×体积。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mass: Option<f32>,
    /// 是否允许自动睡眠（停了就冻结、不再算力，省 CPU），缺省 true。
    ///
    /// 想让物体一直"活着"（等外力、等关节推动）就写 false。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_sleep: Option<bool>,
    /// 是否可见，缺省 true。
    #[serde(default = "default_true")]
    pub visible: bool,
    /// 碰撞层（0~31），缺省 0。
    ///
    /// 一层是一个位；只有**双方都愿意**才会产生碰撞（见 `collides_with`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collision_layer: Option<u8>,
    /// 愿意和哪些层碰（层号列表），缺省"所有层"。
    ///
    /// 判断是**对称**的：`A` 愿意碰 `B` 且 `B` 也愿意碰 `A` 才碰。
    /// 写空数组 `[]` 表示谁也不碰（穿透一切，但仍参与传感器事件）。
    /// 只影响**物理**：射线检测与拾取不看这一项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collides_with: Option<Vec<u8>>,
    /// 传感器（触发区），缺省 false。
    ///
    /// 传感器照常参与接触检测与事件（`sensor_events` / 报告里的"进入/离开"），
    /// 但**不产生任何冲量**，别的物体直接穿过去。需要 `body` 不为 `"visual"`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<bool>,
    /// 连续碰撞检测（CCD），缺省 false。
    ///
    /// 给"高速小物体"用：每子步做一次形状扫掠，把这一步朝障碍物的位移截在接触点，
    /// 因此不会一步跨过薄墙。代价是每子步一次扫掠（与速度无关）。
    /// 需要 `body` 是 `"dynamic"`；它是保守推进式的近似，不是精确 TOI。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ccd: Option<bool>,
}

fn default_true() -> bool {
    true
}

/// 资源搜索路径。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetsDoc {
    /// 依次尝试的目录列表（相对路径以场景文件所在目录为基准）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_paths: Vec<PathBuf>,
}

/// 一个外部模型文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDoc {
    /// 文件路径（`.gltf` / `.glb` / `.obj`）。
    pub path: PathBuf,
    /// 世界位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 欧拉角（度）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<[f32; 3]>,
    /// 均匀缩放，缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// 是否把模型合并成静态三角网格碰撞体（关卡地形），缺省 false。
    #[serde(default)]
    pub collide: bool,
    /// 参与碰撞时的摩擦系数，缺省 0.8。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction: Option<f32>,
}

/// 关节类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    /// 焊接。
    Fixed,
    /// 球窝。
    Ball,
    /// 铰链。
    Hinge,
    /// 距离 / 弹簧。
    Distance,
    /// 滑块。
    Slider,
}

/// 一个关节（用物体名引用两端）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JointDoc {
    /// 关节类型。
    #[serde(rename = "type")]
    pub kind: JointKind,
    /// 一端物体名。
    pub a: String,
    /// 另一端物体名。
    pub b: String,
    /// 轴向（铰链/滑块的转轴/滑轴），缺省 `[0, 1, 0]`。
    ///
    /// 注意是**各自刚体的局部轴**（实现里直接喂给 `local_axis_a/b`），不是世界轴：
    /// 两端刚体没转时两者等价；一旦物体 `rotation_deg` 非零，实际世界轴向会跟着转。
    /// 想要"世界某方向"的铰链，要么别转物体，要么自己把方向乘上物体旋转的逆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<[f32; 3]>,
    /// 关节在刚体 a 上的连接点（**a 的局部坐标**，米），缺省原点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_a: Option<[f32; 3]>,
    /// 关节在刚体 b 上的连接点（**b 的局部坐标**，米），缺省原点。
    ///
    /// 两个锚点错开就能表达"门轴偏在一边""悬臂"这类结构；都给原点（默认）时，
    /// 关节把两个刚体的原点绑在一起。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_b: Option<[f32; 3]>,
    /// 限位：铰链/滑块为 `[最小, 最大]`（铰链单位弧度，滑块单位米）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<[f32; 2]>,
    /// 距离关节的静止长度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rest_length: Option<f32>,
    /// 距离关节的刚度。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stiffness: Option<f32>,
    /// 距离关节的阻尼。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f32>,
    /// 铰链马达目标角速度（弧度/秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor_velocity: Option<f32>,
    /// 铰链马达最大冲量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor_max_impulse: Option<f32>,
    /// 铰链马达的**伺服目标角**（度）。给了它就进伺服模式（转到这个角度停住），
    /// 否则 `motor_velocity` 是速度模式。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor_target_deg: Option<f32>,
    /// 伺服比例增益（1/s），缺省 8。越大越"急着到位"，配合 `motor_max_impulse` 限幅。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor_stiffness: Option<f32>,
    /// 伺服阻尼增益，缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motor_damping: Option<f32>,
    /// 球窝关节的锥形限位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cone_limit: Option<ConeLimitDoc>,
    /// 断裂力（N）：本步关节力超过它就断开。**不写 = 永不断**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_force: Option<f32>,
    /// 断裂力矩（N·m）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub break_torque: Option<f32>,
    /// 关节两端是否**互相碰撞**，缺省 `false` = 不碰（与 Bullet / PhysX 一致）。
    ///
    /// 铰链、球窝的两端在几何上就是重合的（锚点相同）：让它们参与碰撞等于让门和门轴
    /// 互相挤，表现为"转到一半卡住""伺服停在没到位的角度上"。需要"用关节连起来但还要
    /// 能互相弹开"（比如软绳两头拴着两个球）时，把它设成 `true`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collide_connected: Option<bool>,
}

/// 球窝关节的锥形限位：两轴夹角不超过 `half_angle_deg`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConeLimitDoc {
    /// 允许的最大夹角（度，0~180）。
    pub half_angle_deg: f32,
    /// A 端局部轴，缺省 `[0,1,0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis_a: Option<[f32; 3]>,
    /// B 端局部轴，缺省 `[0,1,0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis_b: Option<[f32; 3]>,
}

/// 定时动作的类型。
///
/// 单位与物理一致：力 N、冲量 N·s、角冲量 N·m·s、速度 m/s、角速度 rad/s、长度 m。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// 施加一次冲量（N·s），会唤醒睡眠中的刚体。
    Impulse,
    /// 在**世界坐标**的某点施加一次冲量，同时产生绕质心的扭矩（用来"推门把手"）。
    ImpulseAtPoint,
    /// 施加一次角冲量（N·m·s），让它转起来。
    TorqueImpulse,
    /// 直接设定线速度（m/s），旁边还有 `velocity` 字段可作初始速度。
    Velocity,
    /// 直接设定角速度（rad/s）。
    AngularVelocity,
    /// 持续若干步施加力（N）；`steps` 缺省 1 步。
    Force,
    /// 瞬移到 `position`（可带 `rotation_deg` 朝向）。
    Teleport,
    /// 唤醒睡眠中的刚体（不施加任何作用力，只让它重新参与求解）。
    ///
    /// `target` 可以是具体刚体名，也可以是 `"*"`（唤醒全部，等价于
    /// [`PhysicsWorld::wake_up_all`]）。用法：搬动了一批物体、或从快照恢复之后，
    /// 让"以睡着姿态停在半空"的物体重新落下来。
    Wake,
}

impl ActionKind {
    /// 诊断与报告里显示的名字。
    pub fn name(self) -> &'static str {
        match self {
            ActionKind::Impulse => "impulse",
            ActionKind::ImpulseAtPoint => "impulse_at_point",
            ActionKind::TorqueImpulse => "torque_impulse",
            ActionKind::Velocity => "velocity",
            ActionKind::AngularVelocity => "angular_velocity",
            ActionKind::Force => "force",
            ActionKind::Teleport => "teleport",
            ActionKind::Wake => "wake",
        }
    }

    /// 是否需要 `value`。
    pub fn needs_value(self) -> bool {
        !matches!(self, ActionKind::Teleport | ActionKind::Wake)
    }

    /// 是否用世界坐标作用点 `point`。
    pub fn uses_point(self) -> bool {
        matches!(self, ActionKind::ImpulseAtPoint)
    }

    /// 是否用 `position` / `rotation_deg`。
    pub fn uses_position(self) -> bool {
        matches!(self, ActionKind::Teleport)
    }

    /// 是否用 `steps`（持续步数）。
    pub fn uses_steps(self) -> bool {
        matches!(self, ActionKind::Force)
    }

    /// 是否会给刚体施加作用（静态刚体上完全没有效果）。
    pub fn pushes(self) -> bool {
        !matches!(self, ActionKind::Wake)
    }
}

/// 定时动作：在指定步（或秒）对某个刚体施加一次或一段时间的作用。
///
/// 时序语义：`at_step = N` 表示**第 N 步开始前**施加（第 1 步是第一次物理步进，
/// `at_step = 0` 是仿真开始前）；`at_time` 是它的秒数写法，按 `round(t / dt)` 折算。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDoc {
    /// 动作类型。
    #[serde(rename = "type")]
    pub kind: ActionKind,
    /// 目标刚体名（`objects[].name` 或 `$ground` / `$model[i]`）。
    ///
    /// 只有 `wake` 动作接受特殊值 `"*"`：唤醒**所有**刚体（不需要挨个点名）。
    /// 别的动作给 `"*"` 会报错——否则"我写了 `*` 但它其实只作用在一个物体上"
    /// 就成了最难查的那类静默失效。
    pub target: String,
    /// 第几步开始前施加。与 `at_time` 二选一；两个都不给时是第 0 步。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_step: Option<u32>,
    /// 第几秒开始施加（与 `at_step` 互斥）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_time: Option<f32>,
    /// 作用量：冲量/力/速度/角冲量的 `[x,y,z]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<[f32; 3]>,
    /// 世界坐标的作用点，仅 `impulse_at_point` 用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<[f32; 3]>,
    /// 目标位置，仅 `teleport` 用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 目标朝向（欧拉角 XYZ，度），仅 `teleport` 用；缺省保持当前朝向。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_deg: Option<[f32; 3]>,
    /// `force` 持续多少步，缺省 1。
    #[serde(default = "default_action_steps")]
    pub steps: u32,
}

fn default_action_steps() -> u32 {
    1
}

/// 探针区域形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeShapeDoc {
    /// 球形（用 `radius`）。
    #[default]
    Sphere,
    /// 轴对齐盒（用 `size` 边长）。
    Box,
}

/// 区域探针：只读地记录进入该区域的刚体、进入/离开的步号、区内最高速度。
///
/// 判定用刚体的世界包围盒与区域求交（"碰到"就算进入），因此比按质心判定更符合
/// "有没有进这道门"的直觉。缺省只观测 `dynamic` / `kinematic` 刚体（静态物体的
/// 包围盒往往极大，比如地面平面），要观测静态物体就用 `watch` 明确点名。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeDoc {
    /// 探针名（报告里用它，必须唯一）。
    pub name: String,
    /// 区域形状，缺省球形。
    #[serde(default)]
    pub shape: ProbeShapeDoc,
    /// 区域中心（世界坐标），缺省原点。
    #[serde(default)]
    pub position: [f32; 3],
    /// 球形半径，缺省 1.0。
    #[serde(default = "default_probe_radius")]
    pub radius: f32,
    /// 盒形边长，缺省 `[1,1,1]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 3]>,
    /// 只观测这些刚体（缺省观测全部 dynamic / kinematic 刚体）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<String>,
}

fn default_probe_radius() -> f32 {
    1.0
}

/// 相机配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraDoc {
    /// 相机位置；给了 `position` 与 `look_at` 就会自动对准。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// 注视点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_at: Option<[f32; 3]>,
    /// 视场角（度），缺省 60。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov_deg: Option<f32>,
    /// 环绕距离（不给 `position` 时生效）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
    /// 环绕水平角（度）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw_deg: Option<f32>,
    /// 环绕俯仰角（度）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch_deg: Option<f32>,
    /// 近裁剪面（米），缺省 0.05。调大能减少深度冲突（z-fighting）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near: Option<f32>,
    /// 远裁剪面（米），缺省 5000。必须大于 `near`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub far: Option<f32>,
    /// 正交投影的可视高度（世界单位）。
    ///
    /// 给了就切到正交投影（没有近大远小，适合工程/制图/2.5D 视角），此时 `fov_deg`
    /// 不起作用；不给就是透视投影。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orthographic_height: Option<f32>,
    /// 自动框住整个场景（在 `position` 之后应用），缺省 false。
    #[serde(default)]
    pub frame_scene: bool,
}

/// 一盏点光源。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointLightDoc {
    /// 位置（世界坐标）。
    pub position: [f32; 3],
    /// 颜色 `[r, g, b]`，缺省白色。
    ///
    /// **线性空间数值，与引擎预设一样直接使用**（注意与 `materials.*.color` 的 sRGB
    /// 约定不同）；想要暖光就写 `[1, 0.92, 0.85]` 这种数。
    #[serde(default = "default_light_color")]
    pub color: [f32; 3],
    /// 强度（线性），缺省 12。
    #[serde(default = "default_light_intensity")]
    pub intensity: f32,
    /// 影响半径（米），缺省 18，必须为正。
    #[serde(default = "default_light_radius")]
    pub radius: f32,
}

fn default_light_color() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

fn default_light_intensity() -> f32 {
    12.0
}

fn default_light_radius() -> f32 {
    18.0
}

/// 光照配置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightingDoc {
    /// 预设：`studio`（缺省，含两盏补光）/ `day`（只有太阳+天光）/ `night`（无主光，微弱环境光）
    /// / `none`（无主光且环境光为 0，基本只剩自发光与点光源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// 太阳（平行光）方向（从光源射出的方向），缺省 `[-0.5, -1, -0.35]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sun_direction: Option<[f32; 3]>,
    /// 太阳强度倍率。缺省跟随预设（`studio` 为 2.9，`day` 为 3.0，`night` 无太阳）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sun_intensity: Option<f32>,
    /// 环境光强度。缺省跟随预设（`studio` 为 0.38，`day` 为 0.55，`night` 为 0.35）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambient: Option<f32>,
    /// 曝光，缺省 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    /// 是否绘制天空，缺省 true。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky: Option<bool>,
    /// 是否绘制无限地面网格，缺省 true。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grid: Option<bool>,
    /// 是否投射阴影，缺省 true。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<bool>,
    /// 雾（指数距离雾）。
    ///
    /// **不写 = 关闭**（密度 0，像素与加这个功能之前逐位相同）；写了这一节就是打开。
    /// 颜色是**线性空间**（与 `point_lights.color` 一致，不是 `materials.*.color` 的
    /// sRGB 约定）；`density` 是每米的指数衰减系数，0.02~0.1 是常见区间
    /// （0.05 大约在 20 米外看不清）。天空与参考网格也会跟着雾一起淡出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fog: Option<FogDoc>,
    /// 阴影质量（覆盖范围 / 强度）。
    ///
    /// 与 `shadows`（开关）分开：`shadows: false` 是"完全不投影"，这一节调的是
    /// **投影时的质量**。**不写 = 用引擎缺省**（strength 0.85、radius 28 米）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow_quality: Option<ShadowQualityDoc>,
    /// 天空渐变的三个颜色。
    ///
    /// 不写就跟随 `preset`（`day`/`night` 各有自己的天空）；写了的通道覆盖预设。
    /// 颜色是**线性空间**。天空关掉（`sky: false`）时这一节没有任何视觉效果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky_colors: Option<SkyColorsDoc>,
    /// 点光源列表。
    ///
    /// **不写这个字段 = 保留预设自带的灯**（`studio` 预设有两盏补光）；写 `[]` 才是
    /// 显式清空；写了就整体替换预设。这样加字段不会悄悄改变已有场景的观感。
    ///
    /// 着色时最多同时生效 [`MAX_POINT_LIGHTS`] 盏，多出来的会被**忽略**——所以校验
    /// 会在超过上限时给出警告，而不是让用户以为都亮了。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_lights: Option<Vec<PointLightDoc>>,
    /// Bloom（泛光/亮区外溢）。
    ///
    /// **不写 = 关闭**（与改造前的像素完全一致）；写了这一节就是打开。
    /// 光照与自发光都会先落到**线性 HDR** 中间目标，超过阈值的部分被抽出来做
    /// 高斯模糊，再在 tonemap **之前**加回去——所以亮区外溢不会过曝成白块。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bloom: Option<BloomDoc>,
    /// FXAA（后处理抗锯齿）。
    ///
    /// **不写 = 关闭**（像素与加这个功能之前逐位相同）；写了这一节就是打开。
    /// 它工作在 tonemap **之后**的画面上：找感知边缘、沿着边缘方向做一次定向模糊。
    /// 跟 MSAA 不冲突也不重复：MSAA 在几何边缘上以更高采样率着色，FXAA 负责剩下的
    /// （着色噪声、材质内部的高频），代价是细节会略微变软。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fxaa: Option<FxaaDoc>,
    /// SSAO（屏幕空间环境光遮蔽）。
    ///
    /// **不写 = 关闭**（像素与加这个功能之前逐位相同）；写了这一节就是打开。
    /// 它让"物体与地面接触的缝隙、墙角"变暗，是廉价版的接触阴影。
    /// 实现是**深度-only** 的（没有法线缓冲，法线由深度重建），
    /// 结果乘在整幅 HDR 颜色上（不区分直接光/间接光）——这是后处理式 SSAO 的固有简化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssao: Option<SsaoDoc>,
}

/// 指数距离雾配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FogDoc {
    /// 雾颜色（**线性空间**），缺省跟随预设/引擎缺省 `[0.36, 0.46, 0.63]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// 密度（每米），缺省 0.02（写了这一节就是想看见雾；给 0 等于关掉）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
}

/// 阴影质量配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ShadowQualityDoc {
    /// 阴影强度 0~1：0 = 阴影不压暗（等于没有阴影），1 = 全黑。缺省 0.85。
    ///
    /// 真实场景里阴影不会全黑（还有环境光），所以 0.6~0.9 通常比 1.0 好看。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    /// 阴影相机覆盖半径（米），缺省 28。
    ///
    /// **这是"覆盖范围 ↔ 精度"的旋钮**：给得越大，远处物体也能投上阴影，
    /// 但同样的贴图分辨率摊在更大面积上 → 阴影边缘更糊、锯齿更明显。
    ///
    /// **只在 `cascades = 1`（缺省）时有意义**：多级阴影按相机视锥切段，
    /// 覆盖范围由视锥决定，不再用这个半径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// 阴影级联数（1~4，缺省 1）：1 = 单级（与老场景逐像素一致），2~4 = 把相机视锥切段分级。
    ///
    /// 单级用一个固定的正交盒（覆盖相机前方 `radius` 米）：贴图分辨率全给这一块，
    /// 远处要么被截断要么靠加大 `radius` 牺牲精度。多级把视锥按深度切成 N 段，
    /// **近处细、远处粗**——同样的贴图分辨率下近处阴影明显更干净。
    ///
    /// 代价要说清楚：N 级要画 N 遍阴影 pass，而且每级只分到**半张**贴图（2x2 图集），
    /// 所以 N > 1 时每一级的有效分辨率是 `贴图边长 / 2`。近处划算；超远场景更应该提高
    /// `shadow_map_size`，而不是一味加级数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cascades: Option<u32>,
    /// 级联切分的"对数 ↔ 均匀"混合系数（0~1，缺省 0.5），只在 `cascades > 1` 时有意义。
    ///
    /// 1 = 全对数切分（近处最细，远处跨度大）；0 = 均匀切分（每级跨度一样）。
    /// 缺省 0.5 是常见的折中。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_lambda: Option<f32>,
}

/// 天空渐变的三个颜色（**线性空间**）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SkyColorsDoc {
    /// 天顶颜色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zenith: Option<[f32; 3]>,
    /// 地平线颜色（雾也会往这个颜色上靠）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizon: Option<[f32; 3]>,
    /// 地平线以下的颜色。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<[f32; 3]>,
}

/// SSAO（屏幕空间环境光遮蔽）配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SsaoDoc {
    /// 是否启用，缺省 true（写了这一节就是想开）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 采样半径（**米**，视空间），缺省 0.75：影响多大的范围算"邻居"。
    ///
    /// 物体尺度 1~2 米时 0.5~1.0 合适；给太大远处的墙也会把画面压暗。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// 深度偏移（米），缺省 0.025：压掉"同一个面上自己遮自己"的自遮蔽。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bias: Option<f32>,
    /// 强度，缺省 1.0（0 = 不暗，越大接触处越黑）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<f32>,
    /// 每像素采样数，缺省 16（1~64）：越多越平滑也越贵。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
}

/// FXAA（后处理抗锯齿）配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FxaaDoc {
    /// 是否启用，缺省 true（写了这一节就是想开）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 相对边缘阈值，缺省 0.166（FXAA 原版质量档）：对比度低于它就不当成边缘。
    ///
    /// 越小越敏感（能压掉更细的锯齿，也更容易把细节糊掉）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_threshold: Option<f32>,
    /// 绝对边缘阈值下限，缺省 0.0833：压掉平坦区域里的单像素噪点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_threshold_min: Option<f32>,
}

/// Bloom（泛光）配置。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BloomDoc {
    /// 是否启用，缺省 true（写了这一节就是想开）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// 强度（乘在模糊结果上），缺省 0.6；0 等于关闭。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    /// 亮度阈值（线性空间 Rec.709 亮度），缺省 1.0：只有比它还亮的部分才会外溢。
    ///
    /// 天空与太阳圆盘可以远大于 1，所以阈值 1.0 通常只有太阳/自发光/高光会触发。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
}

/// 物理步进配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulateDoc {
    /// 步数，缺省 0（不步进）。
    #[serde(default)]
    pub steps: u32,
    /// 单步时长（秒），缺省 1/60。
    #[serde(default = "default_dt")]
    pub dt: f32,
}

fn default_dt() -> f32 {
    1.0 / 60.0
}

impl Default for SimulateDoc {
    fn default() -> Self {
        Self {
            steps: 0,
            dt: default_dt(),
        }
    }
}

/// `render.outputs[]` 的一项：一次渲染多张图时，每张自己的名字 / 相机 / 尺寸。
///
/// 缺省（不写 `render.outputs`）就是老行为：只渲 `render.output` 一张。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDoc {
    /// 文件名（不含扩展名，默认 `out-<序号>`）。只当 `render.output` 是目录时用得上。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 用哪台命名相机（见 `cameras`）；缺省用 `camera` 段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// 这一张的宽度（缺省用 `render.width`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// 这一张的高度（缺省用 `render.height`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}
/// 渲染配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderDoc {
    /// 输出 PNG 路径，缺省 `output.png`。
    #[serde(default = "default_output")]
    pub output: PathBuf,
    /// 宽度，缺省 960。
    #[serde(default = "default_width")]
    pub width: u32,
    /// 高度，缺省 600。
    #[serde(default = "default_height")]
    pub height: u32,
    /// MSAA 采样数，缺省 4。
    #[serde(default = "default_msaa")]
    pub msaa: u32,
    /// 一次渲多张：每项自己的名字 / 相机 / 尺寸。
    ///
    /// 空 = 老行为（只渲 `output` 一张）。写了它，`output` 就当**目录**用，
    /// 文件名 = `name`（缺省 `out-<序号>`）+ `.png`；只写一项且没给 `name` 时
    /// 仍然直接用 `output` 那个路径（与旧场景逐位一致）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<OutputDoc>,
    /// 是否渲染（false 时只做仿真，不出图）。
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_output() -> PathBuf {
    PathBuf::from("output.png")
}

fn default_width() -> u32 {
    960
}

fn default_height() -> u32 {
    600
}

fn default_msaa() -> u32 {
    4
}

impl Default for RenderDoc {
    fn default() -> Self {
        Self {
            output: default_output(),
            width: default_width(),
            height: default_height(),
            msaa: default_msaa(),
            enabled: true,
            outputs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// 解析与校验
// ---------------------------------------------------------------------------

/// 解析错误。
#[derive(Debug)]
pub enum DocError {
    /// JSON 语法/结构错误。
    Json(serde_json::Error),
    /// 语义校验失败。
    Invalid(Vec<Diagnostic>),
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocError::Json(e) => write!(f, "JSON 解析失败: {e}"),
            DocError::Invalid(diags) => {
                let errors: Vec<&Diagnostic> = diags
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .collect();
                write!(f, "场景校验失败（{} 个错误）", errors.len())?;
                for d in errors.iter().take(5) {
                    write!(f, "\n  {} : {}", d.path, d.message)?;
                }
                if errors.len() > 5 {
                    write!(f, "\n  …以及 {} 个", errors.len() - 5)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for DocError {}

impl From<serde_json::Error> for DocError {
    fn from(e: serde_json::Error) -> Self {
        DocError::Json(e)
    }
}

/// 一次结构检查：把每一段的类型/枚举错误都转成带 JSON 路径的诊断。
///
/// 合法字段名不是手写的，而是从 `SceneDoc::default()` 序列化结果里取——加了新字段
/// 这里自动跟上，不会漂移。
pub fn structural_diagnostics(value: &Value) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let Some(map) = value.as_object() else {
        out.push(Diagnostic::error("$", "场景根节点必须是 JSON 对象"));
        return out;
    };

    // 未知顶层字段：拿「只有这一个字段」的对象去反序列化，若 serde 报 unknown field
    // 就说明这个名字不存在——判断标准来自结构体本身，不用手写字段清单。
    for (key, val) in map {
        let mut probe = serde_json::Map::new();
        probe.insert(key.clone(), val.clone());
        if let Err(e) = serde_json::from_value::<SceneDoc>(Value::Object(probe)) {
            // 注意：嵌套结构里的未知字段也会报 "unknown field"，所以必须比字段名
            if unknown_field_name(&e.to_string()).as_deref() == Some(key.as_str()) {
                out.push(
                    Diagnostic::error(key, format!("未知字段 \"{key}\"")).with_hint(
                        "顶层可用字段：format、version、name、gravity、ground、materials、\
                         objects、models、assets、joints、actions、probes、camera、lighting、\
                         simulate、render",
                    ),
                );
            }
        }
    }

    // 单值段：类型不对会直接报出来
    check_section::<String>(map.get("format"), "format", &mut out);
    check_section::<String>(map.get("name"), "name", &mut out);
    check_section::<u32>(map.get("version"), "version", &mut out);
    check_section::<[f32; 3]>(map.get("gravity"), "gravity", &mut out);
    check_section::<GroundDoc>(map.get("ground"), "ground", &mut out);
    check_section::<CameraDoc>(map.get("camera"), "camera", &mut out);
    check_section::<Vec<AnimationDoc>>(map.get("animations"), "animations", &mut out);
    check_section::<BTreeMap<String, CameraDoc>>(map.get("cameras"), "cameras", &mut out);
    check_section::<LightingDoc>(map.get("lighting"), "lighting", &mut out);
    check_section::<SimulateDoc>(map.get("simulate"), "simulate", &mut out);
    check_section::<RenderDoc>(map.get("render"), "render", &mut out);
    // `outputs` 在 render 段内部，逐项走一遍（结构体的 deny_unknown_fields 只挡名字，
    // 值的类型要这里查）
    if let Some(render) = map.get("render").and_then(|v| v.as_object()) {
        check_section::<Vec<OutputDoc>>(render.get("outputs"), "render.outputs", &mut out);
    }

    // 模板段：字段集与物体完全相同（用同一个检查器，路径 `templates.名`）
    check_section::<BTreeMap<String, ObjectDoc>>(map.get("templates"), "templates", &mut out);
    if let Some(items) = map.get("templates").and_then(|v| v.as_object()) {
        for (name, item) in items {
            check_object(item, &format!("templates.{name}"), &mut out);
        }
    }
    // 数组段：先确认是数组，再逐个元素检查（路径带下标）
    check_array::<ObjectDoc>(map.get("objects"), "objects", &mut out);
    if let Some(items) = map.get("objects").and_then(|v| v.as_array()) {
        for (i, item) in items.iter().enumerate() {
            check_object(item, &format!("objects[{i}]"), &mut out);
        }
    }
    check_array::<ModelDoc>(map.get("models"), "models", &mut out);
    if let Some(items) = map.get("models").and_then(|v| v.as_array()) {
        for (i, item) in items.iter().enumerate() {
            check_model(item, &format!("models[{i}]"), &mut out);
        }
    }
    check_array::<JointDoc>(map.get("joints"), "joints", &mut out);
    if let Some(items) = map.get("joints").and_then(|v| v.as_array()) {
        for (i, item) in items.iter().enumerate() {
            check_joint(item, &format!("joints[{i}]"), &mut out);
        }
    }
    check_array::<ActionDoc>(map.get("actions"), "actions", &mut out);
    if let Some(items) = map.get("actions").and_then(|v| v.as_array()) {
        for (i, item) in items.iter().enumerate() {
            check_action(item, &format!("actions[{i}]"), &mut out);
        }
    }
    check_array::<ProbeDoc>(map.get("probes"), "probes", &mut out);
    if let Some(items) = map.get("probes").and_then(|v| v.as_array()) {
        for (i, item) in items.iter().enumerate() {
            check_probe(item, &format!("probes[{i}]"), &mut out);
        }
    }

    // 映射段：逐条目检查
    check_section::<BTreeMap<String, Value>>(map.get("materials"), "materials", &mut out);
    if let Some(entries) = map.get("materials").and_then(|v| v.as_object()) {
        for (name, item) in entries {
            check_at::<MaterialDoc>(item, &format!("materials.{name}"), &mut out);
        }
    }

    out
}

/// 从 serde 的 "unknown field `x`, expected one of ..." 里取出字段名。
fn unknown_field_name(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("unknown field `")?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// 数组段：先确认是数组（不是数组就报类型错），元素由调用方逐个细查。
fn check_array<T: DeserializeOwned>(value: Option<&Value>, path: &str, out: &mut Vec<Diagnostic>) {
    if let Some(v) = value {
        if !v.is_array() {
            check_at::<Vec<T>>(v, path, out);
        }
    }
}

/// 单个字段的类型检查（路径精确到字段）。
fn check_field<T: DeserializeOwned>(
    map: &serde_json::Map<String, Value>,
    key: &str,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    if let Some(v) = map.get(key) {
        check_at::<T>(v, &format!("{path}.{key}"), out);
    }
}

/// 图元物体：逐字段检查，能把同一个物体里的多个问题一次报全。
fn check_object(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<ObjectDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<String>(map, "name", path, out);
    if let Some(shape) = map.get("shape") {
        check_shape(shape, &format!("{path}.shape"), out);
    }
    if let Some(shapes) = map.get("shapes") {
        if let Some(list) = shapes.as_array() {
            for (i, part) in list.iter().enumerate() {
                let pp = format!("{path}.shapes[{i}]");
                let Some(pmap) = part.as_object() else {
                    out.push(Diagnostic::error(
                        pp.clone(),
                        "部件必须是对象 { shape, position?, rotation_deg? }",
                    ));
                    continue;
                };
                if let Some(shape) = pmap.get("shape") {
                    check_shape(shape, &format!("{pp}.shape"), out);
                }
                check_field::<[f32; 3]>(pmap, "position", &pp, out);
                check_field::<[f32; 3]>(pmap, "rotation_deg", &pp, out);
            }
        } else {
            out.push(Diagnostic::error(
                format!("{path}.shapes"),
                "shapes 必须是数组（每个元素 { shape, position?, rotation_deg? }）",
            ));
        }
    }
    check_field::<[f32; 3]>(map, "position", path, out);
    check_field::<[f32; 3]>(map, "rotation_deg", path, out);
    check_field::<[f32; 3]>(map, "scale", path, out);
    check_field::<String>(map, "material", path, out);
    check_field::<BodyKind>(map, "body", path, out);
    check_field::<f32>(map, "density", path, out);
    check_field::<f32>(map, "restitution", path, out);
    check_field::<f32>(map, "friction", path, out);
    check_field::<[f32; 3]>(map, "velocity", path, out);
    check_field::<[f32; 3]>(map, "angular_velocity", path, out);
    check_field::<f32>(map, "linear_damping", path, out);
    check_field::<f32>(map, "angular_damping", path, out);
    check_field::<f32>(map, "gravity_scale", path, out);
    check_field::<f32>(map, "mass", path, out);
    check_field::<bool>(map, "allow_sleep", path, out);
    check_field::<bool>(map, "visible", path, out);
    check_field::<String>(map, "template", path, out);
    if let Some(instances) = map.get("instances") {
        if let Some(list) = instances.as_array() {
            for (k, inst) in list.iter().enumerate() {
                let ip = format!("{path}.instances[{k}]");
                let Some(imap) = inst.as_object() else {
                    out.push(Diagnostic::error(
                        ip.clone(),
                        "实例必须是对象 { position?, rotation_deg?, scale?, name? }",
                    ));
                    continue;
                };
                check_field::<String>(imap, "name", &ip, out);
                check_field::<[f32; 3]>(imap, "position", &ip, out);
                check_field::<[f32; 3]>(imap, "rotation_deg", &ip, out);
                check_field::<[f32; 3]>(imap, "scale", &ip, out);
            }
        } else {
            out.push(Diagnostic::error(
                format!("{path}.instances"),
                "instances 必须是数组",
            ));
        }
    }
    // 逐字段都通过时再整体检查一次，专门用来抓「未知字段」这类问题
    if out.len() == before {
        check_at::<ObjectDoc>(item, path, out);
    }
}

/// 形状：`type` 是枚举，其余是数值/数组。
fn check_shape(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<ShapeDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<ShapeKind>(map, "type", path, out);
    check_field::<[f32; 3]>(map, "size", path, out);
    check_field::<f32>(map, "radius", path, out);
    check_field::<f32>(map, "half_height", path, out);
    check_field::<[f32; 3]>(map, "normal", path, out);
    check_field::<Vec<[f32; 3]>>(map, "points", path, out);
    if out.len() == before {
        check_at::<ShapeDoc>(item, path, out);
    }
}

/// 外部模型。
fn check_model(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<ModelDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<PathBuf>(map, "path", path, out);
    check_field::<[f32; 3]>(map, "position", path, out);
    check_field::<[f32; 3]>(map, "rotation_deg", path, out);
    check_field::<f32>(map, "scale", path, out);
    check_field::<bool>(map, "collide", path, out);
    check_field::<f32>(map, "friction", path, out);
    if out.len() == before {
        check_at::<ModelDoc>(item, path, out);
    }
}

/// 关节。
fn check_joint(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<JointDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<JointKind>(map, "type", path, out);
    check_field::<String>(map, "a", path, out);
    check_field::<String>(map, "b", path, out);
    check_field::<[f32; 3]>(map, "axis", path, out);
    check_field::<[f32; 3]>(map, "anchor_a", path, out);
    check_field::<[f32; 3]>(map, "anchor_b", path, out);
    check_field::<[f32; 2]>(map, "limits", path, out);
    check_field::<f32>(map, "rest_length", path, out);
    check_field::<f32>(map, "stiffness", path, out);
    check_field::<f32>(map, "damping", path, out);
    check_field::<f32>(map, "motor_velocity", path, out);
    check_field::<f32>(map, "motor_max_impulse", path, out);
    if out.len() == before {
        check_at::<JointDoc>(item, path, out);
    }
}

/// 定时动作：逐字段检查。
fn check_action(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<ActionDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<ActionKind>(map, "type", path, out);
    check_field::<String>(map, "target", path, out);
    check_field::<u32>(map, "at_step", path, out);
    check_field::<f32>(map, "at_time", path, out);
    check_field::<[f32; 3]>(map, "value", path, out);
    check_field::<[f32; 3]>(map, "point", path, out);
    check_field::<[f32; 3]>(map, "position", path, out);
    check_field::<[f32; 3]>(map, "rotation_deg", path, out);
    check_field::<u32>(map, "steps", path, out);
    if out.len() == before {
        check_at::<ActionDoc>(item, path, out);
    }
}

/// 区域探针：逐字段检查。
fn check_probe(item: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    let Some(map) = item.as_object() else {
        check_at::<ProbeDoc>(item, path, out);
        return;
    };
    let before = out.len();
    check_field::<String>(map, "name", path, out);
    check_field::<ProbeShapeDoc>(map, "shape", path, out);
    check_field::<[f32; 3]>(map, "position", path, out);
    check_field::<f32>(map, "radius", path, out);
    check_field::<[f32; 3]>(map, "size", path, out);
    check_field::<Vec<String>>(map, "watch", path, out);
    if out.len() == before {
        check_at::<ProbeDoc>(item, path, out);
    }
}

/// 单值段 / 数组段的整体检查。
fn check_section<T: DeserializeOwned>(
    value: Option<&Value>,
    path: &str,
    out: &mut Vec<Diagnostic>,
) {
    if let Some(v) = value {
        check_at::<T>(v, path, out);
    }
}

fn check_at<T: DeserializeOwned>(value: &Value, path: &str, out: &mut Vec<Diagnostic>) {
    if let Err(e) = serde_json::from_value::<T>(value.clone()) {
        out.push(Diagnostic::error(path, clean_serde_message(&e.to_string())));
    }
}

/// 去掉 serde 错误里冗余的定位后缀（路径已经比行列号更好用）。
fn clean_serde_message(msg: &str) -> String {
    match msg.find(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

impl SceneDoc {
    /// 从 JSON 文本解析。
    ///
    /// 分两步走：先做一次**逐段结构检查**把字段名/类型/枚举错误全部收集成带路径的
    /// 诊断（agent 一次就能拿到所有问题，而不是只看到第一个），确认结构无误后再真正
    /// 反序列化。
    pub fn from_json(text: &str) -> Result<Self, DocError> {
        let value: Value = serde_json::from_str(text)?;
        Self::from_value(value)
    }

    /// 从已经解析好的 JSON 值构造（便于 MCP / 内联场景直接复用）。
    pub fn from_value(value: Value) -> Result<Self, DocError> {
        let structural = structural_diagnostics(&value);
        if !structural.is_empty() {
            return Err(DocError::Invalid(structural));
        }
        Ok(serde_json::from_value(value)?)
    }

    /// 从文件解析（相对路径按当前工作目录解析）。
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, DocError> {
        let path_ref = path.as_ref();
        let text = std::fs::read_to_string(path_ref).map_err(|e| {
            DocError::Invalid(vec![Diagnostic::error(
                "$",
                format!("无法读取场景文件 {}: {e}", path.as_ref().display()),
            )
            .with_hint(format!(
                "确认路径是否正确；当前工作目录是 {}",
                std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|_| "?".into())
            ))])
        })?;
        let mut doc = Self::from_json(&text)?;
        // 相对路径以场景文件所在目录为基准（空父目录 = 当前目录）
        doc.base_dir = path_ref.parent().map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        });
        Ok(doc)
    }

    /// 序列化为美观 JSON。
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 把场景内的资源相对路径展开成候选路径，按解析优先级排列。
    ///
    /// 顺序：绝对路径原样 → `场景文件所在目录/rel` → `工作目录/rel` →
    /// 引擎的标准搜索路径（可执行文件旁、`QUENCH3D_ASSET_PATH` 等，
    /// 见 `quench3d::assets::resolve_path`）。去重后返回。
    pub fn asset_candidates(&self, rel: &str) -> Vec<PathBuf> {
        let raw = std::path::Path::new(rel);
        if raw.is_absolute() {
            return vec![raw.to_path_buf()];
        }
        let mut out = Vec::new();
        let mut push = |p: PathBuf| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        if let Some(base) = &self.base_dir {
            push(base.join(raw));
        }
        // 场景自己声明的资源库：排在"场景目录"之后、"当前工作目录"之前。
        // 相对路径以场景目录为基准（与贴图/模型路径同一套规则）。
        if let Some(assets) = &self.assets {
            for sp in &assets.search_paths {
                let dir = if sp.is_absolute() {
                    sp.clone()
                } else {
                    match &self.base_dir {
                        Some(base) => base.join(sp),
                        None => sp.clone(),
                    }
                };
                push(dir.join(raw));
            }
        }
        push(raw.to_path_buf());
        if let Ok(resolved) = quench3d::assets::resolve_path(rel) {
            push(resolved);
        }
        out
    }

    /// 解析出第一个真实存在的候选路径（都没有时返回 `None`）。
    pub fn resolve_asset(&self, rel: &str) -> Option<PathBuf> {
        self.asset_candidates(rel).into_iter().find(|p| p.exists())
    }

    /// 模型的候选路径：与贴图**共用同一套规则**。
    ///
    /// 以前模型走的是加载器自己的规则（原路径、`assets/`、`quench3d/assets/`），
    /// **不含场景文件所在目录**——于是 `models[].path: "robot.glb"` 只有在"工作目录
    /// 正好是场景目录"时才加载得动，而 `validate` 用另一套规则又可能说没问题。
    /// 现在贴图与模型统一：场景目录 → `assets.search_paths` → 工作目录 → 引擎搜索路径。
    ///
    /// 非 UTF-8 的模型路径会被 `to_string_lossy` 处理（这种路径在 JSON 里本来就写不出来）。
    pub fn model_candidates(&self, path: &std::path::Path) -> Vec<PathBuf> {
        self.asset_candidates(&path.to_string_lossy())
    }

    /// 解析出第一个真实存在的模型路径。
    pub fn resolve_model(&self, path: &std::path::Path) -> Option<PathBuf> {
        self.model_candidates(path).into_iter().find(|p| p.exists())
    }

    /// 按 `--camera <名字>` 选一台命名相机：把 `camera` 段换成它。
    ///
    /// `default` 是 `camera` 段本身的别名（不做改动）。未知名返回 `Err`，消息里列出
    /// 可用名字——CLI 把它包成 `unknown_camera`（退出码 2，属于"输入有问题，改参数再跑"）。
    pub fn select_camera(&mut self, name: &str) -> Result<(), String> {
        if name == "default" {
            return Ok(());
        }
        match self.cameras.get(name) {
            Some(c) => {
                self.camera = c.clone();
                Ok(())
            }
            None => {
                let names: Vec<&str> = self.cameras.keys().map(|k| k.as_str()).collect();
                Err(if names.is_empty() {
                    format!("场景里没有 `cameras` 段，`--camera {name}` 无从选择")
                } else {
                    format!(
                        "场景里没有名为 `{name}` 的相机（可用：{}）",
                        names.join("、")
                    )
                })
            }
        }
    }

    /// 语义校验；返回全部诊断（含警告），调用方自行判断是否有 `Error`。
    pub fn validate(&self) -> Vec<Diagnostic> {
        // 命名相机（`cameras`）：格式与校验先落地；`--camera <名字>` 的选择留到下一步。
        // 名字只在两处会出问题：空名（无从引用）与保留名 `default`（它是 `camera` 段的别名）。
        let mut cam_diags = Vec::new();
        for name in self.cameras.keys() {
            if name.trim().is_empty() {
                cam_diags.push(
                    Diagnostic::error("cameras", "相机名字不能为空").with_hint(
                        "空名字没法被 `--camera` 引用；给它起个名字，或直接写 `camera` 段",
                    ),
                );
            } else if name == "default" {
                cam_diags.push(
                    Diagnostic::error("cameras", "`default` 是保留名（指 `camera` 段这台）")
                        .with_hint("把这一项改名，例如 `default` → `main`"),
                );
            }
        }
        // `render.outputs`：名字不能重复/带路径分隔符，相机必须存在，宽高不能为 0。
        // 这些都会让"一次渲多张"产出用户没预期的文件，属于值得当场拦下的错误。
        let mut seen: Vec<&str> = Vec::new();
        for (i, o) in self.render.outputs.iter().enumerate() {
            if let Some(name) = &o.name {
                if name.trim().is_empty() {
                    cam_diags.push(Diagnostic::error(
                        "render.outputs",
                        "输出名字不能为空（要么不写，要么给个名字）",
                    ));
                }
                if name.contains('/') || name.contains('\\') || name.contains("..") {
                    cam_diags.push(Diagnostic::error(
                        "render.outputs",
                        "输出名字不能含路径分隔符或 `..`（它就是文件名）",
                    ));
                }
                if seen.contains(&name.as_str()) {
                    cam_diags.push(Diagnostic::error(
                        "render.outputs",
                        "输出名字重复了（会互相覆盖）",
                    ));
                }
                seen.push(name.as_str());
            }
            if let Some(cam) = &o.camera {
                if cam != "default" && !self.cameras.contains_key(cam) {
                    cam_diags.push(Diagnostic::error(
                        "render.outputs",
                        "这一项指定的相机不存在（见 `cameras` 段，或用 `default` 指 `camera` 段）",
                    ));
                }
            }
            for v in [o.width, o.height].into_iter().flatten() {
                if v == 0 {
                    cam_diags.push(Diagnostic::error("render.outputs", "宽/高不能为 0"));
                }
            }
            let _ = i;
        }
        // 关键帧动画：轨道本身要自洽（长度一致、时间严格递增、>= 0、数值有限），
        // 目标名与"至少一条轨道"也要当场拦住——这些都是"跑起来才发现"的错。
        for a in &self.animations {
            let label = a.name.clone().unwrap_or_else(|| a.target.clone());
            if a.target.trim().is_empty() {
                cam_diags.push(Diagnostic::error("animations", "动画的 target 不能为空"));
            }
            if a.position.is_none() && a.rotation.is_none() {
                cam_diags.push(Diagnostic::error(
                    "animations",
                    format!(
                        "动画 `{label}` 至少要有一条 position 或 rotation 轨道（否则它什么都不做）"
                    ),
                ));
            }
            if let Some(w) = a.weight {
                if !w.is_finite() || !(0.0..=1.0).contains(&w) {
                    cam_diags.push(Diagnostic::error(
                        "animations",
                        "weight 必须在 0~1 之间（0 = 不生效，1 = 完全生效）",
                    ));
                }
            }
            if let Some(sp) = a.speed {
                if !(sp.is_finite() && sp > 0.0) {
                    cam_diags.push(Diagnostic::error("animations", "speed 必须是正的有限值"));
                }
            }
            if let Some(st) = a.start {
                if !st.is_finite() {
                    cam_diags.push(Diagnostic::error("animations", "start 必须是有限值"));
                }
            }
            for (what, tr) in [("position", &a.position), ("rotation", &a.rotation)] {
                let Some(tr) = tr else { continue };
                if tr.times.len() != tr.values.len() {
                    cam_diags.push(Diagnostic::error(
                        "animations",
                        format!("{what} 轨道的关键帧 times 与 values 长度必须一致"),
                    ));
                }
                if tr.times.is_empty() {
                    cam_diags.push(Diagnostic::error("animations", "关键帧不能是空的"));
                }
                let mut prev = f32::NEG_INFINITY;
                for (k, t) in tr.times.iter().enumerate() {
                    if !t.is_finite() || *t < 0.0 {
                        cam_diags.push(Diagnostic::error(
                            "animations",
                            format!("{what} 轨道的关键帧时间必须是非负有限值"),
                        ));
                    }
                    if *t <= prev {
                        cam_diags.push(Diagnostic::error(
                            "animations",
                            format!(
                                "{what} 轨道的关键帧时间必须严格递增（同一时刻只能有一个取值）"
                            ),
                        ));
                    }
                    prev = *t;
                    if let Some(v) = tr.values.get(k) {
                        if v.iter().any(|c| !c.is_finite()) {
                            cam_diags.push(Diagnostic::error(
                                "animations",
                                format!("{what} 轨道的关键帧取值必须是有限数"),
                            ));
                        }
                    }
                }
                // 旋转关键帧是按**姿态**存的、段内走四元数 slerp，所以一段最多转 180°：
                // 跨度再大就走最短弧（0° → 360° 甚至退化成"完全不动"）。这是语义问题，
                // 不是数值误差——给警告，让人补中间帧，而不是让他对着"我写了 720° 它没转"发呆。
                if what == "rotation" {
                    for w in tr.values.windows(2) {
                        if (0..3).any(|c| (w[1][c] - w[0][c]).abs() > 180.0) {
                            cam_diags.push(Diagnostic::warn(
                                "animations",
                                format!(
                                    "{what} 有一段跨度超过 180°：朝向按四元数 slerp 走最短弧，\
实际转不到那么多（两个等效姿态之间还会退化成不动）——请在中间插关键帧"
                                ),
                            ));
                            break;
                        }
                    }
                }
            }
        }
        // 这三组检查（命名相机 / render.outputs / 关键帧动画）走"早退"路径：
        // 只要它们有问题就直接返回，不再往下跑语义校验。**guard 必须放在最后**——
        // 放在中间会让后面追加的诊断被静默丢掉（这是本轮真实踩到的 bug：
        // 动画时间不递增时 validate 报 valid: true）。
        if !cam_diags.is_empty() {
            return cam_diags;
        }
        // 有模板/实例时：先在**声明的**文档上做结构检查（引用、实例、名字、未用模板），
        // 再把其余一切交给展开后的文档验一遍。这样每条问题只报一次，
        // 而物体字段/材质/关节那些检查一个字都不用改。
        // 注意：展开之后 `objects[k]` 的下标是**展开后**的下标（instances 会变成多个物体）。
        if !self.templates.is_empty()
            || self
                .objects
                .iter()
                .any(|o| o.template.is_some() || o.instances.is_some())
        {
            let mut out = self.validate_template_layer();
            out.extend(self.expanded().validate());
            return out;
        }
        let mut out = Vec::new();

        // --- 规模上限：宁可给一条明确诊断，也不要跑到内存耗尽 / 卡死 ---
        if self.objects.len() > MAX_OBJECTS {
            out.push(
                Diagnostic::error(
                    "objects",
                    format!(
                        "物体数 {} 超过上限 {MAX_OBJECTS}（每个物体都会参与宽相/窄相，再大请拆成多个场景）",
                        self.objects.len()
                    ),
                )
                .with_hint("把大场景拆成若干个场景文件，或用 --steps 分帧推进"),
            );
        }
        if let Some(v) = self.physics.static_friction_speed {
            if !v.is_finite() || v < 0.0 {
                out.push(Diagnostic::error(
                    "physics.static_friction_speed",
                    "静/动摩擦的切换速度必须是非负的有限数",
                ));
            } else if v > 1.0 {
                out.push(
                    Diagnostic::warn(
                        "physics.static_friction_speed",
                        format!("阈值 {v} m/s 偏大：慢速滑行会被当成静止接触（表现为「滑到一半突然很黏」）"),
                    )
                    .with_hint("多数场景 0.01~0.1 就够"),
                );
            }
        }

        if self.simulate.steps > MAX_STEPS {
            out.push(
                Diagnostic::error(
                    "simulate.steps",
                    format!("步数 {} 超过上限 {MAX_STEPS}", self.simulate.steps),
                )
                .with_hint("分多次 simulate 推进，或减小步数"),
            );
        }
        if !(self.simulate.dt.is_finite() && self.simulate.dt > 0.0) {
            out.push(Diagnostic::error(
                "simulate.dt",
                format!("时间步长必须是有限正数，当前为 {}", self.simulate.dt),
            ));
        }

        if self.format != FORMAT {
            out.push(
                Diagnostic::error(
                    "format",
                    format!("未知格式 \"{}\"，应为 \"{FORMAT}\"", self.format),
                )
                .with_hint("可以省略 format 字段，缺省即为 quench3d.scene"),
            );
        }
        if self.version != VERSION {
            let hint = if self.version < VERSION {
                // 版本迁移策略（见 docs/gap_analysis.md #19）：1 → 2 的唯一差别是
                // v2 起 `objects[].parent` 生效。**没有 parent 的文件语义完全一样**，
                // 所以"迁移"就是把版本号改成 2 —— 不需要、也不该维护一个迁移器。
                format!(
                    "版本 1 → 2 的唯一差别是 `objects[].parent` 在 v2 才生效；                     文件里没有 parent 时语义完全一样，把 version 改成 {VERSION} 即可"
                )
            } else {
                format!("这个文件比当前版本新：请升级 quench3d（当前支持 {VERSION}）")
            };
            out.push(
                Diagnostic::error(
                    "version",
                    format!("不支持的版本 {}，当前支持 {VERSION}", self.version),
                )
                .with_hint(hint),
            );
        }
        if let Some(g) = &self.gravity {
            if !finite3(g) {
                out.push(Diagnostic::error("gravity", "重力必须是有限数"));
            }
        }

        // 材质
        for (name, m) in &self.materials {
            let p = format!("materials.{name}");
            if !finite3(&m.color) {
                out.push(Diagnostic::error(format!("{p}.color"), "颜色必须是有限数"));
            }
            if m.color.iter().any(|c| *c < 0.0 || *c > 1.0) {
                out.push(Diagnostic::warn(
                    format!("{p}.color"),
                    "颜色分量应在 0~1 之间",
                ));
            }
            if !(0.0..=1.0).contains(&m.metallic) {
                out.push(Diagnostic::error(
                    format!("{p}.metallic"),
                    "金属度应在 0~1 之间",
                ));
            }
            if !(0.05..=1.0).contains(&m.roughness) {
                out.push(Diagnostic::error(
                    format!("{p}.roughness"),
                    "粗糙度应在 0.05~1 之间",
                ));
            }
            if !m.emissive.is_finite() || m.emissive < 0.0 {
                out.push(Diagnostic::error(
                    format!("{p}.emissive"),
                    "自发光强度必须是非负有限数",
                ));
            }
            if let Some(a) = m.alpha {
                if !a.is_finite() || !(0.0..=1.0).contains(&a) {
                    out.push(Diagnostic::error(
                        format!("{p}.alpha"),
                        format!("不透明度应在 0~1 之间，当前为 {a}"),
                    ));
                }
            }
            if let Some(c) = &m.emissive_color {
                if !finite3(c) {
                    out.push(Diagnostic::error(
                        format!("{p}.emissive_color"),
                        "自发光颜色必须是有限数",
                    ));
                }
            }
            if let Some(u) = m.uv_scale {
                if !u.is_finite() || u <= 0.0 {
                    out.push(Diagnostic::error(
                        format!("{p}.uv_scale"),
                        format!("UV 缩放必须是正有限数，当前为 {u}"),
                    ));
                }
            }
            if let Some(n) = m.normal_scale {
                if !n.is_finite() {
                    out.push(Diagnostic::error(
                        format!("{p}.normal_scale"),
                        "法线强度必须是有限数",
                    ));
                }
            }
            if let Some(cut) = m.alpha_cutoff {
                if !cut.is_finite() || cut < 0.0 {
                    out.push(Diagnostic::error(
                        format!("{p}.alpha_cutoff"),
                        format!("alpha 裁剪阈值必须是非负有限数，当前为 {cut}"),
                    ));
                } else if cut > 0.0 && m.alpha_blend == Some(true) {
                    out.push(Diagnostic::warn(
                        format!("{p}.alpha_cutoff"),
                        "同时给了 alpha_cutoff 与 alpha_blend：裁剪优先，混合不会生效",
                    ));
                }
            }
            // 贴图：文件必须真的找得到；否则渲染时会静默变纯色（这类"静默"正是要消灭的）
            for (field, path, _srgb) in m.textures() {
                let Some(path) = path else { continue };
                let shown = format!("{p}.{field}");
                if path.trim().is_empty() {
                    out.push(Diagnostic::error(shown, "贴图路径不能为空"));
                    continue;
                }
                let lower = path.to_ascii_lowercase();
                if !(lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg"))
                {
                    out.push(Diagnostic::warn(
                        format!("{p}.{field}"),
                        format!("只支持 PNG/JPEG 贴图，当前是 \"{path}\""),
                    ));
                }
                if self.resolve_asset(path).is_none() {
                    let tried = self
                        .asset_candidates(path)
                        .iter()
                        .map(|c| c.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" / ");
                    out.push(
                        Diagnostic::error(
                            format!("{p}.{field}"),
                            format!("找不到贴图文件 \"{path}\"（找过：{tried}）"),
                        )
                        .with_hint("把贴图放到场景文件同目录，或写成绝对路径"),
                    );
                }
            }
        }

        // 点光源：数量上限、有限性、半径与强度
        let point_lights = self.lighting.point_lights.as_deref().unwrap_or(&[]);
        for (i, l) in point_lights.iter().enumerate() {
            let p = format!("lighting.point_lights[{i}]");
            if !finite3(&l.position) {
                out.push(Diagnostic::error(
                    format!("{p}.position"),
                    "点光源位置必须是有限数",
                ));
            }
            if !finite3(&l.color) {
                out.push(Diagnostic::error(
                    format!("{p}.color"),
                    "点光源颜色必须是有限数",
                ));
            }
            if !l.intensity.is_finite() || l.intensity < 0.0 {
                out.push(Diagnostic::error(
                    format!("{p}.intensity"),
                    "点光源强度必须是非负有限数",
                ));
            }
            if !l.radius.is_finite() || l.radius <= 0.0 {
                out.push(Diagnostic::error(
                    format!("{p}.radius"),
                    "点光源半径必须是正有限数",
                ));
            }
        }
        if point_lights.len() > MAX_POINT_LIGHTS {
            out.push(
                Diagnostic::warn(
                    "lighting.point_lights",
                    format!(
                        "给了 {} 盏点光源，但着色时最多 {} 盏生效，其余会被忽略",
                        point_lights.len(),
                        MAX_POINT_LIGHTS
                    ),
                )
                .with_hint("把远处的灯删掉，或把它们烘进自发光材质里"),
            );
        }

        // 雾：颜色有限且非负、密度非负有限
        if let Some(fog) = &self.lighting.fog {
            if let Some(c) = fog.color {
                if !finite3(&c) {
                    out.push(Diagnostic::error(
                        "lighting.fog.color",
                        "雾颜色必须是有限数",
                    ));
                } else if c.iter().any(|v| *v < 0.0) {
                    out.push(Diagnostic::error(
                        "lighting.fog.color",
                        "雾颜色不能是负数（线性空间的辐射值没有负的）",
                    ));
                } else if c.iter().any(|v| *v > 4.0) {
                    out.push(
                        Diagnostic::warn(
                            "lighting.fog.color",
                            format!("雾颜色 {c:?} 明显超过 1（线性空间），画面会很怪"),
                        )
                        .with_hint("雾颜色是线性值不是 sRGB：常见区间 0~1"),
                    );
                }
            }
            match fog.density {
                Some(d) if !d.is_finite() || d < 0.0 => out.push(Diagnostic::error(
                    "lighting.fog.density",
                    "雾密度必须是非负有限数",
                )),
                Some(d) if d > 1.0 => out.push(
                    Diagnostic::warn(
                        "lighting.fog.density",
                        format!("雾密度 {d} 太大（每米衰减），几米之外就全是雾了"),
                    )
                    .with_hint("常见区间 0.02~0.1：0.05 大约 20 米外看不清"),
                ),
                _ => {}
            }
        }
        // 阴影质量：强度 0~1、覆盖半径为正
        if let Some(q) = &self.lighting.shadow_quality {
            match q.strength {
                Some(v) if !v.is_finite() || !(0.0..=1.0).contains(&v) => out.push(
                    Diagnostic::error(
                        "lighting.shadow_quality.strength",
                        "阴影强度必须在 0~1 之间",
                    )
                    .with_hint("0 = 阴影不压暗（等于没有），1 = 全黑；真实场景 0.6~0.9 更自然"),
                ),
                _ => {}
            }
            match q.radius {
                Some(v) if !v.is_finite() || v <= 0.0 => out.push(Diagnostic::error(
                    "lighting.shadow_quality.radius",
                    "阴影覆盖半径必须是正有限数（米）",
                )),
                Some(v) if v > 400.0 => out.push(
                    Diagnostic::warn(
                        "lighting.shadow_quality.radius",
                        format!("阴影覆盖半径 {v} 米太大：分辨率摊得太薄，阴影会糊成一片"),
                    )
                    .with_hint("引擎会在 2~4000 米之间钳制；常规模拟给 10~60"),
                ),
                _ => {}
            }
            if let Some(n) = q.cascades {
                if !(1..=4).contains(&n) {
                    out.push(Diagnostic::error(
                        "lighting.shadow_quality.cascades",
                        "cascades 必须在 1~4 之间（1 = 单级；4 是 2x2 图集的上限）",
                    ));
                }
            }
            if let Some(l) = q.split_lambda {
                if !l.is_finite() || !(0.0..=1.0).contains(&l) {
                    out.push(Diagnostic::error(
                        "lighting.shadow_quality.split_lambda",
                        "split_lambda 必须在 0~1 之间（0 = 均匀切分，1 = 全对数切分）",
                    ));
                }
            }
        }
        // 天空颜色：三个通道都要有限非负
        if let Some(sky) = &self.lighting.sky_colors {
            for (name, c) in [
                ("zenith", sky.zenith),
                ("horizon", sky.horizon),
                ("ground", sky.ground),
            ] {
                let Some(c) = c else { continue };
                if !finite3(&c) {
                    out.push(Diagnostic::error(
                        format!("lighting.sky_colors.{name}"),
                        "天空颜色必须是有限数",
                    ));
                } else if c.iter().any(|v| *v < 0.0) {
                    out.push(Diagnostic::error(
                        format!("lighting.sky_colors.{name}"),
                        "天空颜色不能是负数",
                    ));
                }
            }
        }

        if let Some(g) = &self.ground {
            if !g.size.is_finite() || g.size <= 0.0 {
                out.push(Diagnostic::error("ground.size", "地面边长必须为正数"));
            }
            if !g.y.is_finite() {
                out.push(Diagnostic::error("ground.y", "地面高度必须是有限数"));
            }
            if !g.friction.is_finite() || g.friction < 0.0 {
                out.push(Diagnostic::error(
                    "ground.friction",
                    "地面摩擦系数必须是非负数",
                ));
            }
            if !(0.0..=1.0).contains(&g.restitution) {
                out.push(Diagnostic::error(
                    "ground.restitution",
                    "地面恢复系数应在 0~1 之间",
                ));
            }
            if let Some(m) = &g.material {
                if !self.materials.contains_key(m) {
                    out.push(
                        Diagnostic::error("ground.material", format!("未定义的材质 \"{m}\""))
                            .with_hint("在 materials 里补上，或删掉该字段使用默认材质"),
                    );
                }
            }
        }

        // 物体
        for (i, o) in self.objects.iter().enumerate() {
            let p = format!("objects[{i}]");
            // 形状：`shape` 与 `shapes` 二选一
            if o.shape.is_some() && o.shapes.is_some() {
                out.push(
                    Diagnostic::error(
                        format!("{p}.shapes"),
                        "shape 与 shapes 只能写一个（单形状用 shape，复合形状用 shapes）",
                    )
                    .with_hint("把单形状那个改写成 shapes 数组，或删掉多余的字段"),
                );
            }
            match (&o.shape, &o.shapes) {
                (Some(shape), None) => validate_shape(shape, &format!("{p}.shape"), &mut out),
                (None, Some(parts)) => {
                    if parts.is_empty() {
                        out.push(Diagnostic::error(
                            format!("{p}.shapes"),
                            "shapes 不能是空数组（至少一个部件）",
                        ));
                    }
                    for (i, part) in parts.iter().enumerate() {
                        let pp = format!("{p}.shapes[{i}]");
                        validate_shape(&part.shape, &format!("{pp}.shape"), &mut out);
                        if let Some(v) = &part.position {
                            if !finite3(v) {
                                out.push(Diagnostic::error(
                                    format!("{pp}.position"),
                                    "部件位置必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &part.rotation_deg {
                            if !finite3(v) {
                                out.push(Diagnostic::error(
                                    format!("{pp}.rotation_deg"),
                                    "部件欧拉角必须是有限数",
                                ));
                            }
                        }
                        if part.shape.kind == ShapeKind::Plane {
                            out.push(Diagnostic::warn(
                                &pp,
                                "无限半空间当复合形状的一个部件：整个刚体都会被它撑成无限大（通常只想当整个物体用）",
                            ));
                        }
                    }
                    if parts.len() == 1 {
                        out.push(Diagnostic::warn(
                            format!("{p}.shapes"),
                            "只写了一个部件：用 shape 更直接（行为完全一样）",
                        ));
                    }
                }
                (None, None) => {}
                // 两个都写了：上面已经报 error（这里不重复报，也不猜以谁为准）
                (Some(_), Some(_)) => {}
            }
            if let Some(v) = &o.position {
                if !finite3(v) {
                    out.push(Diagnostic::error(
                        format!("{p}.position"),
                        "位置必须是有限数",
                    ));
                }
            }
            if let Some(r) = &o.rotation_deg {
                if !finite3(r) {
                    out.push(Diagnostic::error(
                        format!("{p}.rotation_deg"),
                        "旋转必须是有限数",
                    ));
                }
            }
            if let Some(s) = &o.scale {
                if !finite3(s) || s.iter().any(|v| *v <= 0.0) {
                    out.push(Diagnostic::error(
                        format!("{p}.scale"),
                        "缩放必须是正有限数",
                    ));
                }
            }
            if let Some(m) = &o.material {
                if !self.materials.contains_key(m) {
                    out.push(
                        Diagnostic::error(format!("{p}.material"), format!("未定义的材质 \"{m}\""))
                            .with_hint("在 materials 里补上，或删掉该字段"),
                    );
                }
            }
            if let Some(d) = o.density {
                if !d.is_finite() || d <= 0.0 {
                    out.push(Diagnostic::error(format!("{p}.density"), "密度必须为正数"));
                }
            }
            if let Some(e) = o.restitution {
                if !(0.0..=1.0).contains(&e) {
                    out.push(Diagnostic::error(
                        format!("{p}.restitution"),
                        "恢复系数应在 0~1 之间",
                    ));
                }
            }
            if let Some(f) = o.friction {
                if !f.is_finite() || f < 0.0 {
                    out.push(Diagnostic::error(
                        format!("{p}.friction"),
                        "摩擦系数必须是非负数",
                    ));
                }
            }
            if let Some(sf) = o.static_friction {
                if !sf.is_finite() || sf < 0.0 {
                    out.push(Diagnostic::error(
                        format!("{p}.static_friction"),
                        "静摩擦系数必须是非负数",
                    ));
                } else if let Some(df) = o.friction {
                    if df.is_finite() && sf < df - 1e-6 {
                        out.push(
                            Diagnostic::warn(
                                format!("{p}.static_friction"),
                                format!(
                                    "静摩擦 {sf} 比动摩擦 {df} 小：不会更黏，只会更容易起步滑动"
                                ),
                            )
                            .with_hint("真实材质通常静摩擦 ≥ 动摩擦（想「推不动」就写大一点）"),
                        );
                    }
                }
            }
            if o.friction_combine.is_some() && o.body == BodyKind::None {
                out.push(Diagnostic::warn(
                    &p,
                    "这个物体没有刚体（body = none），friction_combine 不会生效",
                ));
            }
            if o.restitution_combine.is_some() && o.body == BodyKind::None {
                out.push(Diagnostic::warn(
                    &p,
                    "这个物体没有刚体（body = none），restitution_combine 不会生效",
                ));
            }
            if let Some(v) = &o.velocity {
                if !finite3(v) {
                    out.push(Diagnostic::error(
                        format!("{p}.velocity"),
                        "速度必须是有限数",
                    ));
                }
            }
            if let Some(v) = &o.angular_velocity {
                if !finite3(v) {
                    out.push(Diagnostic::error(
                        format!("{p}.angular_velocity"),
                        "角速度必须是有限数",
                    ));
                }
            }
            for (key, value) in [
                ("linear_damping", o.linear_damping),
                ("angular_damping", o.angular_damping),
            ] {
                if let Some(d) = value {
                    if !d.is_finite() || d < 0.0 {
                        out.push(Diagnostic::error(
                            format!("{p}.{key}"),
                            format!("{key} 必须是非负数"),
                        ));
                    }
                }
            }
            if let Some(g) = o.gravity_scale {
                if !g.is_finite() {
                    out.push(Diagnostic::error(
                        format!("{p}.gravity_scale"),
                        "重力缩放必须是有限数",
                    ));
                }
            }
            if let Some(m) = o.mass {
                if !m.is_finite() || m <= 0.0 {
                    out.push(
                        Diagnostic::error(format!("{p}.mass"), "质量必须为正数")
                            .with_hint("想按密度自动算质量就删掉 mass、只留 density"),
                    );
                }
            }
            // 这些字段只有动态刚体才消费：静默忽略是最难查的一类坑
            if o.body != BodyKind::Dynamic {
                let ignored: Vec<&str> = [
                    ("mass", o.mass.is_some()),
                    ("gravity_scale", o.gravity_scale.is_some()),
                    ("linear_damping", o.linear_damping.is_some()),
                    ("angular_damping", o.angular_damping.is_some()),
                    ("allow_sleep", o.allow_sleep.is_some()),
                    ("angular_velocity", o.angular_velocity.is_some()),
                ]
                .iter()
                .filter(|(_, given)| *given)
                .map(|(name, _)| *name)
                .collect();
                if !ignored.is_empty() {
                    out.push(Diagnostic::warn(
                        &p,
                        format!(
                            "body 是 {:?}，这些字段只对 dynamic 生效，会被忽略：{}",
                            o.body,
                            ignored.join(", ")
                        ),
                    ));
                }
            }
            let has_plane = o
                .parts()
                .iter()
                .any(|part| part.shape.kind == ShapeKind::Plane);
            if has_plane && o.body == BodyKind::Dynamic {
                out.push(
                    Diagnostic::error(
                        format!("{p}.body"),
                        "平面不能是动态刚体（无限大物体没有有限质量）",
                    )
                    .with_hint("把 body 改成 \"static\""),
                );
            }
            if o.body == BodyKind::None && o.velocity.is_some() {
                out.push(Diagnostic::warn(
                    format!("{p}.velocity"),
                    "物体没有刚体，速度不会生效",
                ));
            }
            // 碰撞过滤：层号只有 0~31（掩码是 32 位）
            if let Some(layer) = o.collision_layer {
                if layer > 31 {
                    out.push(
                        Diagnostic::error(
                            format!("{p}.collision_layer"),
                            format!("碰撞层 {layer} 超出范围（0~31）"),
                        )
                        .with_hint("层号对应 32 位掩码的一位，最多 32 层"),
                    );
                }
            }
            if let Some(mask) = &o.collides_with {
                for (k, layer) in mask.iter().enumerate() {
                    if *layer > 31 {
                        out.push(Diagnostic::error(
                            format!("{p}.collides_with[{k}]"),
                            format!("层号 {layer} 超出范围（0~31）"),
                        ));
                    }
                }
                let mut seen = mask.clone();
                seen.sort_unstable();
                let before = seen.len();
                seen.dedup();
                if seen.len() != before {
                    out.push(Diagnostic::warn(
                        format!("{p}.collides_with"),
                        "层号有重复，重复项没有额外作用",
                    ));
                }
                if mask.is_empty() {
                    out.push(
                        Diagnostic::warn(
                            format!("{p}.collides_with"),
                            "空列表 = 谁也不碰：这个物体会穿过所有东西",
                        )
                        .with_hint("想让它和所有层碰就删掉本字段"),
                    );
                }
                if o.body == BodyKind::None {
                    out.push(Diagnostic::warn(
                        format!("{p}.collides_with"),
                        "物体没有刚体，碰撞过滤不会生效",
                    ));
                }
                // 只和自己那一层碰是常见写法；但若连自己那层都不含，容易看漏
                if let Some(layer) = o.collision_layer {
                    if !mask.contains(&layer) {
                        out.push(Diagnostic::warn(
                            format!("{p}.collides_with"),
                            format!("列表里不含自己的层 {layer}，它不会和同层物体碰撞"),
                        ));
                    }
                }
            }
            // 传感器：必须是刚体，且密度/质量/none 之类会静默失效的字段要说清楚
            if o.sensor == Some(true) {
                if o.body == BodyKind::None {
                    out.push(
                        Diagnostic::error(
                            format!("{p}.sensor"),
                            "传感器需要刚体（body 是 \"none\" 的物体不参与物理）",
                        )
                        .with_hint("写成 \"static\"（固定触发区）或 \"dynamic\"（跟着动的触发区）"),
                    );
                }
                if o.density.is_some() || o.mass.is_some() {
                    out.push(Diagnostic::warn(
                        &p,
                        "传感器不产生冲量，密度/质量对它没有意义",
                    ));
                }
                if o.restitution.is_some() || o.friction.is_some() {
                    out.push(Diagnostic::warn(
                        &p,
                        "传感器不产生冲量，恢复系数/摩擦对它没有意义",
                    ));
                }
                if has_plane {
                    out.push(Diagnostic::warn(
                        format!("{p}.sensor"),
                        "无限半空间当传感器：整个空间永远在里面，事件只在开始时报一次",
                    ));
                }
            }
            // CCD：只对动态刚体有意义
            if o.ccd == Some(true) {
                if o.body != BodyKind::Dynamic {
                    out.push(Diagnostic::warn(
                        format!("{p}.ccd"),
                        format!("body 是 {:?}，CCD 只对 dynamic 生效，会被忽略", o.body),
                    ));
                }
                if has_plane {
                    out.push(Diagnostic::warn(
                        format!("{p}.ccd"),
                        "无限半空间不需要 CCD（它不会动）",
                    ));
                }
            }
            // scale 是渲染专用的：碰撞形状取 shape 里的原始尺寸，这里放大会静默对不上
            if o.scale.is_some() && o.body != BodyKind::None {
                out.push(
                    Diagnostic::warn(
                        format!("{p}.scale"),
                        "scale 只影响渲染，碰撞形状仍用 shape 的原始尺寸",
                    )
                    .with_hint("要真的放大就改 shape 的 size/radius，否则视觉与碰撞对不上"),
                );
            }
        }

        // 重名：物体名是关节、轨迹跟踪与报告索引的键，重名会让先出现的那个被静默覆盖
        let mut seen_names: BTreeMap<&str, usize> = BTreeMap::new();
        for (i, o) in self.objects.iter().enumerate() {
            if let Some(n) = o.name.as_deref() {
                if let Some(first) = seen_names.insert(n, i) {
                    out.push(
                        Diagnostic::error(
                            format!("objects[{i}].name"),
                            format!("物体名 \"{n}\" 与 objects[{first}] 重复"),
                        )
                        .with_hint("名字用于关节两端、--trace 与报告索引，必须唯一"),
                    );
                }
            }
        }

        // 父子：名字解析 → 成环 → 与刚体的冲突。
        // 这三条都在构建场景**之前**查出来：悬空的名字在构建时找不到，环会让"世界变换"
        // 只能在运行时截断，而带刚体的物体挂父物体则是静默失效 —— 三种都不该留到运行时。
        for (i, o) in self.objects.iter().enumerate() {
            let p = format!("objects[{i}].parent");
            let Some(parent) = o.parent.as_deref() else {
                continue;
            };
            if parent.trim().is_empty() {
                out.push(Diagnostic::error(p, "父物体名不能为空"));
                continue;
            }
            if o.name.as_deref() == Some(parent) {
                out.push(
                    Diagnostic::error(&p, "物体不能以自己为父")
                        .with_hint("删掉 parent 字段，或用另一个物体的名字"),
                );
                continue;
            }
            if !seen_names.contains_key(parent) {
                out.push(
                    Diagnostic::error(
                        &p,
                        format!("找不到名为 \"{parent}\" 的父物体（父物体必须写在 objects 里并显式命名）"),
                    )
                    .with_hint("给父物体补一个 name，或把 parent 改成它的名字"),
                );
                continue;
            }
            if o.body != BodyKind::None {
                out.push(
                    Diagnostic::error(
                        &p,
                        format!(
                            "objects[{i}] 是 {:?} 刚体，位姿由物理决定，不能再挂父物体",
                            o.body
                        ),
                    )
                    .with_hint("要么去掉 parent，要么把 body 改成 none"),
                );
            }
        }
        // 成环：沿父链走，走不动之前又回到自己就是环。
        // 解析不了的父名当成"链到此为止"，这样一条悬空引用不会把成环检查也一起关掉。
        if self.objects.len() <= MAX_OBJECTS {
            let index_of = |name: &str| seen_names.get(name).copied();
            for (i, o) in self.objects.iter().enumerate() {
                let mut cursor = o.parent.as_deref().and_then(index_of);
                let mut walked: Vec<usize> = Vec::new();
                while let Some(j) = cursor {
                    if j == i {
                        // 环上的每个物体沿父链都会走回自己，只让下标最小的那个报一次，
                        // 否则一个三物体的环会刷出三条一模一样的诊断。
                        if walked.iter().all(|&w| w > i) {
                            out.push(
                                Diagnostic::error(
                                    format!("objects[{i}].parent"),
                                    format!(
                                        "父子链成环（从 objects[{i}] 出发沿 parent 又回到自己）"
                                    ),
                                )
                                .with_hint("层级必须是一棵树：去掉环上的某一条 parent"),
                            );
                        }
                        break;
                    }
                    walked.push(j);
                    cursor = self.objects[j].parent.as_deref().and_then(index_of);
                    if walked.len() > self.objects.len() {
                        break;
                    }
                }
            }
        }

        // 资源搜索路径：空项是错误；不存在的目录给警告（打错一个字就静默失效最难查）
        if let Some(assets) = &self.assets {
            for (i, sp) in assets.search_paths.iter().enumerate() {
                let p = format!("assets.search_paths[{i}]");
                if sp.as_os_str().is_empty() {
                    out.push(Diagnostic::error(p, "搜索路径不能为空"));
                    continue;
                }
                let dir = if sp.is_absolute() {
                    sp.clone()
                } else {
                    match &self.base_dir {
                        Some(base) => base.join(sp),
                        None => sp.clone(),
                    }
                };
                if !dir.is_dir() {
                    out.push(
                        Diagnostic::warn(
                            p,
                            format!("搜索路径 {} 不是一个存在的目录", dir.display()),
                        )
                        .with_hint("相对路径以场景文件所在目录为基准"),
                    );
                }
            }
        }

        // 模型
        for (i, m) in self.models.iter().enumerate() {
            let p = format!("models[{i}]");
            if m.path.as_os_str().is_empty() {
                out.push(Diagnostic::error(format!("{p}.path"), "模型路径不能为空"));
            } else {
                let ext = m
                    .path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if !matches!(ext.as_str(), "gltf" | "glb" | "obj") {
                    out.push(Diagnostic::error(
                        format!("{p}.path"),
                        format!("不支持的模型格式 \".{ext}\""),
                    ));
                }
                // 与贴图共用同一套候选（场景目录 → assets.search_paths → 工作目录 →
                // 引擎搜索路径）。以前这里用的是加载器自己的规则，和渲染路径**不一致**：
                // 场景目录里的模型会被判成"找不到"，而靠 assets/ 搜到的又可能被误判。
                match self.resolve_model(&m.path) {
                    Some(found) => {
                        if found != m.path {
                            out.push(Diagnostic::warn(
                                format!("{p}.path"),
                                format!("文件不在写的路径上，实际会用 {}", found.display()),
                            ));
                        }
                    }
                    None => {
                        let tried = self
                            .model_candidates(&m.path)
                            .iter()
                            .map(|c| c.display().to_string())
                            .collect::<Vec<_>>()
                            .join(" / ");
                        out.push(
                            Diagnostic::error(
                                format!("{p}.path"),
                                format!("找不到模型文件 \"{}\"（找过：{tried}）", m.path.display()),
                            )
                            .with_hint(
                                "把模型放到场景文件同目录、写进 assets.search_paths，\
                                 或写成绝对路径",
                            ),
                        );
                    }
                }
            }
            if let Some(s) = m.scale {
                if !s.is_finite() || s <= 0.0 {
                    out.push(Diagnostic::error(format!("{p}.scale"), "缩放必须是正数"));
                }
            }
        }

        // 关节
        let names: BTreeMap<&str, &ObjectDoc> = self
            .objects
            .iter()
            .filter_map(|o| o.name.as_deref().map(|n| (n, o)))
            .collect();
        for (i, j) in self.joints.iter().enumerate() {
            let p = format!("joints[{i}]");
            let mut ends = Vec::new();
            for (field, name) in [("a", &j.a), ("b", &j.b)] {
                match names.get(name.as_str()) {
                    None => out.push(
                        Diagnostic::error(
                            format!("{p}.{field}"),
                            format!("找不到名为 \"{name}\" 的物体"),
                        )
                        .with_hint("关节两端必须是 objects 里带 name 的物体"),
                    ),
                    Some(o) => {
                        if o.body == BodyKind::None {
                            out.push(
                                Diagnostic::error(
                                    format!("{p}.{field}"),
                                    format!("物体 \"{name}\" 没有刚体，不能参与关节"),
                                )
                                .with_hint("给它加上 \"body\": \"dynamic\" 或 \"static\""),
                            );
                        }
                        ends.push(o);
                    }
                }
            }
            if ends.len() == 2
                && ends[0].body == BodyKind::Static
                && ends[1].body == BodyKind::Static
            {
                out.push(Diagnostic::warn(&p, "关节两端都是静态刚体，不会有任何运动"));
            }
            if j.kind == JointKind::Distance && j.rest_length.is_some_and(|l| l < 0.0) {
                out.push(Diagnostic::error(
                    format!("{p}.rest_length"),
                    "静止长度不能为负",
                ));
            }
            if let Some(l) = &j.limits {
                if !l[0].is_finite() || !l[1].is_finite() {
                    out.push(Diagnostic::error(format!("{p}.limits"), "限位必须是有限数"));
                } else if l[0] > l[1] {
                    out.push(Diagnostic::error(
                        format!("{p}.limits"),
                        format!("下限 {} 大于上限 {}", l[0], l[1]),
                    ));
                }
            }
            // 零向量/非有限轴会被 `normalize_or_zero` 静默变成零轴，关节行为不可预期
            if let Some(axis) = &j.axis {
                let bad = axis.iter().any(|v| !v.is_finite());
                let zero =
                    !bad && (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]) < 1e-12;
                if bad {
                    out.push(Diagnostic::error(format!("{p}.axis"), "轴向必须是有限数"));
                } else if zero {
                    out.push(
                        Diagnostic::error(format!("{p}.axis"), "轴向不能是零向量")
                            .with_hint("铰链/滑块的轴必须是某个方向，例如 [0, 1, 0]"),
                    );
                }
            }
            for (key, anchor) in [("anchor_a", &j.anchor_a), ("anchor_b", &j.anchor_b)] {
                if let Some(v) = anchor {
                    if !finite3(v) {
                        out.push(Diagnostic::error(format!("{p}.{key}"), "锚点必须是有限数"));
                    }
                }
            }
            if j.kind != JointKind::Hinge && j.motor_velocity.is_some() {
                out.push(Diagnostic::warn(
                    format!("{p}.motor_velocity"),
                    "只有铰链关节支持马达",
                ));
            }
            // 单位陷阱：铰链的角度量都是弧度，而物体的 rotation_deg 是角度
            if j.kind == JointKind::Hinge {
                if let Some(l) = &j.limits {
                    if l.iter().any(|v| v.abs() > 6.5) {
                        out.push(
                            Diagnostic::warn(
                                format!("{p}.limits"),
                                format!(
                                    "铰链限位单位是弧度，当前最大 {:.2} rad ≈ {:.0}°",
                                    l[0].abs().max(l[1].abs()),
                                    l[0].abs().max(l[1].abs()).to_degrees()
                                ),
                            )
                            .with_hint("写的是角度就除以 57.2958（90° → 1.5708）"),
                        );
                    }
                }
                if let Some(v) = j.motor_velocity {
                    if v.abs() > 100.0 {
                        out.push(
                            Diagnostic::warn(
                                format!("{p}.motor_velocity"),
                                format!("马达速度单位是弧度/秒，当前 {v}"),
                            )
                            .with_hint("写的是角度/秒就除以 57.2958"),
                        );
                    }
                }
                if j.motor_target_deg.is_some() && j.motor_velocity.is_some() {
                    out.push(
                        Diagnostic::warn(
                            format!("{p}.motor_velocity"),
                            "同时给了 motor_target_deg 与 motor_velocity：按伺服模式走，速度会被忽略",
                        )
                        .with_hint("伺服模式请只给 motor_target_deg（可选 motor_stiffness / motor_damping）"),
                    );
                }
            } else {
                for (key, given) in [
                    ("motor_target_deg", j.motor_target_deg.is_some()),
                    ("motor_stiffness", j.motor_stiffness.is_some()),
                    ("motor_damping", j.motor_damping.is_some()),
                ] {
                    if given {
                        out.push(Diagnostic::warn(
                            format!("{p}.{key}"),
                            "只有铰链关节支持马达",
                        ));
                    }
                }
            }
            if let Some(deg) = j.motor_target_deg {
                if !deg.is_finite() {
                    out.push(Diagnostic::error(
                        format!("{p}.motor_target_deg"),
                        "伺服目标角必须是有限数",
                    ));
                } else if deg.abs() > 3600.0 {
                    out.push(Diagnostic::warn(
                        format!("{p}.motor_target_deg"),
                        format!("伺服目标角 {deg}° 不太像个角度（单位是度）"),
                    ));
                }
            }
            for (key, v) in [
                ("motor_stiffness", j.motor_stiffness),
                ("motor_damping", j.motor_damping),
            ] {
                if let Some(v) = v {
                    if !v.is_finite() || v < 0.0 {
                        out.push(Diagnostic::error(
                            format!("{p}.{key}"),
                            "伺服增益必须是非负的有限数",
                        ));
                    }
                }
            }
            // 锥形限位：只有球窝有
            match (&j.cone_limit, j.kind) {
                (Some(cone), JointKind::Ball) => {
                    if !cone.half_angle_deg.is_finite()
                        || cone.half_angle_deg < 0.0
                        || cone.half_angle_deg > 180.0
                    {
                        out.push(Diagnostic::error(
                            format!("{p}.cone_limit.half_angle_deg"),
                            "锥形限位的半角必须在 0~180 度之间",
                        ));
                    } else if cone.half_angle_deg < 0.5 {
                        out.push(
                            Diagnostic::warn(
                                format!("{p}.cone_limit.half_angle_deg"),
                                "半角几乎是 0：这等于把球窝锁死，通常想要的是 fixed 关节",
                            )
                            .with_hint("想要完全固定就用 \"type\": \"fixed\""),
                        );
                    }
                    for (key, axis) in [("axis_a", &cone.axis_a), ("axis_b", &cone.axis_b)] {
                        if let Some(a) = axis {
                            let zero = a.iter().all(|v| v.abs() < 1e-6);
                            if !finite3(a) {
                                out.push(Diagnostic::error(
                                    format!("{p}.cone_limit.{key}"),
                                    "锥形限位的轴必须是有限数",
                                ));
                            } else if zero {
                                out.push(Diagnostic::error(
                                    format!("{p}.cone_limit.{key}"),
                                    "锥形限位的轴不能是零向量",
                                ));
                            }
                        }
                    }
                }
                (Some(_), _) => out.push(Diagnostic::error(
                    format!("{p}.cone_limit"),
                    "锥形限位只有球窝关节（type = ball）支持",
                )),
                (None, _) => {}
            }
            // 关节两端默认不互相碰撞；显式打开是"我知道会互相挤"的意思，
            // 静态-静态这种组合本来就警告过，这里不再重复。
            // 断裂阈值：> 0 才有意义（0/负值等于"立刻断"或"永远不断"，两种都是笔误）
            for (key, v) in [
                ("break_force", j.break_force),
                ("break_torque", j.break_torque),
            ] {
                if let Some(v) = v {
                    let unit = if key == "break_force" { "N" } else { "N·m" };
                    if !v.is_finite() || v <= 0.0 {
                        out.push(Diagnostic::error(
                            format!("{p}.{key}"),
                            format!("断裂阈值必须是正数（{unit}）；不想让它断就别写这个字段"),
                        ));
                    } else if v < 0.5 {
                        out.push(Diagnostic::warn(
                            format!("{p}.{key}"),
                            format!("断裂阈值只有 {v} {unit}：几乎一上力就断"),
                        ));
                    }
                }
            }
        }

        // --- 定时动作与区域探针 ---
        // 目标名解析：物体（名字缺省是 `objects[i]`）、`$ground`、有碰撞体的 `$model[i]`。
        // 地面与模型网格都是静态刚体——这正是"冲量打上去没反应"最常见的原因，所以要警告。
        let mut bodies: BTreeMap<String, BodyKind> = BTreeMap::new();
        for (i, o) in self.objects.iter().enumerate() {
            let name = o.name.clone().unwrap_or_else(|| format!("objects[{i}]"));
            bodies.insert(name, o.body);
        }
        if self.ground.is_some() {
            bodies.insert("$ground".to_string(), BodyKind::Static);
        }
        for (i, m) in self.models.iter().enumerate() {
            if m.collide {
                bodies.insert(format!("$model[{i}]"), BodyKind::Static);
            }
        }
        let body_name_hint =
            "target 用 objects[].name（没写 name 就是 objects[i]）、$ground 或 $model[i]";

        if self.actions.len() > MAX_ACTIONS {
            out.push(
                Diagnostic::error(
                    "actions",
                    format!("动作数 {} 超过上限 {MAX_ACTIONS}", self.actions.len()),
                )
                .with_hint("把长脚本拆成多次 simulate，或减少动作数量"),
            );
        }
        for (i, a) in self.actions.iter().enumerate() {
            let p = format!("actions[{i}]");
            let kind = a.kind.name();

            // 时序：步号与秒数二选一
            if a.at_step.is_some() && a.at_time.is_some() {
                out.push(
                    Diagnostic::error(
                        format!("{p}.at_time"),
                        "at_step 与 at_time 只能给一个（给了两个不知道听谁的）",
                    )
                    .with_hint("at_time 是秒数写法，内部按 round(t / dt) 折算成步号"),
                );
            }
            let eff_step = match (a.at_step, a.at_time) {
                (Some(s), _) => Some(s),
                (None, Some(t)) if t.is_finite() && t >= 0.0 && self.simulate.dt > 0.0 => {
                    Some((t / self.simulate.dt).round() as u32)
                }
                _ => None,
            };
            if let Some(t) = a.at_time {
                if !t.is_finite() || t < 0.0 {
                    out.push(Diagnostic::error(
                        format!("{p}.at_time"),
                        format!("时间点必须是有限的非负数，当前为 {t}"),
                    ));
                }
            }
            if self.simulate.steps == 0 {
                out.push(
                    Diagnostic::warn(&p, "simulate.steps = 0，一步都不跑，动作不会触发")
                        .with_hint("给 simulate.steps 一个正数（例如 120）"),
                );
            }
            if let Some(step) = eff_step {
                if self.simulate.steps > 0 && step > self.simulate.steps {
                    out.push(
                        Diagnostic::warn(
                            &p,
                            format!(
                                "在第 {step} 步施加，但 simulate.steps = {}，这个动作不会触发",
                                self.simulate.steps
                            ),
                        )
                        .with_hint("调大 simulate.steps，或把 at_step / at_time 提前"),
                    );
                }
            }

            // 参数：缺了必需的、或者给了用不上的，都要说清楚（静默忽略最难查）
            match &a.value {
                None if a.kind.needs_value() => out.push(
                    Diagnostic::error(
                        format!("{p}.value"),
                        format!("{kind} 动作需要 value（[x, y, z]）"),
                    )
                    .with_hint(match a.kind {
                        ActionKind::Impulse | ActionKind::ImpulseAtPoint => "单位是 N·s",
                        ActionKind::TorqueImpulse => "单位是 N·m·s",
                        ActionKind::Velocity | ActionKind::AngularVelocity => "单位是 m/s 或 rad/s",
                        ActionKind::Force => "单位是 N",
                        _ => "给一个方向与大小",
                    }),
                ),
                Some(v) if !finite3(v) => out.push(Diagnostic::error(
                    format!("{p}.value"),
                    "value 必须是有限数",
                )),
                Some(_) if !a.kind.needs_value() => out.push(Diagnostic::warn(
                    format!("{p}.value"),
                    format!("{kind} 动作不需要 value，会被忽略"),
                )),
                _ => {}
            }
            if a.kind.uses_point() {
                match &a.point {
                    None => out.push(Diagnostic::error(
                        format!("{p}.point"),
                        "impulse_at_point 需要 point（世界坐标的作用点）",
                    )),
                    Some(q) if !finite3(q) => out.push(Diagnostic::error(
                        format!("{p}.point"),
                        "作用点必须是有限数",
                    )),
                    Some(_) => {}
                }
            } else if a.point.is_some() {
                out.push(Diagnostic::warn(
                    format!("{p}.point"),
                    format!("只有 impulse_at_point 用 point，{kind} 会忽略它"),
                ));
            }
            if a.kind.uses_position() {
                match &a.position {
                    None => out.push(Diagnostic::error(
                        format!("{p}.position"),
                        "teleport 需要 position（目标位置）",
                    )),
                    Some(v) if !finite3(v) => out.push(Diagnostic::error(
                        format!("{p}.position"),
                        "目标位置必须是有限数",
                    )),
                    Some(_) => {}
                }
                if a.rotation_deg.as_ref().is_some_and(|r| !finite3(r)) {
                    out.push(Diagnostic::error(
                        format!("{p}.rotation_deg"),
                        "目标朝向必须是有限数",
                    ));
                }
            } else {
                if a.position.is_some() {
                    out.push(Diagnostic::warn(
                        format!("{p}.position"),
                        format!("只有 teleport 用 position，{kind} 会忽略它"),
                    ));
                }
                if a.rotation_deg.is_some() {
                    out.push(Diagnostic::warn(
                        format!("{p}.rotation_deg"),
                        format!("只有 teleport 用 rotation_deg，{kind} 会忽略它"),
                    ));
                }
            }
            if a.kind.uses_steps() {
                if a.steps == 0 {
                    out.push(Diagnostic::warn(
                        format!("{p}.steps"),
                        "steps = 0，这个力一步都不会施加",
                    ));
                }
            } else if a.steps != 1 {
                out.push(Diagnostic::warn(
                    format!("{p}.steps"),
                    format!("steps 只对 force 生效，{kind} 会忽略它"),
                ));
            }

            // `target: "*"` 是 `wake` 专用的特殊值：唤醒全部刚体
            if a.target == "*" {
                if a.kind != ActionKind::Wake {
                    out.push(
                        Diagnostic::error(
                            format!("{p}.target"),
                            format!("target \"*\" 只有 wake 动作支持，{kind} 需要一个具体刚体名"),
                        )
                        .with_hint("把它换成一个物体名，或把 type 改成 wake"),
                    );
                }
            } else {
                // 目标必须是能动的刚体
                match bodies.get(a.target.as_str()) {
                    None => out.push(
                        Diagnostic::error(
                            format!("{p}.target"),
                            format!("找不到名为 \"{}\" 的刚体", a.target),
                        )
                        .with_hint(body_name_hint),
                    ),
                    Some(BodyKind::None) => out.push(
                        Diagnostic::error(
                            format!("{p}.target"),
                            format!(
                                "物体 \"{}\" 没有刚体（body = none），动作不会生效",
                                a.target
                            ),
                        )
                        .with_hint("把它的 body 改成 dynamic"),
                    ),
                    Some(BodyKind::Static) if a.kind.pushes() => out.push(
                        Diagnostic::warn(
                            &p,
                            format!(
                                "\"{}\" 是静态刚体（无限质量），{kind} 打上去不会有任何效果",
                                a.target
                            ),
                        )
                        .with_hint("要推动它就先把它改成 dynamic；只想记录位置可以用 probes"),
                    ),
                    Some(_) => {}
                }
            }
        }

        if self.probes.len() > MAX_PROBES {
            out.push(
                Diagnostic::error(
                    "probes",
                    format!("探针数 {} 超过上限 {MAX_PROBES}", self.probes.len()),
                )
                .with_hint("每个探针每步都要做一次区域查询，探针太多会拖慢仿真"),
            );
        }
        let mut probe_names: BTreeSet<&str> = BTreeSet::new();
        for (i, pr) in self.probes.iter().enumerate() {
            let p = format!("probes[{i}]");
            if pr.name.trim().is_empty() {
                out.push(Diagnostic::error(
                    format!("{p}.name"),
                    "探针名不能为空（报告里用它标识）",
                ));
            } else if !probe_names.insert(pr.name.as_str()) {
                out.push(Diagnostic::error(
                    format!("{p}.name"),
                    format!("探针名 \"{}\" 重复，报告里无法区分", pr.name),
                ));
            }
            if !finite3(&pr.position) {
                out.push(Diagnostic::error(
                    format!("{p}.position"),
                    "探针中心必须是有限数",
                ));
            }
            match pr.shape {
                ProbeShapeDoc::Sphere => {
                    if !(pr.radius.is_finite() && pr.radius > 0.0) {
                        out.push(Diagnostic::error(
                            format!("{p}.radius"),
                            format!("球形探针半径必须是有限正数，当前为 {}", pr.radius),
                        ));
                    }
                    if pr.size.is_some() {
                        out.push(
                            Diagnostic::warn(format!("{p}.size"), "球形探针不用 size，会被忽略")
                                .with_hint("要盒形区域就把 shape 写成 \"box\""),
                        );
                    }
                }
                ProbeShapeDoc::Box => {
                    if let Some(sz) = pr.size {
                        if !finite3(&sz) || sz.iter().any(|v| *v <= 0.0) {
                            out.push(Diagnostic::error(
                                format!("{p}.size"),
                                "盒形探针边长必须是有限正数",
                            ));
                        }
                    }
                }
            }
            for (k, name) in pr.watch.iter().enumerate() {
                match bodies.get(name.as_str()) {
                    None => out.push(
                        Diagnostic::error(
                            format!("{p}.watch[{k}]"),
                            format!("找不到名为 \"{name}\" 的刚体"),
                        )
                        .with_hint(body_name_hint),
                    ),
                    Some(BodyKind::None) => out.push(Diagnostic::error(
                        format!("{p}.watch[{k}]"),
                        format!("物体 \"{name}\" 没有刚体，位置永远不会变"),
                    )),
                    Some(_) => {}
                }
            }
            if self.simulate.steps == 0 {
                out.push(
                    Diagnostic::warn(&p, "simulate.steps = 0，探针不会观测到任何东西")
                        .with_hint("给 simulate.steps 一个正数（例如 120）"),
                );
            }
        }

        // Bloom：数值必须合理，且"开了但强度为 0"要提醒（那等于没开）
        if let Some(b) = &self.lighting.bloom {
            let enabled = b.enabled.unwrap_or(true);
            if let Some(i) = b.intensity {
                if !i.is_finite() || i < 0.0 {
                    out.push(Diagnostic::error(
                        "lighting.bloom.intensity",
                        "bloom 强度必须是非负数",
                    ));
                } else if i == 0.0 && enabled {
                    out.push(
                        Diagnostic::warn(
                            "lighting.bloom.intensity",
                            "bloom 开着但强度为 0，等于没开",
                        )
                        .with_hint("想要亮区外溢就给个正数（例如 0.6），或者直接去掉 bloom 这一节"),
                    );
                }
            }
            if let Some(t) = b.threshold {
                if !t.is_finite() || t < 0.0 {
                    out.push(Diagnostic::error(
                        "lighting.bloom.threshold",
                        "bloom 阈值必须是非负数",
                    ));
                } else if t > 20.0 {
                    out.push(
                        Diagnostic::warn(
                            "lighting.bloom.threshold",
                            format!("阈值 {t} 超过了太阳圆盘的亮度，bloom 基本不会触发"),
                        )
                        .with_hint("常用 0.8~2.0；阈值是线性空间亮度，不是 0~255"),
                    );
                }
            }
        }

        // FXAA：阈值必须落在 (0, 1) 这个量级，否则要么永远不触发要么糊成一片
        if let Some(f) = &self.lighting.fxaa {
            let enabled = f.enabled.unwrap_or(true);
            for (key, value) in [
                ("lighting.fxaa.edge_threshold", f.edge_threshold),
                ("lighting.fxaa.edge_threshold_min", f.edge_threshold_min),
            ] {
                let Some(v) = value else { continue };
                if !v.is_finite() || v < 0.0 {
                    out.push(Diagnostic::error(key, "FXAA 阈值必须是非负数"));
                } else if v > 1.0 && enabled {
                    out.push(
                        Diagnostic::warn(key, format!("阈值 {v} 大于 1，几乎不会判定出边缘"))
                            .with_hint(
                                "亮度差是 0~1 的量：常用 edge_threshold 0.166、edge_threshold_min 0.0833",
                            ),
                    );
                }
            }
        }

        // SSAO：半径/偏移/强度/采样数都得是能用的量（"开了但值为 0"等于没开）
        if let Some(a) = &self.lighting.ssao {
            let enabled = a.enabled.unwrap_or(true);
            if let Some(r) = a.radius {
                if !r.is_finite() || r <= 0.0 {
                    out.push(Diagnostic::error(
                        "lighting.ssao.radius",
                        "SSAO 半径必须是正数（单位：米）",
                    ));
                } else if r > 20.0 {
                    out.push(
                        Diagnostic::warn(
                            "lighting.ssao.radius",
                            format!("半径 {r} 米太大，几乎整屏都会被压暗"),
                        )
                        .with_hint("物体尺度 1~2 米时用 0.5~1.0"),
                    );
                }
            }
            if let Some(b) = a.bias {
                if !b.is_finite() || b < 0.0 {
                    out.push(Diagnostic::error(
                        "lighting.ssao.bias",
                        "SSAO 深度偏移不能是负数",
                    ));
                } else if let Some(r) = a.radius {
                    if r.is_finite() && r > 0.0 && b >= r {
                        out.push(
                            Diagnostic::warn(
                                "lighting.ssao.bias",
                                format!("偏移 {b} 不小于半径 {r}，遮挡会被全部滤掉"),
                            )
                            .with_hint("bias 是压自遮蔽用的小量，通常 0.01~0.05"),
                        );
                    }
                }
            }
            if let Some(st) = a.strength {
                if !st.is_finite() || st < 0.0 {
                    out.push(Diagnostic::error(
                        "lighting.ssao.strength",
                        "SSAO 强度不能是负数",
                    ));
                } else if st == 0.0 && enabled {
                    out.push(
                        Diagnostic::warn("lighting.ssao.strength", "SSAO 开着但强度为 0，等于没开")
                            .with_hint("想要接触阴影就给个正数（例如 1.0），或者去掉 ssao 这一节"),
                    );
                } else if st > 1.0 {
                    out.push(Diagnostic::warn(
                        "lighting.ssao.strength",
                        format!("强度 {st} 大于 1，接触处会被压得过黑"),
                    ));
                }
            }
            if let Some(n) = a.samples {
                if !(1..=64).contains(&n) {
                    out.push(
                        Diagnostic::error(
                            "lighting.ssao.samples",
                            format!("SSAO 采样数 {n} 超出范围（1~64）"),
                        )
                        .with_hint("16 是画质/开销的平衡点；超过 32 收益很小"),
                    );
                }
            }
        }

        // 光照预设：以前写错名字会静默退回 studio，现在直接报错
        if let Some(preset) = self.lighting.preset.as_deref() {
            if !matches!(preset, "studio" | "day" | "night" | "none") {
                out.push(
                    Diagnostic::error("lighting.preset", format!("未知的光照预设 \"{preset}\""))
                        .with_hint(
                            "可选：studio（缺省）/ day / night / none（无主光、环境光为 0）",
                        ),
                );
            }
        }

        // 相机
        if let Some(f) = self.camera.fov_deg {
            if !f.is_finite() || !(1.0..=179.0).contains(&f) {
                out.push(Diagnostic::error(
                    "camera.fov_deg",
                    "视场角应在 1~179 度之间",
                ));
            }
        }
        if let Some(n) = self.camera.near {
            if !n.is_finite() || n <= 0.0 {
                out.push(Diagnostic::error("camera.near", "近裁剪面必须为正数"));
            }
        }
        if let Some(f) = self.camera.far {
            if !f.is_finite() || f <= 0.0 {
                out.push(Diagnostic::error("camera.far", "远裁剪面必须为正数"));
            }
        }
        if let (Some(n), Some(f)) = (self.camera.near, self.camera.far) {
            if n.is_finite() && f.is_finite() && f <= n {
                out.push(Diagnostic::error(
                    "camera.far",
                    format!("远裁剪面必须大于近裁剪面（near={n}, far={f}）"),
                ));
            }
        }
        if let Some(h) = self.camera.orthographic_height {
            if !h.is_finite() || h <= 0.0 {
                out.push(Diagnostic::error(
                    "camera.orthographic_height",
                    "正交投影的可视高度必须为正数",
                ));
            }
        }
        if let Some(d) = self.camera.distance {
            if !d.is_finite() || d <= 0.0 {
                out.push(Diagnostic::error("camera.distance", "环绕距离必须为正数"));
            }
        }
        if self.camera.position.is_some() && self.camera.look_at.is_none() {
            out.push(Diagnostic::warn(
                "camera.look_at",
                "只给了位置没给注视点，相机会看向原点",
            ));
        }

        // 仿真 / 渲染
        if !self.simulate.dt.is_finite() || self.simulate.dt <= 0.0 || self.simulate.dt > 0.5 {
            out.push(Diagnostic::error(
                "simulate.dt",
                "步长必须在 (0, 0.5] 秒之间",
            ));
        }
        if self.render.width == 0 || self.render.height == 0 {
            out.push(Diagnostic::error(
                "render.width/height",
                "渲染尺寸必须大于 0",
            ));
        }
        if self.render.width > 8192 || self.render.height > 8192 {
            out.push(Diagnostic::warn(
                "render.width/height",
                "渲染尺寸过大，软件光栅下可能非常慢",
            ));
        }
        if !matches!(self.render.msaa, 1 | 2 | 4 | 8) {
            out.push(Diagnostic::error(
                "render.msaa",
                "MSAA 只能是 1 / 2 / 4 / 8",
            ));
        }
        if self.render.output.as_os_str().is_empty() {
            out.push(Diagnostic::error("render.output", "输出路径不能为空"));
        }

        out
    }

    /// 是否存在错误级诊断。
    pub fn has_errors(diags: &[Diagnostic]) -> bool {
        diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// 校验并在有错误时返回 `Err`。
    pub fn validated(self) -> Result<Self, DocError> {
        let diags = self.validate();
        if Self::has_errors(&diags) {
            Err(DocError::Invalid(diags))
        } else {
            Ok(self)
        }
    }
}

fn validate_shape(s: &ShapeDoc, path: &str, out: &mut Vec<Diagnostic>) {
    match s.kind {
        ShapeKind::Box => {
            if let Some(size) = s.size {
                if !finite3(&size) || size.iter().any(|v| *v <= 0.0) {
                    out.push(Diagnostic::error(
                        format!("{path}.size"),
                        "边长必须是正有限数",
                    ));
                }
            }
        }
        ShapeKind::Sphere => {
            if let Some(r) = s.radius {
                if !r.is_finite() || r <= 0.0 {
                    out.push(Diagnostic::error(
                        format!("{path}.radius"),
                        "半径必须为正数",
                    ));
                }
            }
        }
        ShapeKind::Capsule | ShapeKind::Cylinder => {
            if let Some(h) = s.half_height {
                if !h.is_finite() || h <= 0.0 {
                    out.push(Diagnostic::error(
                        format!("{path}.half_height"),
                        "半高必须为正数",
                    ));
                }
            }
            if let Some(r) = s.radius {
                if !r.is_finite() || r <= 0.0 {
                    out.push(Diagnostic::error(
                        format!("{path}.radius"),
                        "半径必须为正数",
                    ));
                }
            }
        }
        ShapeKind::Plane => {
            if let Some(n) = s.normal {
                if !finite3(&n) || Vec3::from_array(n).length_squared() < 1e-6 {
                    out.push(Diagnostic::error(
                        format!("{path}.normal"),
                        "法线必须是非零有限向量",
                    ));
                }
            }
        }
        ShapeKind::Convex => match &s.points {
            None => out.push(Diagnostic::error(
                format!("{path}.points"),
                "凸包必须提供 points",
            )),
            Some(pts) => {
                if pts.len() < 4 {
                    out.push(Diagnostic::error(
                        format!("{path}.points"),
                        format!("凸包至少需要 4 个顶点，当前 {}", pts.len()),
                    ));
                }
                if pts.iter().any(|p| !finite3(p)) {
                    out.push(Diagnostic::error(
                        format!("{path}.points"),
                        "顶点必须是有限数",
                    ));
                }
                if pts.len() >= 4 && pts.iter().all(finite3) {
                    out.push(
                        Diagnostic::warn(
                            path,
                            "凸包的物理是精确凸包，但渲染用点集的 AABB 盒体近似",
                        )
                        .with_hint("要看到真实外形请用 glTF/OBJ 模型（models），或改用 box"),
                    );
                }
            }
        },
    }
}

fn finite3(v: &[f32; 3]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// 把 `[x, y, z]` 转成 `Vec3`。
pub fn v3(a: [f32; 3]) -> Vec3 {
    Vec3::new(a[0], a[1], a[2])
}

/// 把 `Vec3` 转成 `[x, y, z]`（用于 JSON 输出）。
pub fn arr(v: Vec3) -> [f32; 3] {
    [v.x, v.y, v.z]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_object_is_a_valid_scene() {
        let doc = SceneDoc::from_json("{}").unwrap();
        assert_eq!(doc.format, FORMAT);
        assert_eq!(doc.version, VERSION);
        assert!(doc.objects.is_empty());
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "空场景应当合法: {diags:?}");
    }

    #[test]
    fn unknown_field_is_rejected_with_position() {
        let err = SceneDoc::from_json(r#"{"object": []}"#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("object"), "错误信息应指出字段名: {msg}");
    }

    #[test]
    fn scene_size_caps_produce_clear_diagnostics() {
        // 物体数超上限
        let objects: Vec<Value> = (0..=MAX_OBJECTS)
            .map(|i| {
                json!({
                    "name": format!("o{i}"),
                    "shape": { "type": "box", "size": [1, 1, 1] },
                })
            })
            .collect();
        let doc = SceneDoc::from_value(json!({ "objects": objects })).expect("应当能解析");
        let diags = doc.validate();
        let hit = diags
            .iter()
            .find(|d| d.path == "objects")
            .expect("应当报告 objects 超限");
        assert_eq!(hit.severity, Severity::Error);
        assert!(hit.message.contains("超过上限"), "{}", hit.message);
        assert!(hit.hint.is_some(), "应当给出怎么处理的提示");

        // 刚好处在上限内则不该报错
        let objects: Vec<Value> = (0..MAX_OBJECTS)
            .map(|i| {
                json!({
                    "name": format!("o{i}"),
                    "shape": { "type": "box", "size": [1, 1, 1] },
                })
            })
            .collect();
        let doc = SceneDoc::from_value(json!({ "objects": objects })).expect("应当能解析");
        assert!(
            doc.validate().iter().all(|d| d.path != "objects"),
            "等于上限不该报错"
        );

        // 步数超上限
        let doc = SceneDoc::from_value(json!({
            "simulate": { "steps": MAX_STEPS + 1 },
        }))
        .expect("应当能解析");
        assert!(
            doc.validate().iter().any(|d| d.path == "simulate.steps"),
            "应当报告步数超限"
        );

        // 非法 dt
        let doc = SceneDoc::from_value(json!({ "simulate": { "dt": 0.0 } })).expect("应当能解析");
        assert!(
            doc.validate().iter().any(|d| d.path == "simulate.dt"),
            "dt 必须为正数"
        );
    }

    #[test]
    fn structural_errors_are_all_reported_with_paths() {
        // 一次报出全部结构问题，每条都带 JSON 路径
        let err = SceneDoc::from_json(
            r#"{
                "ground": { "unknown_field": 1 },
                "materials": { "ok": { "color": [1, 0, 0] } },
                "objects": [
                    { "name": "a", "material": "nope", "body": "dynamic" },
                    { "name": "b", "shape": { "type": "pyramid" }, "body": "dynamic" },
                    { "name": "c", "body": "flying" }
                ]
            }"#,
        )
        .unwrap_err();
        let diags = match err {
            DocError::Invalid(d) => d,
            other => panic!("应当是结构诊断，实际 {other:?}"),
        };
        let paths: Vec<&str> = diags.iter().map(|d| d.path.as_str()).collect();
        assert!(
            paths.contains(&"ground") || paths.contains(&"ground.unknown_field"),
            "{paths:?}"
        );
        assert!(
            paths.contains(&"objects[1].shape.type"),
            "应当指出第二个物体的形状问题: {paths:?}"
        );
        assert!(
            paths.contains(&"objects[2].body"),
            "三个物体的错误应当一次全报: {paths:?}"
        );
        assert!(diags.len() >= 3, "应当收集到多条诊断: {diags:?}");
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(diags.iter().any(|d| d.message.contains("flying")));
    }

    #[test]
    fn multiple_errors_inside_one_object_are_all_reported() {
        let err = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","shape":{"type":"pyramid","size":"big"},
                            "body":"flying","position":[1,2],"material":3}]}"#,
        )
        .unwrap_err();
        let diags = match err {
            DocError::Invalid(d) => d,
            other => panic!("应当是结构诊断，实际 {other:?}"),
        };
        let paths: Vec<&str> = diags.iter().map(|d| d.path.as_str()).collect();
        for expected in [
            "objects[0].shape.type",
            "objects[0].shape.size",
            "objects[0].body",
            "objects[0].position",
            "objects[0].material",
        ] {
            assert!(paths.contains(&expected), "缺少 {expected}: {paths:?}");
        }
        assert_eq!(diags.len(), 5, "不该有重复诊断: {diags:?}");
    }

    #[test]
    fn unknown_top_level_field_is_named_but_nested_ones_are_not_confused() {
        // 顶层拼错字段名要被点出来；嵌套结构里的未知字段不能被误报成顶层字段
        let err = SceneDoc::from_json(
            r#"{"object": [], "ground": { "unknown_field": 1 }, "gravity": [0, -9.81, 0]}"#,
        )
        .unwrap_err();
        let diags = match err {
            DocError::Invalid(d) => d,
            other => panic!("应当是结构诊断，实际 {other:?}"),
        };
        let unknown_top: Vec<&str> = diags
            .iter()
            .filter(|d| d.message.contains("未知字段"))
            .map(|d| d.path.as_str())
            .collect();
        assert_eq!(
            unknown_top,
            vec!["object"],
            "只应点出 object 这一个顶层拼写错误"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.path == "ground" && d.message.contains("unknown_field")),
            "嵌套未知字段要带 ground 路径: {diags:?}"
        );
        // gravity 是对的，不该产生诊断
        assert!(!diags.iter().any(|d| d.path == "gravity"), "{diags:?}");
    }

    #[test]
    fn unknown_material_reference_is_an_error() {
        let doc =
            SceneDoc::from_json(r#"{"objects":[{"name":"a","material":"nope","body":"dynamic"}]}"#)
                .unwrap();
        let diags = doc.validate();
        assert!(SceneDoc::has_errors(&diags));
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0].material" && d.message.contains("nope")));
    }

    #[test]
    fn bloom_defaults_and_validation() {
        // 缺省 = 关闭：不写这一节时序列化结果里不该冒出新字段（对已有场景零影响）
        let doc = SceneDoc::from_json(r#"{"objects":[]}"#).unwrap();
        assert!(doc.lighting.bloom.is_none());
        let text = serde_json::to_string(&doc).unwrap();
        assert!(!text.contains("bloom"), "不写就不该出现在输出里：{text}");

        // 写了这一节：缺省打开、强度 0.6、阈值 1.0
        let doc = SceneDoc::from_json(r#"{"objects":[],"lighting":{"bloom":{}}}"#).unwrap();
        let b = doc.lighting.bloom.expect("bloom 节应当被解析出来");
        assert_eq!(b.enabled, None);
        assert_eq!(b.intensity, None);
        assert_eq!(b.threshold, None);
        assert!(doc.validate().iter().all(|d| d.severity != Severity::Error));

        // 负强度是错误；0 强度与过大阈值是警告——都指向具体字段
        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"bloom":{"intensity":-1.0,"threshold":50.0}}}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.bloom.intensity" && d.severity == Severity::Error));
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.bloom.threshold" && d.severity == Severity::Warning));

        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"bloom":{"enabled":true,"intensity":0.0}}}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.bloom.intensity" && d.severity == Severity::Warning));
        // 但"开着但强度 0"只是可疑，不算不合法（不然没法用它做 A/B）
        assert!(!SceneDoc::has_errors(&diags));

        // 字段名拼错要报未知字段，而不是静默忽略
        assert!(
            SceneDoc::from_json(r#"{"objects":[],"lighting":{"bloom":{"knee":1.0}}}"#).is_err()
        );
    }

    #[test]
    fn fxaa_defaults_and_validation() {
        // 缺省 = 关闭：不写这一节时序列化结果里不该冒出新字段（对已有场景零影响）
        let doc = SceneDoc::from_json(r#"{"objects":[]}"#).unwrap();
        assert!(doc.lighting.fxaa.is_none());
        let text = serde_json::to_string(&doc).unwrap();
        assert!(!text.contains("fxaa"), "不写就不该出现在输出里：{text}");

        // 写了这一节（哪怕是空对象）：解析出来，且阈值留给渲染器缺省
        let doc = SceneDoc::from_json(r#"{"objects":[],"lighting":{"fxaa":{}}}"#).unwrap();
        let f = doc.lighting.fxaa.expect("fxaa 节应当被解析出来");
        assert_eq!(f.enabled, None);
        assert_eq!(f.edge_threshold, None);
        assert_eq!(f.edge_threshold_min, None);
        assert!(!SceneDoc::has_errors(&doc.validate()));

        // 负阈值是错误；大于 1 的阈值是警告——都指向具体字段
        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"fxaa":{"edge_threshold":-0.1,"edge_threshold_min":2.0}}}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.fxaa.edge_threshold" && d.severity == Severity::Error));
        assert!(diags.iter().any(
            |d| d.path == "lighting.fxaa.edge_threshold_min" && d.severity == Severity::Warning
        ));

        // 显式关掉就不该再警告"阈值没意义"
        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"fxaa":{"enabled":false,"edge_threshold":5.0}}}"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&doc.validate()));
        assert!(
            !doc.validate()
                .iter()
                .any(|d| d.path.starts_with("lighting.fxaa")),
            "关掉的 fxaa 不该再有诊断"
        );

        // 字段名拼错要报未知字段，而不是静默忽略
        assert!(SceneDoc::from_json(r#"{"objects":[],"lighting":{"fxaa":{"knee":1.0}}}"#).is_err());
    }

    #[test]
    fn collision_filter_sensor_and_ccd_validation() {
        // 不写 = 和以前一样（序列化不该冒出新字段）
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"dynamic","velocity":[1,0,0]}]}"#,
        )
        .unwrap();
        let o = &doc.objects[0];
        assert!(o.collision_layer.is_none() && o.collides_with.is_none());
        assert!(o.sensor.is_none() && o.ccd.is_none());
        let json = serde_json::to_string(&doc).unwrap();
        for key in ["collision_layer", "collides_with", "sensor", "ccd"] {
            assert!(!json.contains(key), "缺省时不该序列化出 {key}");
        }

        // 层号越界是错误；重复层是警告
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"static","collision_layer":32,"collides_with":[0,0,40]}]}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0].collision_layer" && d.severity == Severity::Error));
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0].collides_with[2]" && d.severity == Severity::Error));
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0].collides_with" && d.severity == Severity::Warning));

        // 空列表 = 谁也不碰（警告）；不含自己那一层（警告）
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"static","collision_layer":3,"collides_with":[]}]}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags.iter().any(|d| d.path == "objects[0].collides_with"));
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"static","collision_layer":3,"collides_with":[1]}]}"#,
        )
        .unwrap();
        assert!(doc
            .validate()
            .iter()
            .any(|d| d.path == "objects[0].collides_with" && d.severity == Severity::Warning));

        // 传感器：没有刚体是错误；带密度/摩擦是警告
        let doc = SceneDoc::from_json(r#"{"objects":[{"name":"a","sensor":true}]}"#).unwrap();
        assert!(doc
            .validate()
            .iter()
            .any(|d| d.path == "objects[0].sensor" && d.severity == Severity::Error));
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"static","sensor":true,"density":500,"friction":0.1}]}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0]" && d.severity == Severity::Warning));
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");

        // CCD：只对 dynamic 有意义
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a","body":"static","ccd":true},{"name":"b","body":"dynamic","ccd":true}]}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[0].ccd" && d.severity == Severity::Warning));
        assert!(!diags.iter().any(|d| d.path == "objects[1].ccd"));

        // 拼错字段名要报未知字段
        assert!(
            SceneDoc::from_json(r#"{"objects":[{"name":"a","body":"static","layer":1}]}"#).is_err()
        );
    }

    #[test]
    fn ssao_defaults_and_validation() {
        // 缺省 = 关闭：不写这一节时序列化结果里不该冒出新字段（对已有场景零影响）
        let doc = SceneDoc::from_json(r#"{"objects":[]}"#).unwrap();
        assert!(doc.lighting.ssao.is_none());
        assert!(!serde_json::to_string(&doc).unwrap().contains("ssao"));

        // 写了这一节（哪怕空对象）就解析出来，参数留给渲染器缺省
        let doc = SceneDoc::from_json(r#"{"objects":[],"lighting":{"ssao":{}}}"#).unwrap();
        let a = doc.lighting.ssao.expect("ssao 节应当被解析出来");
        assert_eq!(a.enabled, None);
        assert_eq!(a.radius, None);
        assert_eq!(a.samples, None);
        assert!(!SceneDoc::has_errors(&doc.validate()));

        // 半径非正、采样数为 0 或过大：都是错误
        let doc =
            SceneDoc::from_json(r#"{"objects":[],"lighting":{"ssao":{"radius":0.0,"samples":0}}}"#)
                .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.ssao.radius" && d.severity == Severity::Error));
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.ssao.samples" && d.severity == Severity::Error));

        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"ssao":{"samples":200,"radius":50.0,"bias":-1.0,"strength":3.0}}}"#,
        )
        .unwrap();
        let diags = doc.validate();
        for (path, sev) in [
            ("lighting.ssao.samples", Severity::Error),
            ("lighting.ssao.radius", Severity::Warning),
            ("lighting.ssao.bias", Severity::Error),
            ("lighting.ssao.strength", Severity::Warning),
        ] {
            assert!(
                diags.iter().any(|d| d.path == path && d.severity == sev),
                "缺少 {path} 的 {sev:?} 诊断: {diags:?}"
            );
        }

        // 偏移不小于半径 = 遮挡全被滤掉（警告）；强度 0 = 等于没开（警告）
        let doc = SceneDoc::from_json(
            r#"{"objects":[],"lighting":{"ssao":{"radius":0.5,"bias":0.6,"strength":0.0}}}"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.ssao.bias" && d.severity == Severity::Warning));
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.ssao.strength" && d.severity == Severity::Warning));

        // 字段名拼错要报未知字段，而不是静默忽略
        assert!(
            SceneDoc::from_json(r#"{"objects":[],"lighting":{"ssao":{"range":1.0}}}"#).is_err()
        );
    }

    #[test]
    fn joint_requires_named_dynamic_bodies() {
        let doc = SceneDoc::from_json(
            r#"{"objects":[{"name":"a"}],"joints":[{"type":"hinge","a":"a","b":"missing"}]}"#,
        )
        .unwrap();
        let diags = doc.validate();
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        // a 没有刚体 + b 不存在
        assert_eq!(errors.len(), 2, "{diags:?}");
        assert!(errors.iter().any(|d| d.path == "joints[0].a"));
        assert!(errors.iter().any(|d| d.path == "joints[0].b"));
    }

    #[test]
    fn dynamic_plane_is_rejected() {
        let doc =
            SceneDoc::from_json(r#"{"objects":[{"shape":{"type":"plane"},"body":"dynamic"}]}"#)
                .unwrap();
        assert!(SceneDoc::has_errors(&doc.validate()));
    }

    #[test]
    fn roundtrip_keeps_meaningful_fields() {
        let text = r#"{
            "name": "t",
            "ground": {"size": 20},
            "materials": {"m": {"color": [0.1, 0.2, 0.3], "roughness": 0.7}},
            "objects": [{"name":"b","shape":{"type":"sphere","radius":0.4},
                         "position":[1,2,3],"body":"dynamic","restitution":0.5}],
            "simulate": {"steps": 60},
            "render": {"output":"x.png","width":320,"height":240}
        }"#;
        let doc = SceneDoc::from_json(text).unwrap();
        assert!(doc.validate().iter().all(|d| d.severity != Severity::Error));
        let again = SceneDoc::from_json(&doc.to_json_pretty()).unwrap();
        assert_eq!(again.objects.len(), 1);
        assert_eq!(again.objects[0].primary_shape().kind, ShapeKind::Sphere);
        assert_eq!(again.simulate.steps, 60);
        assert_eq!(again.render.output, PathBuf::from("x.png"));
    }

    #[test]
    fn silent_fallbacks_are_now_reported() {
        // 重名物体：名字是关节两端、--trace 与报告索引的键，重名会静默覆盖一个
        let doc = SceneDoc::from_value(json!({
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1,1,1] }, "body": "dynamic" },
                { "name": "箱", "shape": { "type": "box", "size": [1,1,1] }, "body": "dynamic" }
            ]
        }))
        .unwrap();
        let diags = doc.validate();
        assert!(SceneDoc::has_errors(&diags), "重名必须是错误: {diags:?}");
        assert!(diags
            .iter()
            .any(|d| d.path == "objects[1].name" && d.message.contains("重复")));

        // 未知光照预设：以前静默退回 studio
        let doc = SceneDoc::from_value(json!({ "lighting": { "preset": "stduio" } })).unwrap();
        let diags = doc.validate();
        assert!(diags
            .iter()
            .any(|d| d.path == "lighting.preset" && d.severity == Severity::Error));
        for preset in ["studio", "day", "night", "none"] {
            let doc = SceneDoc::from_value(json!({ "lighting": { "preset": preset } })).unwrap();
            let diags = doc.validate();
            assert!(
                !SceneDoc::has_errors(&diags),
                "预设 {preset} 应当合法: {diags:?}"
            );
        }
    }

    #[test]
    fn unit_and_render_only_traps_are_warned_about() {
        // 铰链限位写成角度（90 而不是 1.5708 弧度）
        let doc = SceneDoc::from_value(json!({
            "objects": [
                { "name": "a", "shape": { "type": "box", "size": [1,1,1] }, "body": "static" },
                { "name": "b", "shape": { "type": "box", "size": [1,1,1] }, "body": "dynamic" }
            ],
            "joints": [ { "type": "hinge", "a": "a", "b": "b", "limits": [-90, 90] } ]
        }))
        .unwrap();
        let diags = doc.validate();
        let hit = diags
            .iter()
            .find(|d| d.path == "joints[0].limits")
            .unwrap_or_else(|| panic!("应当有单位警告: {diags:?}"));
        assert_eq!(hit.severity, Severity::Warning);
        assert!(hit.message.contains("弧度"), "{}", hit.message);

        // 关节轴是**局部轴**而不是世界轴：文档必须说实话（见 JointDoc::axis 注释）
        let doc = SceneDoc::from_value(json!({
            "objects": [
                { "name": "a", "shape": { "type": "box", "size": [1,1,1] }, "body": "static" },
                { "name": "b", "shape": { "type": "box", "size": [1,1,1] }, "body": "dynamic" }
            ],
            "joints": [ { "type": "hinge", "a": "a", "b": "b", "axis": [0, 0, 0] } ]
        }))
        .unwrap();
        let zero_axis = doc
            .validate()
            .into_iter()
            .find(|d| d.path == "joints[0].axis")
            .unwrap_or_else(|| panic!("零轴向必须报错，否则会被 normalize_or_zero 静默吃掉"));
        assert_eq!(zero_axis.severity, Severity::Error);

        // scale 只影响渲染（碰撞形状仍是 shape 的原始尺寸）
        let doc = SceneDoc::from_value(json!({
            "objects": [ { "name": "s", "shape": { "type": "box", "size": [1,1,1] },
                           "body": "dynamic", "scale": [2,2,2] } ]
        }))
        .unwrap();
        assert!(doc.validate().iter().any(|d| d.path == "objects[0].scale"
            && d.severity == Severity::Warning
            && d.message.contains("只影响渲染")));

        // 凸包：物理精确、渲染用 AABB 近似（警告而非错误）
        let doc = SceneDoc::from_value(json!({
            "objects": [ { "name": "h", "body": "dynamic",
                "shape": { "type": "convex", "points": [[0,0,0],[1,0,0],[0,1,0],[0,0,1]] } } ]
        }))
        .unwrap();
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "凸包本身合法: {diags:?}");
        assert!(
            diags.iter().any(|d| d.message.contains("AABB")),
            "应当提示渲染近似: {diags:?}"
        );
    }

    #[test]
    fn ground_friction_is_configurable_and_validated() {
        // 缺省保持 0.85，保证既有场景行为不变
        let doc = SceneDoc::from_value(json!({ "ground": { "size": 10 } })).unwrap();
        assert_eq!(doc.ground.as_ref().unwrap().friction, 0.85);
        assert_eq!(doc.ground.as_ref().unwrap().restitution, 0.0);

        let doc = SceneDoc::from_value(json!({
            "ground": { "friction": 0.02, "restitution": 0.6 }
        }))
        .unwrap();
        assert_eq!(doc.ground.as_ref().unwrap().friction, 0.02);
        assert!(!SceneDoc::has_errors(&doc.validate()));

        let doc = SceneDoc::from_value(json!({ "ground": { "friction": -1.0 } })).unwrap();
        assert!(doc.validate().iter().any(|d| d.path == "ground.friction"));
        let doc = SceneDoc::from_value(json!({ "ground": { "restitution": 2.0 } })).unwrap();
        assert!(doc
            .validate()
            .iter()
            .any(|d| d.path == "ground.restitution"));
    }

    /// 定时动作：目标、时序、参数怎么错都要报出来。
    #[test]
    fn actions_are_validated() {
        let base = |actions: Value| {
            json!({
                "objects": [
                    { "name": "球", "shape": { "type": "sphere" }, "body": "dynamic" },
                    { "name": "台", "shape": { "type": "box" }, "body": "static" },
                    { "name": "贴图", "shape": { "type": "box" } }
                ],
                "simulate": { "steps": 120, "dt": 0.01666667 },
                "actions": actions
            })
        };
        // 全对：五类动作一起给，也必须一条诊断都没有
        let ok = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 30, "value": [0, 4, 0] },
            { "type": "impulse_at_point", "target": "球", "at_step": 40, "value": [2, 0, 0], "point": [1, 0, 0] },
            { "type": "torque_impulse", "target": "球", "at_time": 0.5, "value": [0, 1, 0] },
            { "type": "velocity", "target": "球", "value": [1, 0, 0] },
            { "type": "angular_velocity", "target": "球", "at_step": 10, "value": [0, 2, 0] },
            { "type": "force", "target": "球", "at_step": 60, "steps": 30, "value": [0, 9, 0] },
            { "type": "teleport", "target": "球", "at_step": 90, "position": [0, 5, 0], "rotation_deg": [0, 45, 0] },
            { "type": "wake", "target": "球", "at_step": 100 }
        ])))
        .unwrap();
        assert!(!SceneDoc::has_errors(&ok.validate()), "{:?}", ok.validate());
        // 一条警告都不该有：这些写法都是明确的
        assert!(ok.validate().is_empty(), "{:?}", ok.validate());

        // 目标不存在 / 没有刚体 / 静态刚体推不动
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "不存在", "at_step": 1, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].target" && d.message.contains("找不到")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "贴图", "at_step": 1, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("没有刚体")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "台", "at_step": 1, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("静态刚体")),
            "{diags:?}"
        );

        // 时序：两个都给是错误；超出仿真步数是警告（否则动作静默不触发）
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 10, "at_time": 0.5, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].at_time" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 500, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("不会触发")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_time": -1.0, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].at_time" && d.severity == Severity::Error),
            "{diags:?}"
        );

        // 参数：该给的没给 = 错误；给了用不上的 = 警告（防静默忽略）
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 1 }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].value" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse_at_point", "target": "球", "at_step": 1, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].point" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "teleport", "target": "球", "at_step": 1 }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "actions[0].position" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "teleport", "target": "球", "at_step": 1, "position": [0, 1, 0], "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("不需要 value")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "force", "target": "球", "at_step": 1, "steps": 0, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags.iter().any(|d| d.message.contains("一步都不会施加")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 1, "steps": 5, "value": [0, 1, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("只对 force 生效")),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "type": "impulse", "target": "球", "at_step": 1, "value": [0, 1, 0], "point": [1, 0, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("只有 impulse_at_point 用 point")),
            "{diags:?}"
        );

        // simulate.steps = 0：任何动作都不可能触发
        let diags = SceneDoc::from_value(json!({
            "objects": [{ "name": "球", "body": "dynamic" }],
            "actions": [{ "type": "wake", "target": "球", "at_step": 1 }]
        }))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("不会触发")),
            "{diags:?}"
        );

        // 未知动作类型走结构化报错（不是静默当成 impulse）
        let err = SceneDoc::from_value(base(json!([
            { "type": "explode", "target": "球", "at_step": 1, "value": [0, 1, 0] }
        ])))
        .unwrap_err();
        assert!(format!("{err:?}").contains("actions[0].type"), "{err:?}");
    }

    /// 区域探针：重名、非法尺寸、未知观测目标都要报出来。
    #[test]
    fn probes_are_validated() {
        let base = |probes: Value| {
            json!({
                "objects": [
                    { "name": "球", "shape": { "type": "sphere" }, "body": "dynamic" },
                    { "name": "贴图", "shape": { "type": "box" } }
                ],
                "simulate": { "steps": 60 },
                "probes": probes
            })
        };
        let ok = SceneDoc::from_value(base(json!([
            { "name": "落点", "shape": "sphere", "position": [0, 0.5, 0], "radius": 1.5 },
            { "name": "窗口", "shape": "box", "position": [0, 2, 0], "size": [1, 1, 1] },
            { "name": "只盯球", "shape": "sphere", "position": [0, 0, 0], "radius": 2.0, "watch": ["球"] }
        ])))
        .unwrap();
        assert!(!SceneDoc::has_errors(&ok.validate()), "{:?}", ok.validate());
        assert!(ok.validate().is_empty(), "{:?}", ok.validate());

        // 重名
        let diags = SceneDoc::from_value(base(json!([
            { "name": "同", "position": [0, 0, 0] },
            { "name": "同", "position": [1, 0, 0] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[1].name" && d.message.contains("重复")),
            "{diags:?}"
        );
        // 空名
        let diags = SceneDoc::from_value(base(json!([{ "name": "  ", "position": [0, 0, 0] }])))
            .unwrap()
            .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[0].name" && d.severity == Severity::Error),
            "{diags:?}"
        );
        // 半径与盒边长
        let diags = SceneDoc::from_value(base(json!([
            { "name": "坏球", "position": [0, 0, 0], "radius": 0.0 }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[0].radius" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "name": "坏盒", "shape": "box", "position": [0, 0, 0], "size": [1, 0, 1] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[0].size" && d.severity == Severity::Error),
            "{diags:?}"
        );
        // 球形探针给了 size：警告而不是静默忽略
        let diags = SceneDoc::from_value(base(json!([
            { "name": "球", "position": [0, 0, 0], "size": [1, 1, 1] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[0].size" && d.severity == Severity::Warning),
            "{diags:?}"
        );
        // 观测目标不存在 / 没有刚体
        let diags = SceneDoc::from_value(base(json!([
            { "name": "p", "position": [0, 0, 0], "watch": ["查无此物"] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "probes[0].watch[0]" && d.severity == Severity::Error),
            "{diags:?}"
        );
        let diags = SceneDoc::from_value(base(json!([
            { "name": "p", "position": [0, 0, 0], "watch": ["贴图"] }
        ])))
        .unwrap()
        .validate();
        assert!(
            diags.iter().any(|d| d.message.contains("没有刚体")),
            "{diags:?}"
        );
        // simulate.steps = 0：探针永远观测不到东西 → 警告
        let diags = SceneDoc::from_value(json!({
            "objects": [{ "name": "球", "body": "dynamic" }],
            "probes": [{ "name": "p", "position": [0, 0, 0] }]
        }))
        .unwrap()
        .validate();
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.message.contains("不会观测到")),
            "{diags:?}"
        );

        // 缺省：不写 actions / probes 时两个字段都是空的，且不影响 JSON 往返
        let plain =
            SceneDoc::from_value(json!({ "objects": [{ "name": "球", "body": "dynamic" }] }))
                .unwrap();
        assert!(plain.actions.is_empty() && plain.probes.is_empty());
        let text = serde_json::to_string(&plain).unwrap();
        assert!(
            !text.contains("actions") && !text.contains("probes"),
            "{text}"
        );
    }

    /// 本轮新接线字段的语义校验：错的值必须报出来，对的写法必须放行。
    #[test]
    fn newly_wired_fields_are_validated() {
        // 材质：alpha 范围、uv_scale 正数、alpha_cutoff 与 alpha_blend 同时给是无效组合
        let doc = SceneDoc::from_value(json!({
            "materials": { "m": { "color": [1, 1, 1], "alpha": 2.0, "uv_scale": 0.0,
                                  "alpha_cutoff": 0.5, "alpha_blend": true } }
        }))
        .unwrap();
        let diags = doc.validate();
        let paths: Vec<&str> = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.path.as_str())
            .collect();
        assert!(
            paths.contains(&"materials.m.alpha"),
            "alpha 超范围要报错: {diags:?}"
        );
        assert!(
            paths.contains(&"materials.m.uv_scale"),
            "uv_scale 必须为正: {diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.path == "materials.m.alpha_cutoff"),
            "裁剪与混合同时给应当给出警告: {diags:?}"
        );

        // 合法值不应产生 error
        let ok = SceneDoc::from_value(json!({
            "materials": { "m": { "color": [1, 1, 1], "alpha": 0.5, "uv_scale": 2.0,
                                  "emissive_color": [1.0, 0.5, 0.0], "normal_scale": 0.5,
                                  "alpha_blend": true, "double_sided": true, "unlit": false } }
        }))
        .unwrap();
        assert!(!SceneDoc::has_errors(&ok.validate()), "{:?}", ok.validate());

        // 相机：近/远裁剪面与正交高度
        let cam = SceneDoc::from_value(json!({
            "camera": { "near": 0.0, "far": 1.0, "orthographic_height": -2.0 }
        }))
        .unwrap();
        let cam_paths: Vec<String> = cam.validate().iter().map(|d| d.path.clone()).collect();
        assert!(
            cam_paths.contains(&"camera.near".to_string()),
            "{cam_paths:?}"
        );
        assert!(
            cam_paths.contains(&"camera.orthographic_height".to_string()),
            "{cam_paths:?}"
        );

        let cam_ok = SceneDoc::from_value(json!({
            "camera": { "near": 0.1, "far": 100.0, "orthographic_height": 4.0 }
        }))
        .unwrap();
        assert!(!SceneDoc::has_errors(&cam_ok.validate()));

        // 刚体：阻尼非负、质量为正、角速度有限
        let body = SceneDoc::from_value(json!({
            "objects": [ { "name": "b", "body": "dynamic", "linear_damping": -1.0,
                           "angular_damping": -0.5, "mass": 0.0,
                           "angular_velocity": [1.0, 0.0, 0.0] } ]
        }))
        .unwrap();
        let body_paths: Vec<String> = body
            .validate()
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.path.clone())
            .collect();
        assert!(
            body_paths.contains(&"objects[0].linear_damping".to_string()),
            "{body_paths:?}"
        );
        assert!(
            body_paths.contains(&"objects[0].angular_damping".to_string()),
            "{body_paths:?}"
        );
        assert!(
            body_paths.contains(&"objects[0].mass".to_string()),
            "{body_paths:?}"
        );

        // 只有动态刚体消费这些字段：非 dynamic 时静默忽略是最难查的坑，必须警告
        let ignored = SceneDoc::from_value(json!({
            "objects": [ { "name": "s", "body": "static", "mass": 5.0,
                           "gravity_scale": 0.0, "allow_sleep": false } ]
        }))
        .unwrap();
        let warn = ignored
            .validate()
            .into_iter()
            .find(|d| d.path == "objects[0]" && d.severity == Severity::Warning)
            .unwrap_or_else(|| panic!("应当警告这些字段被忽略"));
        assert!(warn.message.contains("mass"), "{}", warn.message);
        assert!(warn.message.contains("gravity_scale"), "{}", warn.message);

        // 关节锚点必须是有限数
        let joint = SceneDoc::from_value(json!({
            "objects": [ { "name": "a", "body": "dynamic" }, { "name": "b", "body": "dynamic" } ],
            "joints": [ { "type": "ball", "a": "a", "b": "b",
                          "anchor_a": [0.0, null, 0.0] } ]
        }));
        // null 会被结构解析拦下（这里只关心非有限数走语义校验）
        assert!(joint.is_err() || !SceneDoc::has_errors(&joint.unwrap().validate()));

        // 点光源：超过着色器上限要给警告，而不是让用户以为都亮了
        let lights: Vec<serde_json::Value> = (0..MAX_POINT_LIGHTS + 2)
            .map(|i| json!({ "position": [i as f32, 0.0, 0.0] }))
            .collect();
        let many = SceneDoc::from_value(json!({ "lighting": { "point_lights": lights } })).unwrap();
        assert!(
            many.validate()
                .iter()
                .any(|d| d.severity == Severity::Warning && d.path == "lighting.point_lights"),
            "超过 {MAX_POINT_LIGHTS} 盏应当警告"
        );
        // 不写 point_lights 与写了空数组是两种意思：前者保留预设补光
        let absent = SceneDoc::from_value(json!({ "lighting": { "preset": "studio" } })).unwrap();
        assert!(absent.lighting.point_lights.is_none());
        let cleared = SceneDoc::from_value(json!({ "lighting": { "point_lights": [] } })).unwrap();
        assert_eq!(
            cleared.lighting.point_lights.as_ref().map(|v| v.len()),
            Some(0)
        );
    }

    #[test]
    fn assets_search_paths_and_model_paths_use_the_same_candidates() {
        let dir = std::env::temp_dir().join(format!("quench3d_assets_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        std::fs::write(dir.join("lib").join("robot.obj"), "o cube\n").unwrap();
        std::fs::write(dir.join("here.glb"), b"not-really").unwrap();
        let scene = dir.join("level.json");
        let write = |v: serde_json::Value| {
            std::fs::write(&scene, serde_json::to_string(&v).unwrap()).unwrap();
        };

        // 1) 场景目录里的模型：解析到**场景目录**，而不是"进程当前工作目录"。
        //    这正是修掉的那个不一致：贴图走场景目录，模型走加载器的另一套规则。
        write(json!({ "models": [ { "path": "here.glb" } ] }));
        let doc = SceneDoc::from_file(&scene).unwrap();
        assert_eq!(
            doc.resolve_model(std::path::Path::new("here.glb")).unwrap(),
            dir.join("here.glb")
        );

        // 2) assets.search_paths：模型在 `lib/` 里也找得到；顺序是
        //    场景目录 → 搜索路径 → 当前工作目录 → 引擎搜索路径。
        write(json!({
            "assets": { "search_paths": ["lib"] },
            "models": [ { "path": "robot.obj" } ]
        }));
        let doc = SceneDoc::from_file(&scene).unwrap();
        let cands = doc.model_candidates(std::path::Path::new("robot.obj"));
        assert_eq!(cands[0], dir.join("robot.obj"), "场景目录优先：{cands:?}");
        assert_eq!(
            cands[1],
            dir.join("lib").join("robot.obj"),
            "其次是搜索路径：{cands:?}"
        );
        assert_eq!(
            doc.resolve_model(std::path::Path::new("robot.obj"))
                .unwrap(),
            dir.join("lib").join("robot.obj")
        );
        assert!(
            doc.validate().iter().all(|d| d.severity != Severity::Error),
            "有搜索路径能找到时不该报错：{:?}",
            doc.validate()
        );

        // 3) 找不到：错误里要列出"找过"的位置（含搜索路径展开后的绝对路径）
        write(json!({
            "assets": { "search_paths": ["lib"] },
            "models": [ { "path": "nope_3d7f.glb" } ]
        }));
        let doc = SceneDoc::from_file(&scene).unwrap();
        let diags = doc.validate();
        let hit = diags
            .iter()
            .find(|d| d.path == "models[0].path" && d.severity == Severity::Error)
            .unwrap_or_else(|| panic!("应当报找不到文件：{diags:?}"));
        assert!(
            hit.message.contains("找过：") && hit.message.contains("lib"),
            "错误要列出找过的位置：{}",
            hit.message
        );
        assert!(
            hit.hint
                .as_deref()
                .unwrap_or("")
                .contains("assets.search_paths"),
            "提示里要说清怎么办：{:?}",
            hit.hint
        );

        // 4) 搜索路径自身：空项 = 错误；不存在的目录 = 警告（打错一个字会静默失效）
        write(json!({ "assets": { "search_paths": ["", "no_such_dir_3d7f"] } }));
        let doc = SceneDoc::from_file(&scene).unwrap();
        let diags = doc.validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "assets.search_paths[0]" && d.severity == Severity::Error),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.path == "assets.search_paths[1]" && d.severity == Severity::Warning),
            "{diags:?}"
        );

        // 5) 不写 assets = 老行为（候选里没有搜索路径，序列化时也不出现这个键）
        write(json!({ "models": [ { "path": "here.glb" } ] }));
        let doc = SceneDoc::from_file(&scene).unwrap();
        assert!(doc.assets.is_none());
        let round = SceneDoc::from_json(&doc.to_json_pretty()).unwrap();
        assert!(round.assets.is_none());
        assert!(!doc.to_json_pretty().contains("search_paths"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 层级（格式版本 2 的 objects[].parent）------------------------------

    /// 取"第一条提到某个字段"的诊断，方便断言"确实报了这个错"。
    fn diag_at<'a>(diags: &'a [Diagnostic], path: &str) -> Option<&'a Diagnostic> {
        diags.iter().find(|d| d.path == path)
    }

    #[test]
    fn parent_edge_survives_a_json_round_trip() {
        let doc = SceneDoc::from_json(
            r#"{
              "objects": [
                { "name": "底座", "shape": { "type": "box", "size": [2, 0.2, 2] } },
                { "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                  "parent": "底座", "position": [0, 1.5, 0] }
              ]
            }"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&doc.validate()));
        let text = doc.to_json_pretty();
        let back = SceneDoc::from_json(&text).unwrap();
        assert_eq!(back.objects[1].parent.as_deref(), Some("底座"));
        // 没有父物体的那条不该被写出 `parent` 字段（`skip_serializing_if`）。
        assert!(back.objects[0].parent.is_none());
        assert!(!text.contains("\"parent\": null"), "{text}");
    }

    #[test]
    fn dangling_parent_is_an_error() {
        let doc =
            SceneDoc::from_json(r#"{ "objects": [ { "name": "球", "parent": "不存在的东西" } ] }"#)
                .unwrap();
        let diags = doc.validate();
        let d = diag_at(&diags, "objects[0].parent").expect("必须报悬空父物体");
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("不存在的东西"), "{}", d.message);
    }

    #[test]
    fn unnamed_parent_cannot_be_referenced() {
        // 父物体必须**显式命名**：缺省名 objects[i] 只是给人看的编号，不是可引用的名字。
        let doc = SceneDoc::from_json(
            r#"{ "objects": [ { "shape": { "type": "box" } },
                              { "name": "球", "parent": "objects[0]" } ] }"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(
            diag_at(&diags, "objects[1].parent").is_some_and(|d| d.severity == Severity::Error),
            "{diags:?}"
        );
    }

    #[test]
    fn self_parent_is_an_error() {
        let doc =
            SceneDoc::from_json(r#"{ "objects": [ { "name": "球", "parent": "球" } ] }"#).unwrap();
        let diags = doc.validate();
        let d = diag_at(&diags, "objects[0].parent").expect("必须报自环");
        assert!(d.message.contains("自己"), "{}", d.message);
    }

    #[test]
    fn parent_cycle_is_reported_once() {
        let doc = SceneDoc::from_json(
            r#"{ "objects": [
                  { "name": "a", "parent": "c" },
                  { "name": "b", "parent": "a" },
                  { "name": "c", "parent": "b" } ] }"#,
        )
        .unwrap();
        let diags = doc.validate();
        let cycles: Vec<&Diagnostic> = diags
            .iter()
            .filter(|d| d.path.ends_with(".parent") && d.message.contains("成环"))
            .collect();
        // 环上每个物体沿父链都会走回自己 —— 但只报一条，否则一个环会刷出一屏。
        assert_eq!(cycles.len(), 1, "{diags:?}");
        assert_eq!(cycles[0].path, "objects[0].parent");
    }

    #[test]
    fn a_body_cannot_hang_under_a_parent() {
        let doc = SceneDoc::from_json(
            r#"{ "objects": [
                  { "name": "支架", "shape": { "type": "box" } },
                  { "name": "箱子", "parent": "支架", "body": "dynamic" } ] }"#,
        )
        .unwrap();
        let diags = doc.validate();
        let d = diag_at(&diags, "objects[1].parent").expect("带刚体不能有父物体");
        assert!(d.message.contains("刚体"), "{}", d.message);
        // 反过来（刚体当父物体）是允许的：子物体跟着刚体走。
        let ok = SceneDoc::from_json(
            r#"{ "objects": [
                  { "name": "平台", "body": "static" },
                  { "name": "装饰", "parent": "平台" } ] }"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&ok.validate()));
    }

    #[test]
    fn templates_and_instances_expand_validate_and_round_trip() {
        let json = r#"{
          "templates": {
            "箱子": { "shape": { "type": "box", "size": [0.5, 0.5, 0.5] },
                      "body": "dynamic", "material": "木头", "density": 500 },
            "没用": { "shape": { "type": "sphere" }, "position": [9, 9, 9] }
          },
          "materials": { "木头": { "color": [0.8, 0.6, 0.35] } },
          "objects": [
            { "name": "堆", "template": "箱子",
              "instances": [ { "position": [0, 0.25, 0] },
                             { "position": [0, 0.75, 0], "rotation_deg": [0, 15, 0] },
                             { "name": "顶箱", "position": [0, 1.5, 0] } ] },
            { "name": "重箱", "template": "箱子", "density": 2000, "position": [1, 0.25, 0] } ] }"#;
        let doc = SceneDoc::from_json(json).unwrap();
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("没有被任何物体引用")),
            "用不到的模板该有警告：{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("模板里的 parent / position")),
            "模板里写位置该有警告：{msgs:?}"
        );

        // 展开：三个副本 + 一个单独物体
        let ex = doc.expanded();
        let names: Vec<String> = ex.objects.iter().map(|o| o.name.clone().unwrap()).collect();
        assert_eq!(names, vec!["堆#1", "堆#2", "顶箱", "重箱"]);
        assert!(ex.templates.is_empty(), "展开后不该还留着模板");
        assert!(ex.objects.iter().all(|o| o.instances.is_none()));
        // 模板字段被继承：形状、刚体类型、材质、密度
        let first = &ex.objects[0];
        assert_eq!(first.primary_shape().kind, ShapeKind::Box);
        assert_eq!(first.primary_shape().size, Some([0.5, 0.5, 0.5]));
        assert_eq!(first.body, BodyKind::Dynamic);
        assert_eq!(first.material.as_deref(), Some("木头"));
        assert_eq!(first.density, Some(500.0));
        assert_eq!(first.position, Some([0.0, 0.25, 0.0]));
        // 实例自己的朝向
        assert_eq!(ex.objects[1].rotation_deg, Some([0.0, 15.0, 0.0]));
        // 物体显式字段赢模板：密度 2000
        assert_eq!(ex.objects[3].density, Some(2000.0));
        assert_eq!(ex.objects[3].position, Some([1.0, 0.25, 0.0]));
        // 展开后的文档自身必须干净（名字/字段都具体了），且不该再有模板层诊断
        let ex_diags = ex.validate();
        assert!(!SceneDoc::has_errors(&ex_diags), "{ex_diags:?}");

        // 单个实例、没写 name：保持物体本名（关节/动作引用老名字仍然有效）
        let one = SceneDoc::from_json(
            r#"{ "templates": { "t": { "body": "static" } },
                 "objects": [ { "name": "墙", "template": "t",
                                "instances": [ { "position": [0, 0, 0] } ] } ] }"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&one.validate()));
        let ex_one = one.expanded();
        assert_eq!(ex_one.objects.len(), 1);
        assert_eq!(ex_one.objects[0].name.as_deref(), Some("墙"));
        assert_eq!(ex_one.objects[0].body, BodyKind::Static);

        // 往返：`templates` 与 `instances` 原样保留（模板不是"装载时展开一次就丢"的东西）
        let round = doc.to_json_pretty();
        assert!(round.contains("\"templates\""), "{round}");
        assert!(round.contains("\"instances\""), "{round}");
        let back = SceneDoc::from_json(&round).unwrap();
        assert_eq!(back.expanded().objects.len(), 4);

        // 错误①：引用了不存在的模板
        let unknown =
            SceneDoc::from_json(r#"{ "objects": [ { "name": "甲", "template": "没有这个" } ] }"#)
                .unwrap();
        let d = unknown.validate();
        assert!(SceneDoc::has_errors(&d), "{d:?}");
        assert!(d.iter().any(|x| x.message.contains("找不到模板")), "{d:?}");
        // 错误②：空实例表
        let empty =
            SceneDoc::from_json(r#"{ "objects": [ { "name": "乙", "instances": [] } ] }"#).unwrap();
        assert!(SceneDoc::has_errors(&empty.validate()));
        // 错误③：展开后名字撞车
        let dup = SceneDoc::from_json(
            r#"{ "objects": [
                 { "name": "箱", "instances": [ {}, {} ] },
                 { "name": "箱#2" } ] }"#,
        )
        .unwrap();
        let d = dup.validate();
        assert!(
            d.iter().any(|x| x.message.contains("重复")),
            "名字撞车必须报错：{d:?}"
        );
        // 实例里的未知字段要报错
        assert!(SceneDoc::from_json(
            r#"{ "objects": [ { "name": "丙", "instances": [ { "scale2": [1, 1, 1] } ] } ] }"#
        )
        .is_err());
    }

    #[test]
    fn compound_shapes_parse_validate_and_round_trip() {
        // 合法：三条腿的桌子（顶板 + 两条腿），部件的 position/rotation_deg 是局部量
        let doc = SceneDoc::from_json(
            r#"{ "objects": [
                   { "name": "桌子", "body": "dynamic",
                     "shapes": [
                       { "shape": { "type": "box", "size": [1.2, 0.1, 0.8] },
                         "position": [0, 0.75, 0] },
                       { "shape": { "type": "cylinder", "radius": 0.05, "half_height": 0.35 },
                         "position": [0.5, 0.35, 0.3] },
                       { "shape": { "type": "cylinder", "radius": 0.05, "half_height": 0.35 },
                         "position": [-0.5, 0.35, -0.3], "rotation_deg": [0, 0, 15] } ] } ] }"#,
        )
        .unwrap();
        assert!(
            !SceneDoc::has_errors(&doc.validate()),
            "{:?}",
            doc.validate()
        );
        let parts = doc.objects[0].parts();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].shape.kind, ShapeKind::Box);
        assert_eq!(parts[0].position, Some([0.0, 0.75, 0.0]));
        assert_eq!(parts[1].shape.kind, ShapeKind::Cylinder);
        assert_eq!(parts[2].rotation_deg, Some([0.0, 0.0, 15.0]));
        // 主形状就是第一个部件
        assert_eq!(doc.objects[0].primary_shape().kind, ShapeKind::Box);
        // 往返：`shapes` 必须原样写回去（不是被"归一成单形状"吃掉）
        let json = doc.to_json_pretty();
        let back = SceneDoc::from_json(&json).unwrap();
        assert_eq!(back.objects[0].parts().len(), 3);
        assert!(json.contains("\"shapes\""));
        assert!(back.objects[0].shape.is_none(), "复合形状不该同时写 shape");

        // 单形状：老写法仍然只有 `shape`，`parts()` 归一成一项
        let single = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "球", "shape": { "type": "sphere", "radius": 0.3 } } ] }"#,
        )
        .unwrap();
        assert_eq!(single.objects[0].parts().len(), 1);
        assert!(single.objects[0].shapes.is_none());
        assert_eq!(single.objects[0].primary_shape().kind, ShapeKind::Sphere);
        // 两个都不写 = 缺省 1×1×1 立方体（老行为）
        let bare = SceneDoc::from_json(r#"{ "objects": [ { "name": "默认" } ] }"#).unwrap();
        assert_eq!(bare.objects[0].primary_shape().kind, ShapeKind::Box);
        assert_eq!(
            bare.objects[0].primary_shape().size,
            None,
            "缺省尺寸靠 ShapeDoc 的缺省值，不写字面量"
        );

        // 错误①：shape 与 shapes 同时写
        let both = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "甲", "shape": { "type": "box" },
                 "shapes": [ { "shape": { "type": "sphere" } } ] } ] }"#,
        );
        assert!(
            both.is_ok(),
            "解析层允许（校验层报错），避免 serde 报错信息不可读"
        );
        let diags = both.unwrap().validate();
        assert!(
            SceneDoc::has_errors(&diags),
            "shape 与 shapes 同时写必须报错：{diags:?}"
        );
        // 错误②：空数组
        let empty =
            SceneDoc::from_json(r#"{ "objects": [ { "name": "乙", "shapes": [] } ] }"#).unwrap();
        assert!(SceneDoc::has_errors(&empty.validate()));
        // 错误③：部件位置 NaN（JSON 里写不出 NaN，用字符串探针验一下别的非法值）
        let bad_pos = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "丙", "shapes": [
                 { "shape": { "type": "box" }, "position": [0, 0, 0], "rotation_deg": [0, 0, 0],
                   "extra": 1 } ] } ] }"#,
        );
        assert!(
            bad_pos.is_err(),
            "部件里的未知字段要报错（deny_unknown_fields）"
        );

        // 警告①：只写一个部件；警告②：半空间当部件
        let warn = SceneDoc::from_json(
            r#"{ "objects": [
                 { "name": "丁", "shapes": [ { "shape": { "type": "box" } } ] },
                 { "name": "戊", "body": "static",
                   "shapes": [ { "shape": { "type": "plane" } },
                               { "shape": { "type": "box" } } ] } ] }"#,
        )
        .unwrap();
        let diags = warn.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("只写了一个部件")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter().any(|m| m.contains("无限半空间当复合形状")),
            "{msgs:?}"
        );
    }

    #[test]
    fn joint_completion_fields_parse_validate_and_round_trip() {
        // 合法：球窝锥形限位 + 断裂阈值 + 铰链伺服（度 → 由 runner 换算成弧度）
        let doc = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "锚", "body": "static" },
                              { "name": "摆", "body": "dynamic" },
                              { "name": "门", "body": "dynamic" } ],
                 "joints": [
                   { "type": "ball", "a": "锚", "b": "摆",
                     "cone_limit": { "half_angle_deg": 25.0, "axis_a": [0, 1, 0], "axis_b": [0, 1, 0] },
                     "break_force": 500.0, "break_torque": 12.0, "collide_connected": true },
                   { "type": "hinge", "a": "锚", "b": "门", "axis": [0, 1, 0],
                     "motor_target_deg": 30.0, "motor_stiffness": 10.0, "motor_damping": 2.0 } ] }"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&doc.validate()));
        let cone = doc.joints[0].cone_limit.as_ref().unwrap();
        assert_eq!(cone.half_angle_deg, 25.0);
        assert_eq!(cone.axis_a, Some([0.0, 1.0, 0.0]));
        assert_eq!(doc.joints[0].break_force, Some(500.0));
        assert_eq!(doc.joints[0].break_torque, Some(12.0));
        assert_eq!(doc.joints[0].collide_connected, Some(true));
        assert_eq!(doc.joints[1].motor_target_deg, Some(30.0));
        assert_eq!(doc.joints[1].motor_stiffness, Some(10.0));
        assert_eq!(doc.joints[1].motor_damping, Some(2.0));

        // JSON 往返：读回来按类型核对
        let back = SceneDoc::from_json(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(
            back.joints[0].cone_limit.as_ref().unwrap().half_angle_deg,
            25.0
        );
        assert_eq!(back.joints[0].break_force, Some(500.0));
        assert_eq!(back.joints[0].collide_connected, Some(true));
        assert_eq!(back.joints[1].motor_target_deg, Some(30.0));

        // 不写 = 全 None（老场景行为不变：不断、不限、缺省不互相碰撞）
        let plain = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "ball", "a": "a", "b": "b" } ] }"#,
        )
        .unwrap();
        let j = &plain.joints[0];
        assert!(j.cone_limit.is_none() && j.break_force.is_none() && j.break_torque.is_none());
        assert!(j.collide_connected.is_none() && j.motor_target_deg.is_none());

        // 非法组合：锥形限位给铰链、半角越界、阈值非正、增益为负
        let bad = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [
                   { "type": "hinge", "a": "a", "b": "b", "cone_limit": { "half_angle_deg": 20.0 } },
                   { "type": "ball", "a": "a", "b": "b",
                     "cone_limit": { "half_angle_deg": 200.0 }, "break_force": 0.0,
                     "break_torque": -3.0, "motor_stiffness": -1.0 } ] }"#,
        )
        .unwrap();
        let diags = bad.validate();
        for path in [
            "joints[0].cone_limit",
            "joints[1].cone_limit.half_angle_deg",
            "joints[1].break_force",
            "joints[1].break_torque",
            "joints[1].motor_stiffness",
        ] {
            assert!(
                diags
                    .iter()
                    .any(|d| d.path == path && d.severity == Severity::Error),
                "缺少对 {path} 的报错：{diags:?}"
            );
        }

        // 提醒类：半角几乎为 0 / 阈值小得离谱 / 同时给伺服目标与速度 / 给错关节类型
        let warn = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [
                   { "type": "ball", "a": "a", "b": "b",
                     "cone_limit": { "half_angle_deg": 0.2 }, "break_force": 0.1 },
                   { "type": "hinge", "a": "a", "b": "b",
                     "motor_target_deg": 30.0, "motor_velocity": 1.0 },
                   { "type": "slider", "a": "a", "b": "b", "motor_damping": 2.0 } ] }"#,
        )
        .unwrap();
        let diags = warn.validate();
        for path in [
            "joints[0].cone_limit.half_angle_deg",
            "joints[0].break_force",
            "joints[1].motor_velocity",
            "joints[2].motor_damping",
        ] {
            assert!(
                diags
                    .iter()
                    .any(|d| d.path == path && d.severity == Severity::Warning),
                "缺少对 {path} 的提醒：{diags:?}"
            );
        }
        assert!(
            !SceneDoc::has_errors(&diags),
            "提醒不该升级成错误：{diags:?}"
        );
    }

    #[test]
    fn physics_materials_parse_validate_and_round_trip() {
        // 合法：世界缺省 + 物体覆盖都写全
        let doc = SceneDoc::from_json(
            r#"{ "physics": { "friction_combine": "min", "restitution_combine": "average",
                            "static_friction_speed": 0.05 },
                 "objects": [
                   { "name": "箱子", "body": "dynamic", "friction": 0.4, "static_friction": 0.8,
                     "friction_combine": "max", "restitution_combine": "multiply" } ] }"#,
        )
        .unwrap();
        assert!(!SceneDoc::has_errors(&doc.validate()));
        assert_eq!(
            doc.physics.friction_combine,
            Some(CombineRuleDoc::Min),
            "世界缺省要解析出来"
        );
        assert_eq!(doc.physics.static_friction_speed, Some(0.05));
        let o = &doc.objects[0];
        assert_eq!(o.static_friction, Some(0.8));
        assert_eq!(o.friction_combine, Some(CombineRuleDoc::Max));
        assert_eq!(o.restitution_combine, Some(CombineRuleDoc::Multiply));

        // JSON 往返：写出去再读回来，数值按类型核对（不做字符串匹配）
        let json = serde_json::to_string(&doc).unwrap();
        let back = SceneDoc::from_json(&json).unwrap();
        assert_eq!(back.physics.friction_combine, Some(CombineRuleDoc::Min));
        assert_eq!(back.objects[0].static_friction, Some(0.8));
        assert_eq!(back.objects[0].friction_combine, Some(CombineRuleDoc::Max));

        // 不写 = 全 None（行为与以前逐位相同）
        let plain = SceneDoc::from_json(r#"{ "objects": [ { "name": "a", "body": "dynamic" } ] }"#)
            .unwrap();
        assert_eq!(plain.physics.friction_combine, None);
        assert_eq!(plain.physics.restitution_combine, None);
        assert_eq!(plain.physics.static_friction_speed, None);
        assert_eq!(plain.objects[0].static_friction, None);
        assert_eq!(plain.objects[0].friction_combine, None);

        // 非法：负的静摩擦 / 负的切换速度 / 未知规则名
        let bad = SceneDoc::from_json(
            r#"{ "physics": { "static_friction_speed": -1.0 },
                 "objects": [ { "name": "a", "body": "dynamic", "static_friction": -0.2 } ] }"#,
        )
        .unwrap();
        let diags = bad.validate();
        assert!(
            diags
                .iter()
                .any(|d| d.path == "objects[0].static_friction" && d.severity == Severity::Error),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.path == "physics.static_friction_speed"
                    && d.severity == Severity::Error),
            "{diags:?}"
        );
        let err = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "dynamic", "friction_combine": "geometric" } ] }"#,
        )
        .unwrap_err();
        // serde 的报错路径指到**物体**上（它不知道是哪个字段），但消息里列全了合法值，
        // 够定位了：`objects[0]` + "expected one of `geometric_mean`, ..."
        let text = format!("{err:?}");
        assert!(text.contains("objects[0]"), "要指到物体上：{text}");
        assert!(text.contains("geometric_mean"), "要列出合法值：{text}");

        // 警告：静摩擦比动摩擦小 / 切换速度偏大 / 没有刚体却写了合成规则
        let warn = SceneDoc::from_json(
            r#"{ "physics": { "static_friction_speed": 3.0 },
                 "objects": [
                   { "name": "a", "body": "dynamic", "friction": 0.9, "static_friction": 0.1 },
                   { "name": "b", "body": "none", "friction_combine": "min" } ] }"#,
        )
        .unwrap();
        let diags = warn.validate();
        assert!(!SceneDoc::has_errors(&diags), "这些都只是警告：{diags:?}");
        assert!(
            diags.iter().any(|d| d.path == "objects[0].static_friction"),
            "{diags:?}"
        );
        assert!(
            diags
                .iter()
                .any(|d| d.path == "physics.static_friction_speed"),
            "{diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.path.starts_with("objects[1]")),
            "{diags:?}"
        );
    }

    #[test]
    fn fog_shadow_quality_and_sky_colors_parse_validate_and_round_trip() {
        // 合法：三节都写
        let doc = SceneDoc::from_json(
            r#"{ "lighting": {
                  "fog": { "color": [0.2, 0.3, 0.4], "density": 0.05 },
                  "shadow_quality": { "strength": 0.7, "radius": 40 },
                  "sky_colors": { "zenith": [0.1, 0.2, 0.5], "horizon": [0.6, 0.7, 0.8],
                                  "ground": [0.05, 0.05, 0.06] } } }"#,
        )
        .unwrap();
        assert!(
            !SceneDoc::has_errors(&doc.validate()),
            "{:?}",
            doc.validate()
        );
        // JSON 往返：字段一个不丢（`skip_serializing_if` 只对没写的通道生效）
        let json = serde_json::to_value(&doc).unwrap();
        let back = SceneDoc::from_value(json.clone()).unwrap();
        assert_eq!(back.lighting.fog.and_then(|f| f.density), Some(0.05));
        assert_eq!(
            back.lighting.shadow_quality.and_then(|q| q.radius),
            Some(40.0)
        );
        assert_eq!(
            back.lighting.sky_colors.and_then(|s| s.zenith),
            Some([0.1, 0.2, 0.5])
        );
        // 浮点是 f32：序列化出来是 0.30000001192092896 而不是 0.3
        assert!((json["lighting"]["fog"]["color"][1].as_f64().unwrap() - 0.3).abs() < 1e-6);

        // 负密度 / 负颜色 / 强度越界 / 半径非正 / 天空颜色为负：全是 error
        let bad = SceneDoc::from_json(
            r#"{ "lighting": {
                  "fog": { "color": [-1.0, 0.3, 0.4], "density": -0.1 },
                  "shadow_quality": { "strength": 1.5, "radius": 0.0 },
                  "sky_colors": { "horizon": [0.6, -0.2, 0.8] } } }"#,
        )
        .unwrap();
        let diags = bad.validate();
        for path in [
            "lighting.fog.color",
            "lighting.fog.density",
            "lighting.shadow_quality.strength",
            "lighting.shadow_quality.radius",
            "lighting.sky_colors.horizon",
        ] {
            let d = diag_at(&diags, path).unwrap_or_else(|| panic!("{path} 必须报错：{diags:?}"));
            assert_eq!(d.severity, Severity::Error, "{path}: {d:?}");
        }
        // NaN / inf 也算非法。JSON 里没有 NaN（`json!` 会把它变成 null），
        // 所以这条只能从**代码路径**构造 —— 校验必须自己挡住，不能指望 serde。
        let mut nan = SceneDoc::from_value(json!({ "lighting": { "fog": {} } })).unwrap();
        nan.lighting.fog.as_mut().unwrap().density = Some(f32::NAN);
        assert!(diag_at(&nan.validate(), "lighting.fog.density").is_some());
        nan.lighting.fog.as_mut().unwrap().density = Some(f32::INFINITY);
        assert!(diag_at(&nan.validate(), "lighting.fog.density").is_some());

        // 大到不合常理：warning 而不是 error（能算出画面，只是难看）
        let odd = SceneDoc::from_json(
            r#"{ "lighting": {
                  "fog": { "color": [9.0, 0.3, 0.4], "density": 2.0 },
                  "shadow_quality": { "radius": 900.0 } } }"#,
        )
        .unwrap();
        let diags = odd.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        for path in [
            "lighting.fog.color",
            "lighting.fog.density",
            "lighting.shadow_quality.radius",
        ] {
            let d = diag_at(&diags, path).unwrap_or_else(|| panic!("{path} 该给警告：{diags:?}"));
            assert_eq!(d.severity, Severity::Warning, "{path}: {d:?}");
        }
        // 没写这三节 = 全是 None（不改变默认观感），且不吃未知字段
        let plain = SceneDoc::from_json(r#"{ "objects": [] }"#).unwrap();
        assert!(plain.lighting.fog.is_none());
        assert!(plain.lighting.shadow_quality.is_none());
        assert!(plain.lighting.sky_colors.is_none());
        assert!(SceneDoc::from_json(r#"{ "lighting": { "fog": { "densty": 0.1 } } }"#).is_err());
    }

    #[test]
    fn version_one_is_rejected_with_a_clear_message() {
        let doc =
            SceneDoc::from_json(r#"{ "format": "quench3d.scene", "version": 1, "objects": [] }"#)
                .unwrap();
        let diags = doc.validate();
        let d = diag_at(&diags, "version").expect("版本 1 必须报错");
        assert!(d.message.contains("当前支持 2"), "{}", d.message);
    }
}

#[cfg(test)]
mod camera_tests {
    use super::*;

    /// 命名相机：解析、校验、往返；保留名与空名必须报错。
    #[test]
    fn named_cameras_parse_validate_and_round_trip() {
        let json = r#"{
            "format": "quench3d.scene", "version": 2,
            "cameras": {
                "正面": { "position": [0, 1, 8], "look_at": [0, 1, 0] },
                "俯视": { "position": [0, 9, 0.1], "look_at": [0, 1, 0], "fov_deg": 35 }
            }
        }"#;
        let doc = SceneDoc::from_json(json).expect("命名相机应当能解析");
        assert_eq!(doc.cameras.len(), 2);
        assert!(
            !SceneDoc::has_errors(&doc.validate()),
            "正常名字不该报错：{:?}",
            doc.validate()
        );

        // 往返：命名相机不能丢
        let round = SceneDoc::from_json(&doc.to_json_pretty()).expect("往返要能再解析");
        assert_eq!(round.cameras.len(), 2);
        assert_eq!(round.cameras["俯视"].fov_deg, Some(35.0));

        // 保留名与空名
        for bad in [
            r#"{"format":"quench3d.scene","version":2,"cameras":{"default":{}}}"#,
            r#"{"format":"quench3d.scene","version":2,"cameras":{" ":{}}}"#,
        ] {
            let d = SceneDoc::from_json(bad).expect("要能解析");
            assert!(SceneDoc::has_errors(&d.validate()), "坏名字必须报错：{bad}");
        }
    }
}

#[cfg(test)]
mod camera_select_tests {
    use super::*;

    fn scene() -> SceneDoc {
        SceneDoc::from_json(
            r#"{
                "format": "quench3d.scene", "version": 2,
                "camera": { "position": [7, 4.5, 8], "look_at": [0, 1.2, 0], "fov_deg": 50 },
                "cameras": {
                    "正面": { "position": [0, 1, 8], "look_at": [0, 1, 0] },
                    "俯视": { "position": [0, 9, 0.1], "look_at": [0, 1, 0], "fov_deg": 35 }
                }
            }"#,
        )
        .expect("解析")
    }

    /// `--camera <名字>`：选中的相机参数要真的搬过来；未知名要报出可用名单。
    #[test]
    fn select_camera_swaps_the_camera_and_reports_unknown_names() {
        let mut doc = scene();
        assert_eq!(doc.camera.fov_deg, Some(50.0), "选之前是 camera 段那台");
        doc.select_camera("俯视").expect("选得到");
        assert_eq!(doc.camera.fov_deg, Some(35.0), "选之后是命名相机的参数");
        assert_eq!(doc.camera.position, Some([0.0, 9.0, 0.1]));
        doc.select_camera("default")
            .expect("default 是 camera 段别名");
        assert_eq!(
            doc.camera.fov_deg,
            Some(35.0),
            "default 不该把已选中的相机换回去"
        );

        let err = doc.select_camera("斜后方").expect_err("未知名必须报错");
        assert!(
            err.contains("俯视") && err.contains("正面"),
            "要列出可用名字：{err}"
        );

        let mut plain = SceneDoc::from_json(r#"{"format":"quench3d.scene","version":2}"#).unwrap();
        let err = plain
            .select_camera("任何")
            .expect_err("没有 cameras 段也要报错");
        assert!(err.contains("没有 `cameras` 段"), "{err}");
    }
}
