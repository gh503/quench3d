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
use std::path::{Path, PathBuf};

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
/// 单个高度场允许的最大格点数（`行 × 列`，例如 256×256）。
///
/// 上限的理由是**物理**不是渲染：高度场会被展开成静态三角网，而三角网的窄相是逐三角形的
/// （有逐三角形 AABB 早退，但没有层次结构），所以 `2·(行-1)·(列-1)` 个三角形每步都要过一遍。
/// 65536 个格点 ≈ 13 万个三角形，正是"能跑但已经是负担"的量级；再大应该把地形切块
/// （多个物体各自一个高度场），而不是抬高这个上限。
pub const MAX_HEIGHTFIELD_POINTS: usize = 65_536;
/// 单个场景允许的最大 `include` 嵌套深度（根文件算第 0 层）。
///
/// 上限存在的理由是"环之外还能爆栈"：`a→b→c→…` 一路下去，每一层都要读盘、解析、合并。
/// 16 层足够表达"关卡 → 区域 → 建筑 → 家具"这种层级，再多基本是把目录树抄了一遍。
pub const MAX_INCLUDE_DEPTH: usize = 16;
/// 被 `include` 的**片段不允许写**的段：它们是根场景独有的。
///
/// 判据不是"能不能合并"，而是"合并语义会不会含糊"：`lighting`/`render`/`simulate`
/// 这些段只有一个实例，两个文件同时写就等于"谁赢"没有答案。宁可报错让作者把配置
/// 放到根文件里，也不要静默地按"根赢"或"先来后到"处理 —— 那是查不出来的坑。
const ROOT_ONLY_SECTIONS: [&str; 10] = [
    "name", "gravity", "physics", "ground", "assets", "camera", "lighting", "simulate", "render",
    "water",
];

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

/// 定义稳定的诊断码：**一处声明同时生成枚举、`ALL` 总表与线上名字**。
///
/// 为什么用宏：诊断码是给 agent 分支用的契约，最怕"枚举加了一个变体、总表忘了加"，
/// 那样 `quench3d schema` 与 `docs/error_codes.md` 会静默漏掉一个码。宏让三件事
/// 从同一行声明里生成，结构上不可能漂移（枚举本身是穷尽的，`as_str` 的 `match`
/// 少一个变体就编译不过）。
macro_rules! define_error_codes {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $wire:literal, $meaning:literal, $action:literal;
    )*) => {
        /// 校验诊断的**稳定机器可读错误码**。
        ///
        /// 契约：`code` 描述**问题的性质**，`path` 描述**问题在哪**。两者合起来才是
        /// 完整的定位 —— 所以码表故意做得窄（十几个）而不是"一条诊断一个码"：
        /// 后者既没法稳定，agent 也没法穷举。
        ///
        /// **码一旦发布就不再改含义、不再改名**（改名等于破坏下游分支）。要新增性质
        /// 就加新码；要调整措辞改 `meaning`/`agent_action` 文案即可，那是给人读的。
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum ErrorCode {
            $(
                $(#[$meta])*
                #[serde(rename = $wire)]
                $variant,
            )*
        }

        impl ErrorCode {
            /// 全部诊断码，顺序与 [`ErrorCode::TABLE`] 一致。
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant,)*];

            /// 码总表：`(码, 线上名字, 含义, agent 该怎么办)`。
            ///
            /// **这是诊断码唯一的真相源**：`quench3d schema --json` 的 `error_codes`
            /// 与 `docs/error_codes.md` 都由测试与它对账，不手工维护第二份。
            pub const TABLE: &'static [(ErrorCode, &'static str, &'static str, &'static str)] =
                &[$((ErrorCode::$variant, $wire, $meaning, $action),)*];

            /// 线上名字（JSON 里 `code` 字段的值，也是文档表里的名字）。
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $wire,)*
                }
            }

            /// 这个码是什么意思（给人和 agent 读）。
            pub const fn meaning(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $meaning,)*
                }
            }

            /// agent 拿到这个码之后该做什么。
            pub const fn agent_action(self) -> &'static str {
                match self {
                    $(ErrorCode::$variant => $action,)*
                }
            }
        }
    };
}

define_error_codes! {
    /// 写了引擎不认识的字段：拼错，或者用了已经删掉的字段。
    UnknownField => "unknown_field",
        "出现了模型/格式里不存在的字段名",
        "按 hint 里的合法字段表改名或删掉；嵌套结构的父字段名也可能只是拼错";
    /// 缺了这条写法必需的字段。
    MissingField => "missing_field",
        "缺了当前写法必需的字段（不同动作类型需要的字段不同）",
        "补上提示里的字段；别靠缺省猜，缺省不等于\"它会自己补\"";
    /// JSON 类型不对。
    InvalidType => "invalid_type",
        "JSON 类型不对（该给数组给了字符串、该给数字给了布尔……）",
        "按提示里的期望类型改 JSON 值（注意 `[x,y,z]` 是数组，不是字符串）";
    /// 取值不在允许集合里。
    InvalidValue => "invalid_value",
        "取值不在允许集合里：枚举拼错、格式/预设/扩展名不支持、空数组、两条轨道长度不一致",
        "换成提示里列出的取值（枚举名区分大小写）；引擎不会对未知取值兜底";
    /// 数值越界、非有限或退化。
    OutOfRange => "out_of_range",
        "数值越界、非有限（NaN/Inf）、符号不对，或退化（零向量/零尺寸/上限小于下限）",
        "按提示的范围改数值；单位见字段说明（`rotation_deg` 是度，铰链 `limits` 是弧度）";
    /// 按名字引用了不存在的东西。
    UnknownReference => "unknown_reference",
        "按名字引用了不存在的东西：物体、刚体、材质、模板或相机",
        "改引用名，或把被引用的东西补进对应段；hint 里通常带可用名单";
    /// 同一命名空间里名字重复。
    DuplicateName => "duplicate_name",
        "同一命名空间里名字重复（`instances` 展开之后的重名也算）",
        "改名；重名会让后者覆盖前者，或让引用挂到错的物体上";
    /// 用了保留名。
    ReservedName => "reserved_name",
        "用了引擎保留的名字（例如相机段里的 `default`）",
        "换成别的名字；保留名的含义见 hint";
    /// 父子链成环。
    Cycle => "cycle",
        "父子引用成环（沿 `parent` 走回自己）",
        "打断环：环上至少一个物体的 `parent` 要指向环外或留空";
    /// `include` 成环。
    IncludeCycle => "include_cycle",
        "`include` 引用成环：A 包含 B，B 又（直接或间接）包含 A",
        "打断环：环上至少有一个文件不要再 include 回上游；公共片段应该被并列包含，而不是互相包含";
    /// 找不到资源文件。
    AssetNotFound => "asset_not_found",
        "找不到贴图/模型文件，或 `assets.search_paths` 指向不存在的目录",
        "把文件放到消息里列出的候选位置之一，或用 `assets.search_paths` / `--asset-path` 加一条搜索路径";
    /// 读写文件失败。
    IoError => "io_error",
        "读写文件失败：不只是\"找不到\"，还包括权限、目录、编码",
        "按消息里的 OS 错误处理路径/权限/编码；这不是场景语法问题，改 JSON 没用";
    /// 超过规模上限。
    LimitExceeded => "limit_exceeded",
        "超过规模上限：物体数、步数、动作数、探针数、局部光名额",
        "拆成多个场景或减少数量；hint 里给了具体上限";
    /// 两个字段只能写一个。
    MutuallyExclusive => "mutually_exclusive",
        "两个字段只能写一个，却同时写了（`shape`/`shapes`、`at_step`/`at_time`……）",
        "删掉其中一个；hint 说明同时写时哪个优先";
    /// 写法合法但不会生效。
    Ineffective => "ineffective",
        "写法合法但不会生效：字段会被忽略、是死字段，或该物体根本没有刚体",
        "要么删掉这个字段，要么改成会生效的写法；**别以为写了就生效**";
    /// 合法但很可能不是本意。
    SuspiciousValue => "suspicious_value",
        "合法但很可能不是本意：数值异常大/小，或语义容易误解（如 `scale` 只影响渲染）",
        "复核这个数值是否真的想要；引擎会照写执行，不会替你夹紧";
    /// 单位容易混。
    UnitAmbiguity => "unit_ambiguity",
        "单位容易混：弧度与度、米与度",
        "确认单位：`rotation_deg`/`motor_target_deg`/`cone_limit` 用度，铰链 `limits` 与马达速度用弧度";
    /// 兜底：还没归类的诊断。
    Invalid => "invalid",
        "兜底码：这条诊断还没有被归类（历史诊断，或新加的检查忘了给码）",
        "只能读 message；如果经常撞到它，给它补一个专门的码（见 docs/error_codes.md）";
}

impl Default for ErrorCode {
    /// 缺省是兜底码 [`ErrorCode::Invalid`]：老 JSON（没有 `code` 字段）反序列化时用它。
    fn default() -> Self {
        ErrorCode::Invalid
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条校验诊断。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 级别。
    pub severity: Severity,
    /// **稳定**的机器可读错误码——agent 按它分支，而不是按中文文案。
    ///
    /// 老 JSON 里没有这个字段时反序列化成 [`ErrorCode::Invalid`]（向后兼容）。
    #[serde(default)]
    pub code: ErrorCode,
    /// JSON 路径，例如 `objects[2].shape.size`。
    pub path: String,
    /// 人可读说明。
    pub message: String,
    /// 可选的修复建议。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Diagnostic {
    /// 造一条错误诊断（兜底码 [`ErrorCode::Invalid`]）。
    ///
    /// 只有"确实没法归类"时才用它；能归类的请用 [`Diagnostic::error_code`]。
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::error_code(ErrorCode::Invalid, path, message)
    }

    /// 造一条警告诊断（兜底码 [`ErrorCode::Invalid`]）。
    pub fn warn(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::warn_code(ErrorCode::Invalid, path, message)
    }

    /// 造一条**带稳定诊断码**的错误诊断。
    pub fn error_code(
        code: ErrorCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Error,
            code,
            path: path.into(),
            message: message.into(),
            hint: None,
        }
    }

    /// 造一条**带稳定诊断码**的警告诊断。
    pub fn warn_code(code: ErrorCode, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code,
            path: path.into(),
            message: message.into(),
            hint: None,
        }
    }

    /// 改写诊断码（少见：先用通用构造再用它覆盖）。
    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = code;
        self
    }

    /// 附上修复建议。
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// 把 serde 的反序列化错误归类成稳定的诊断码。
///
/// 结构检查（未知字段 / 类型不对 / 枚举拼错 / 缺字段）全部经过 `check_at` 这一个出口，
/// 所以在这里分类一次就覆盖了所有字段级问题——不用在几十个调用点各写一遍。
pub fn classify_serde_code(message: &str) -> ErrorCode {
    let m = message;
    if m.contains("unknown field") {
        ErrorCode::UnknownField
    } else if m.contains("missing field") {
        ErrorCode::MissingField
    } else if m.contains("unknown variant") || m.contains("invalid value") {
        ErrorCode::InvalidValue
    } else if m.contains("invalid type")
        || m.contains("invalid length")
        || m.starts_with("expected")
    {
        ErrorCode::InvalidType
    } else {
        ErrorCode::Invalid
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
    /// 水域（浮力环境）：水面高度 + XZ 范围，**没有流体求解器**。
    ///
    /// 不写 = 没有水（物理结果与以前逐位相同）。写了之后，动态刚体按"排开体积"
    /// 受浮力与阻尼，睡眠体不受力（不会被浮力一直唤醒）。
    /// 这是 M1 的解析水域；粒子流体（PBF）还没做。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waters: Vec<WaterDoc>,
    /// 粒子流体（CPU PBF）：长方体发射器 + 粒子参数。
    ///
    /// 不写 = 没有粒子流体（物理结果与以前逐位相同）。流体按**排开体积**被刚体
    /// 推开，并把法向冲量反推给动态刚体；粒子的自由表面还给刚体浮力/阻尼（M3 双向）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fluids: Vec<FluidDoc>,
    /// 屏幕空间水的全局风格（颜色/清澈度/波浪/泡沫/折射）。
    ///
    /// 不写 = 使用渲染器缺省值；写了只影响**画面**，不影响物理（浮力/粒子结果逐位相同）。
    /// 这是根独有段：`include` 片段里写会报错（与 `lighting`/`render` 一样）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub water: Option<WaterStyleDoc>,
    /// 具名材质。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub materials: BTreeMap<String, MaterialDoc>,
    /// 被包含的**外部场景片段**（`["parts/wall.json", "../shared/common.json"]`）。
    ///
    /// 语义（六条，`resolved()` 是唯一实现）：
    /// 1. 相对路径以**写这一行的那份文件所在目录**为基准；候选顺序与贴图/模型同一套
    ///    （文件目录 → `assets.search_paths` → `--asset-path` → 工作目录 → 引擎搜索路径）。
    /// 2. 片段必须是合法的 `quench3d.scene`（同样的字段集），但只能贡献**可合并的段**：
    ///    `materials` / `templates` / `cameras`（具名表，重名即错误）与
    ///    `waters` / `fluids` / `objects` / `models` / `joints` / `actions` / `probes` / `animations`
    ///    （数组，按顺序追加）。
    ///    根独有的段（`ground`/`camera`/`lighting`/`simulate`/`render`/`water`/`gravity`/`physics`/`name`/`assets`）
    ///    写在片段里是错误（根独有的段列在源码的 `ROOT_ONLY_SECTIONS` 常量里）。
    /// 3. 顺序：**本文件自己的内容在前**，被 include 的内容在后（按声明顺序先序深度优先）。
    /// 4. 片段里的相对资源路径（4 个贴图槽、`models[].path`）在装载时按**片段自己的目录**
    ///    重定基成"相对根场景目录"的路径 —— 所以片段搬到哪都跟着自己走，而校验/渲染只需要
    ///    根目录一个基准（不会出现"贴图两套解析规则"）。`assets.search_paths` 不在此列：
    ///    它是根独有的段（见第 2 条），片段写了就是错误。
    /// 5. 片段可以再 `include`（最多 [`MAX_INCLUDE_DEPTH`] 层）；**同一个文件在一次解析里只并入一次**
    ///    （菱形包含会得到一条警告并跳过第二次）。成环直接报错（`include_cycle`）。
    /// 6. **不写这个字段 = 行为与以前逐字节相同**（不读任何额外文件、不重定基任何路径）。
    ///
    /// 跨文件 prefab 的推荐写法：片段只放 `templates`（不放 `objects`），根场景用
    /// `objects: [{ "template": "箱子", "instances": [...] }]` 实例化 —— 这样同一份
    /// 定义可以被多个场景复用，也不会因为"多摆了一个物体"而重名。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
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
    /// 这份文档来自哪个文件（**不参与序列化**，由 [`SceneDoc::from_file`] 填入）。
    ///
    /// 只有 `include` 解析用得到它：根文件自己也要进"已包含"集合，否则
    /// `a.json → b.json → a.json` 这种环会退化成"把 a 再并一遍"。
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
    /// CLI 用 `--asset-path` 给的额外搜索路径（**不参与序列化**，也不会写回场景文件）。
    ///
    /// 与 `assets.search_paths` 同样处理，只是排在它们**之后**：命令行是"这一次运行"的
    /// 临时补充，场景文件才是这份场景自己的约定。相对路径同样以场景目录为基准。
    #[serde(skip)]
    pub extra_asset_paths: Vec<PathBuf>,
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
    /// 逐位不变）。**单形状与复合形状都生效**（复合刚体逐部件膨胀）。
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
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.name"),
                    "模板里的 name 没有意义（名字属于物体/实例），会被忽略",
                ));
            }
            if t.parent.is_some() || t.position.is_some() || t.rotation_deg.is_some() {
                out.push(Diagnostic::warn_code(ErrorCode::Ineffective,
                    &p,
                    "模板里的 parent / position / rotation_deg 没有意义（模板只描述\"长什么样\"），会被忽略；位置请写在 instances 或物体上",
                ));
            }
            if t.template.is_some() || t.instances.is_some() {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
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
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            format!("{p}.template"),
                            format!("找不到模板 `{t}`"),
                        )
                        .with_hint(hint),
                    );
                }
            }
            match o.instances.as_deref() {
                Some([]) => out.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    format!("{p}.instances"),
                    "instances 不能是空数组（要么删掉，要么至少写一个副本）",
                )),
                Some(list) => {
                    // 实例多起来是实打实的物理开销（每个副本一个独立刚体，不是 GPU 实例化）
                    if list.len() > 64 {
                        out.push(Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
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
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{ip}.position"),
                                    "实例位置必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &inst.rotation_deg {
                            if !finite3(v) {
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{ip}.rotation_deg"),
                                    "实例欧拉角必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &inst.scale {
                            if !finite3(v) || v.iter().any(|x| *x <= 0.0) {
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
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
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
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
                Diagnostic::error_code(
                    ErrorCode::DuplicateName,
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
            waters: Vec::new(),
            fluids: Vec::new(),
            water: None,
            materials: BTreeMap::new(),
            include: Vec::new(),
            objects: Vec::new(),
            templates: BTreeMap::new(),
            models: Vec::new(),
            assets: None,
            extra_asset_paths: Vec::new(),
            joints: Vec::new(),
            actions: Vec::new(),
            probes: Vec::new(),
            camera: CameraDoc::default(),
            lighting: LightingDoc::default(),
            simulate: SimulateDoc::default(),
            render: RenderDoc::default(),
            base_dir: None,
            source_path: None,
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

/// 一块水域（浮力环境）。
///
/// 不是流体：没有粒子、没有流动，只有"水面以下排开体积"产生的浮力。
/// 水面是**半空间**（`y <= level` 且落在 XZ 范围内就算浸没），没有池底；
/// 要池底就把地面放在水下。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterDoc {
    /// XZ 中心 `[x, z]`，缺省 `[0, 0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<[f32; 2]>,
    /// XZ 尺寸 `[x, z]`（米），必须为正。
    pub size: [f32; 2],
    /// 水面高度（世界 Y）。
    pub level: f32,
    /// 密度（kg/m³），缺省 1000（水）。
    #[serde(default = "default_water_density")]
    pub density: f32,
    /// 浸没部分的线性阻尼（1/s），缺省 6；0 = 无阻尼。
    #[serde(default = "default_water_drag")]
    pub drag: f32,
}

fn default_water_density() -> f32 {
    1000.0
}

fn default_water_drag() -> f32 {
    6.0
}

/// 屏幕空间水的全局外观。
///
/// 所有 `waters` / `fluids` 共用这一份风格：后处理是逐像素的，水面 mask 不携带
/// "这是哪个水域"的索引，所以不做逐水域颜色。缺省值由渲染器提供。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WaterStyleDoc {
    /// 水色（线性 RGB，0~1）；缺省 `[0.06, 0.28, 0.48]`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// 清澈度 0~1（0 = 浑浊/强吸收，1 = 清澈）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clarity: Option<f32>,
    /// 波浪强度 0~4（0 = 平静水面）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wave_strength: Option<f32>,
    /// 泡沫强度 0~2（0 = 无泡沫）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foam: Option<f32>,
    /// 折射强度 0~2（0 = 不扰动背景）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refraction: Option<f32>,
}

/// 粒子流体（CPU PBF）。
///
/// 这是**粒子**流体，与 [`WaterDoc`] 的解析水域是两套东西：水域给刚体浮力，
/// 流体给"看得见的一堆粒子"。两者可以同时出现，互不替换。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FluidDoc {
    /// 发射体中心 `[x, y, z]`，缺省 `[0, 0, 0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<[f32; 3]>,
    /// 发射体尺寸 `[x, y, z]`（米），必须为正。
    pub size: [f32; 3],
    /// 粒子间距（米），缺省 0.1。
    #[serde(default = "default_fluid_spacing")]
    pub spacing: f32,
    /// 粒子碰撞半径（米），缺省 `spacing * 0.5`；必须不大于 `spacing`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// 静止密度（kg/m³），缺省 1000（水）。
    #[serde(default = "default_fluid_density")]
    pub rest_density: f32,
    /// XSPH 粘性系数 0~1，缺省 0.05。
    #[serde(default = "default_fluid_viscosity")]
    pub viscosity: f32,
    /// 浸没刚体的线性阻尼（1/s），缺省 6；0 = 无阻尼。
    #[serde(default = "default_fluid_drag")]
    pub drag: f32,
    /// 密度约束迭代次数 1~8，缺省 4。
    #[serde(default = "default_fluid_iterations")]
    pub iterations: u32,
    /// 粒子上限 1~20000，缺省 4000；超出按晶格顺序截断。
    #[serde(default = "default_fluid_max_particles")]
    pub max_particles: usize,
}

impl FluidDoc {
    /// 实际使用的粒子半径。
    pub fn radius(&self) -> f32 {
        self.radius.unwrap_or(self.spacing * 0.5)
    }

    /// 按晶格估算的粒子数（**不**按 `max_particles` 截断）。
    pub fn raw_particles(&self) -> usize {
        let n = |size: f32| {
            if !(size.is_finite() && self.spacing.is_finite() && self.spacing > 0.0) {
                return 0;
            }
            ((size / self.spacing).floor() as usize).saturating_add(1)
        };
        n(self.size[0])
            .saturating_mul(n(self.size[1]))
            .saturating_mul(n(self.size[2]))
    }

    /// 实际会发射的粒子数（按 `max_particles` 截断）。
    pub fn estimated_particles(&self) -> usize {
        self.raw_particles().min(self.max_particles)
    }
}

fn default_fluid_spacing() -> f32 {
    0.1
}

fn default_fluid_density() -> f32 {
    1000.0
}

fn default_fluid_viscosity() -> f32 {
    0.05
}

fn default_fluid_drag() -> f32 {
    6.0
}

fn default_fluid_iterations() -> u32 {
    4
}

fn default_fluid_max_particles() -> usize {
    4000
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
    /// 圆锥（底面半径 `radius`、半高 `half_height`；底面在 `-half_height`、顶点在 `+half_height`）。
    Cone,
    /// 圆角盒（外接**边长** `size` + 圆角半径 `radius`；`radius` 必须显式给出且 ≤ 最短边长的一半）。
    ///
    /// 几何上是"盒子 ⊕ 半径 `radius` 的球"：`radius` 越小越像盒子，等于最短边长的一半时
    /// （三边相等的话）就是一个球。物理支持函数是精确的，惯量见 `docs/production_readiness.md`。
    ///
    /// 显式写 `rename`：`rename_all = "lowercase"` 会把它变成 `roundedbox`，
    /// 而格式文档/校验/系统提示里都是 `rounded_box` —— 蛇形名得单独钉住。
    #[serde(rename = "rounded_box")]
    RoundedBox,
    /// 高度场（`heights` 行主序：行沿 Z、列沿 X；`cell_size` 缺省 1.0）。
    ///
    /// 网格在 XZ 平面以形状原点为中心，`heights[行][列]` 是相对原点的高度。
    /// 物理上是**静态三角网**，所以只能 `body: "static"`（`dynamic`/`kinematic` 会被校验拦下）。
    HeightField,
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
    /// 胶囊/圆柱/圆锥的半高（胶囊不含两端半球；圆锥是底面到顶点的一半），缺省 0.5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_height: Option<f32>,
    /// 平面法线，缺省 `[0, 1, 0]`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f32; 3]>,
    /// 凸包顶点。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<Vec<[f32; 3]>>,
    /// 高度场的高度，行主序（`heights[行][列]`，行沿 Z、列沿 X），至少 2×2。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heights: Option<Vec<Vec<f32>>>,
    /// 高度场的格子边长（X/Z 方向的间距），缺省 1.0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_size: Option<f32>,
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
            heights: None,
            cell_size: None,
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

/// `--asset-path` 的进程级存放处。
///
/// 为什么不只放在 `SceneDoc` 上：命令实现里还有**自己再 `from_file` 一次**的地方
/// （多输出渲染、内部校验…），逐个传参总会漏掉一处，而漏掉的表现是"旗标有时不生效"——
/// 最难查的那种。这里放一份，`asset_candidates` 无条件读它。
static CLI_ASSET_PATHS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// 登记 `--asset-path`（只应由 CLI 入口调用一次；重复调用会被忽略）。
pub fn set_cli_asset_paths(paths: Vec<PathBuf>) {
    let _ = CLI_ASSET_PATHS.set(paths);
}

/// 资源搜索路径。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetsDoc {
    /// 依次尝试的目录列表（相对路径以场景文件所在目录为基准）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_paths: Vec<PathBuf>,
}

/// 模型文件里一条 glTF 动画的**播放设置**。
///
/// 这是"视觉"动画：它只改模型展开出来的物体的变换，**不动物理**（模型的可选碰撞体
/// 是绑定姿态下的静态三角网，不会跟着动）。名字是否存在于资产里，`validate` 判断不了
/// （它不读资产），只能在装载后由 `simulate` / `render` 报告的
/// `models[].animation.found` 给出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelAnimationDoc {
    /// 动画名（glTF `animations[].name`；没有名字的资产用 `animation_{下标}`）。
    pub name: String,
    /// 从第几秒开始播放，缺省 0。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<f32>,
    /// 播放倍速，缺省 1（必须 > 0）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    /// 是否循环，缺省 **true**（glTF 片段通常是循环的待机/行走）。
    ///
    /// 写 `false` = 播完停在最后一帧。注意与 `animations[].loop`（关键帧动画，缺省
    /// false）方向相反：那边是"一次性动作"，这边是"片段循环"。两处都会在
    /// `schema --json` 的字段表里写明缺省值。
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub looping: Option<bool>,
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
    /// 播放模型里的一条 glTF 动画；**不写 = 静止**（老行为逐字节不变）。
    ///
    /// 名字不存在不会让场景非法（`validate` 不读资产），但会在 `simulate` / `render`
    /// 的 `models[].animation.found` 里报 `false`，几何保持绑定姿态。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<ModelAnimationDoc>,
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
    /// A 端的局部轴（铰链转轴 / 滑块滑轴）；**不写 = 用 `axis`**。
    ///
    /// 只有两端刚体朝向一致时，`axis` 那一份局部轴才能同时描述两端的轴。门框平放、
    /// 门板转了 90° 时，同一个世界方向在两端分别是"局部 Y"和"局部 Z"——这时必须
    /// 分别写 `axis_a` / `axis_b`，否则求解器会先把 B 扭到与 A 对齐（表现成"门装上去
    /// 自己弹回原位"）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis_a: Option<[f32; 3]>,
    /// B 端的局部轴；**不写 = 用 `axis`**。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis_b: Option<[f32; 3]>,
    /// A 端的**参考姿态**（欧拉角，度，与物体 `rotation_deg` 同一套 XYZ 顺序）。
    ///
    /// 只有 `fixed` 关节读它。求解器会驱动两端扭到**参考姿态在世界中重合**，即
    /// `q_a ⊗ frame_a == q_b ⊗ frame_b`（`q_*` 是刚体世界朝向）。典型用法：两块板各自的
    /// "正面"是局部 +X 与 +Z，写 `frame_a` / `frame_b` 把两个正面转到一起，就不必手算
    /// 世界朝向。
    ///
    /// **不写（或两端都写 0）= 老行为**：固定关节锁定**创建那一刻**的相对姿态，
    /// 参考姿态不参与求解。只要有一端不是 0 就切到"扭到参考姿态"那一支。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_a: Option<[f32; 3]>,
    /// B 端的参考姿态（欧拉角，度）；与 `frame_a` 配套，见那里的说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_b: Option<[f32; 3]>,
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
    /// 是否投射阴影，缺省 **false**（不写 = 老的"只照亮、不投影"，像素逐位不变）。
    ///
    /// 整帧**只支持一盏**投影点光源：多盏同时要求投影时，按 `point_lights` 的顺序取
    /// **第一盏**（规则见 `quench3d_render::point_shadow::pick_point_shadow`），
    /// 校验会给一条警告说明实际用的是哪一盏。
    ///
    /// 点光源阴影与聚光灯阴影是**两套独立资源**（6 层深度数组 vs 一张 2D 图），
    /// 所以"一盏投影点光源 + 一盏投影聚光灯"可以同时存在。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_shadows: Option<bool>,
}

/// 聚光灯：有方向的局部光，只照一个圆锥。
///
/// 与点光源**共享**引擎的局部光名额（8 盏，点光源优先），所以场景里点光源 + 聚光灯
/// 一共超过 8 盏时会有警告，超出的部分不参与着色。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpotLightDoc {
    /// 位置（世界坐标）。
    pub position: [f32; 3],
    /// 照射方向（会被归一化；零向量是错误）。
    pub direction: [f32; 3],
    /// 颜色 `[r, g, b]`，缺省白色（**线性空间**，与 `point_lights.color` 一致）。
    #[serde(default = "default_light_color")]
    pub color: [f32; 3],
    /// 强度（线性），缺省 12。
    #[serde(default = "default_light_intensity")]
    pub intensity: f32,
    /// 影响半径（米），缺省 18，必须为正。
    #[serde(default = "default_light_radius")]
    pub radius: f32,
    /// 内锥角（度）：这个角度内是全强度，缺省 20。
    #[serde(default = "default_spot_inner_deg")]
    pub inner_deg: f32,
    /// 外锥角（度）：这个角度外完全没有光，缺省 30，必须大于内锥角。
    #[serde(default = "default_spot_outer_deg")]
    pub outer_deg: f32,
    /// 是否投射阴影，缺省 **false**（不写 = 老的"只照亮、不投影"，像素逐位不变）。
    ///
    /// 整帧**只支持一盏**投影聚光灯：多盏同时要求投影时，按 `spot_lights` 的顺序取
    /// **第一盏**（规则见 `quench3d_render::spot_shadow::pick_spot_shadow`），
    /// 校验会给一条警告说明实际用的是哪一盏。
    ///
    /// 点光源的投影是**另一套资源**（6 面立方体深度数组，见
    /// [`PointLightDoc::cast_shadows`]），所以两边各有一盏互不影响。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_shadows: Option<bool>,
}

fn default_spot_inner_deg() -> f32 {
    20.0
}

fn default_spot_outer_deg() -> f32 {
    30.0
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
    /// 聚光灯列表（有方向的局部光）。`None`（不写）与 `[]`（显式空）行为一致，
    /// 都没有聚光灯；与点光源共享 8 盏的局部光名额。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot_lights: Option<Vec<SpotLightDoc>>,
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
        out.push(Diagnostic::error_code(
            ErrorCode::InvalidType,
            "$",
            "场景根节点必须是 JSON 对象",
        ));
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
                    Diagnostic::error_code(
                        ErrorCode::UnknownField,
                        key,
                        format!("未知字段 \"{key}\""),
                    )
                    .with_hint(
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
    check_array::<WaterDoc>(map.get("waters"), "waters", &mut out);
    check_array::<FluidDoc>(map.get("fluids"), "fluids", &mut out);
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::InvalidType,
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
            out.push(Diagnostic::error_code(
                ErrorCode::InvalidType,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::InvalidType,
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
            out.push(Diagnostic::error_code(
                ErrorCode::InvalidType,
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
    check_field::<[f32; 3]>(map, "axis_a", path, out);
    check_field::<[f32; 3]>(map, "axis_b", path, out);
    check_field::<[f32; 3]>(map, "anchor_a", path, out);
    check_field::<[f32; 3]>(map, "anchor_b", path, out);
    check_field::<[f32; 3]>(map, "frame_a", path, out);
    check_field::<[f32; 3]>(map, "frame_b", path, out);
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
        let message = clean_serde_message(&e.to_string());
        // 类型/枚举/缺字段这一类问题全部从这里出去，所以在**唯一出口**分类一次，
        // 就覆盖了所有字段级诊断（散在各个 `check_*` 里迟早会漏）。
        out.push(Diagnostic::error_code(
            classify_serde_code(&message),
            path,
            message,
        ));
    }
}

/// 去掉 serde 错误里冗余的定位后缀（路径已经比行列号更好用）。
fn clean_serde_message(msg: &str) -> String {
    match msg.find(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

/// `include` 的解析结果。
///
/// 单独一个结构体而不是 `Result<SceneDoc, Vec<Diagnostic>>`：解析期**警告**
/// （菱形包含被跳过）也要能传出去，而"有警告"并不阻止场景运行。
#[derive(Debug, Clone)]
pub struct ResolvedDoc {
    /// 合并后的文档；只要有一条 `Error` 级诊断就是 `None`。
    pub doc: Option<SceneDoc>,
    /// 解析期诊断（文件缺失、成环、重名冲突……），error 与 warning 都在这里。
    pub diagnostics: Vec<Diagnostic>,
}

impl ResolvedDoc {
    /// 有没有 `Error` 级诊断。
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// 把 `Error` 级诊断拼成一句话（给没有诊断通道的调用点用，例如 `RunError`）。
    pub fn error_message(&self) -> String {
        let errors: Vec<String> = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| format!("{}: {}", d.path, d.message))
            .collect();
        if errors.is_empty() {
            return String::new();
        }
        format!(
            "include 解析失败（{} 条）—— {}",
            errors.len(),
            errors.join("；")
        )
    }
}

/// `include` 解析的进行时状态。
struct IncludeCtx {
    diags: Vec<Diagnostic>,
    /// 已经并入过的文件（规范化绝对路径）：既是"只并一次"的依据，也是报环的待查集合。
    seen: Vec<PathBuf>,
    /// 当前递归栈（规范化绝对路径）：报环时给出完整链条。
    stack: Vec<PathBuf>,
    /// 具名段的键（`materials/木纹`）→ 它来自哪个文件（重名冲突时告诉用户两边是谁）。
    origin: BTreeMap<String, String>,
    /// 根场景所在目录：片段里的相对资源路径以它为基准重定基。
    root_dir: Option<PathBuf>,
}

impl IncludeCtx {
    fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| d.severity == Severity::Error)
    }

    /// 装载一个被 include 的文件，返回它**贡献的合并内容**（片段自己 + 它 include 的内容）。
    ///
    /// `parent` 是写这一行 `include` 的文档：候选路径按**它**的目录展开（相对路径以
    /// 写这一行的文件为基准）。任何问题都变成 `self.diags` 里的一条诊断并返回 `None`。
    fn load(&mut self, parent: &SceneDoc, rel: &str, ref_path: &str) -> Option<SceneDoc> {
        let rel = rel.trim();
        if rel.is_empty() {
            self.diags.push(
                Diagnostic::error_code(ErrorCode::InvalidValue, ref_path, "include 的路径不能为空")
                    .with_hint("删掉这一项，或写一个相对文件名（相对**写这一行的文件**所在目录）"),
            );
            return None;
        }
        let candidates = parent.asset_candidates(rel);
        let found = match candidates.iter().find(|p| p.exists()) {
            Some(p) => p.clone(),
            None => {
                let tried: Vec<String> =
                    candidates.iter().map(|p| p.display().to_string()).collect();
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::AssetNotFound,
                        ref_path,
                        format!(
                            "找不到被 include 的场景文件 \"{rel}\"（找过：{}）",
                            tried.join("、")
                        ),
                    )
                    .with_hint(
                        "相对路径以**写这一行的文件**所在目录为基准；也可以写进 `assets.search_paths` 或加 `--asset-path`",
                    ),
                );
                return None;
            }
        };
        if found.is_dir() {
            self.diags.push(
                Diagnostic::error_code(
                    ErrorCode::IoError,
                    ref_path,
                    format!("{} 是目录，include 需要的是一个场景文件", found.display()),
                )
                .with_hint("把文件名补全（例如 parts/wall.json）"),
            );
            return None;
        }
        let canon = std::fs::canonicalize(&found).unwrap_or_else(|_| found.clone());
        if let Some(pos) = self.stack.iter().position(|p| *p == canon) {
            let chain: Vec<String> = self.stack[pos..]
                .iter()
                .chain(std::iter::once(&canon))
                .map(|p| p.display().to_string())
                .collect();
            self.diags.push(
                Diagnostic::error_code(
                    ErrorCode::IncludeCycle,
                    ref_path,
                    format!("include 成环：{}", chain.join(" → ")),
                )
                .with_hint("环上至少一个文件不要再 include 回上游；公共片段应当被并列包含"),
            );
            return None;
        }
        if self.seen.contains(&canon) {
            self.diags.push(
                Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    ref_path,
                    format!(
                        "{} 已经被包含过，这一处跳过（同一个文件在一次解析里只并入一次）",
                        found.display()
                    ),
                )
                .with_hint("要摆两份就把它写成一个模板，用 `objects[].instances` 摆多个副本"),
            );
            return None;
        }
        if self.stack.len() >= MAX_INCLUDE_DEPTH {
            self.diags.push(
                Diagnostic::error_code(
                    ErrorCode::LimitExceeded,
                    ref_path,
                    format!("include 嵌套超过 {MAX_INCLUDE_DEPTH} 层"),
                )
                .with_hint("把层次拆平，或把公共片段直接列在根文件的 `include` 里"),
            );
            return None;
        }
        let text = match std::fs::read_to_string(&found) {
            Ok(t) => t,
            Err(e) => {
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::IoError,
                        ref_path,
                        format!("读不了被 include 的文件 {}：{e}", found.display()),
                    )
                    .with_hint("检查权限与编码（场景文件必须是 UTF-8）"),
                );
                return None;
            }
        };
        // 先看原始 JSON：片段"写了哪些段"要按**原文**判断（反序列化之后
        // "没写"与"写了缺省值"分不出来，而根独有的段必须按"写没写"来判）。
        let raw: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidType,
                        ref_path,
                        format!("{} 不是合法 JSON：{e}", found.display()),
                    )
                    .with_hint("先让它自己能被解析：`quench3d validate <这个文件>`"),
                );
                return None;
            }
        };
        if let Some(f) = raw.get("format").and_then(|v| v.as_str()) {
            if f != FORMAT {
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        ref_path,
                        format!(
                            "{} 的 format 是 \"{f}\"，应当是 \"{FORMAT}\"",
                            found.display()
                        ),
                    )
                    .with_hint("片段也是一份 quench3d.scene，只是被包含进来"),
                );
            }
        }
        if let Some(v) = raw.get("version").and_then(|v| v.as_u64()) {
            if v != u64::from(parent.version) {
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        ref_path,
                        format!(
                            "{} 的 version 是 {v}，与包含它的文件（{}）不一致",
                            found.display(),
                            parent.version
                        ),
                    )
                    .with_hint("把版本号改成一致；版本不匹配的片段不会被按新语义解释"),
                );
            }
        }
        for key in ROOT_ONLY_SECTIONS {
            if raw.get(key).is_some() {
                self.diags.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        format!("{ref_path}.{key}"),
                        format!(
                            "{key} 是根场景独有的段，被 include 的片段（{}）不能写它",
                            found.display()
                        ),
                    )
                    .with_hint(format!("把 {key} 移到根场景文件里，或从片段里删掉")),
                );
            }
        }
        let mut frag = match SceneDoc::from_json(&text) {
            Ok(d) => d,
            Err(e) => {
                self.push_parse_error(e, &found, ref_path);
                return None;
            }
        };
        let file_dir = found.parent().map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        });
        frag.base_dir = file_dir.clone();
        frag.source_path = Some(found.clone());
        // ① 这一层**自己的**资源路径按它自己的目录重定基。
        //    每个文件只做一次（嵌套时子片段在它自己的 `load` 里已经做过），
        //    所以不会出现"重定基两次"把路径指到别处去。
        self.rebase(&mut frag, file_dir.as_deref());
        // ② 片段自己的 include（相对片段所在目录；`frag` 就是那个"写这一行的文件"）
        self.stack.push(canon.clone());
        self.seen.push(canon);
        let mut contribution = SceneDoc::default();
        // 与根同一套顺序：自己的内容在前，自己 include 的内容在后。
        let label = found.display().to_string();
        self.merge_own(&mut contribution, &frag, ref_path, &label);
        for (i, rel2) in frag.include.iter().enumerate() {
            let ref_path2 = format!("{ref_path}.include[{i}]");
            if let Some(sub) = self.load(&frag, rel2, &ref_path2) {
                let label2 = sub
                    .source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| rel2.clone());
                self.merge_own(&mut contribution, &sub, &ref_path2, &label2);
            }
        }
        self.stack.pop();
        contribution.source_path = Some(found);
        Some(contribution)
    }

    /// 把 `doc` 里**可合并的段**并进 `into`。
    ///
    /// 具名表（材质/模板/相机）重名是**错误**：静默覆盖会让引用挂到错的东西上，
    /// 而这类错误在渲染结果里完全看不出来。数组按顺序追加（所以合并顺序就是报告顺序）。
    fn merge_own(&mut self, into: &mut SceneDoc, doc: &SceneDoc, ref_path: &str, label: &str) {
        let materials: Vec<(String, MaterialDoc)> = doc
            .materials
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in materials {
            self.insert_named(&mut into.materials, "materials", &k, &v, ref_path, label);
        }
        let templates: Vec<(String, ObjectDoc)> = doc
            .templates
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in templates {
            self.insert_named(&mut into.templates, "templates", &k, &v, ref_path, label);
        }
        let cameras: Vec<(String, CameraDoc)> = doc
            .cameras
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in cameras {
            self.insert_named(&mut into.cameras, "cameras", &k, &v, ref_path, label);
        }
        into.waters.extend(doc.waters.iter().cloned());
        into.fluids.extend(doc.fluids.iter().cloned());
        into.objects.extend(doc.objects.iter().cloned());
        into.models.extend(doc.models.iter().cloned());
        into.joints.extend(doc.joints.iter().cloned());
        into.actions.extend(doc.actions.iter().cloned());
        into.probes.extend(doc.probes.iter().cloned());
        into.animations.extend(doc.animations.iter().cloned());
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_named<T: Clone>(
        &mut self,
        into: &mut BTreeMap<String, T>,
        section: &str,
        key: &str,
        value: &T,
        ref_path: &str,
        label: &str,
    ) {
        let slot = format!("{section}/{key}");
        if into.contains_key(key) {
            let prev = self
                .origin
                .get(&slot)
                .cloned()
                .unwrap_or_else(|| "更早的定义".to_string());
            self.diags.push(
                Diagnostic::error_code(
                    ErrorCode::DuplicateName,
                    format!("{ref_path}.{section}.{key}"),
                    format!("`{key}` 在 {section} 里重复定义：已在 {prev} 定义，这里又来了一份（{label}）"),
                )
                .with_hint("改名，或把两份定义合并；重名会让后者覆盖前者、引用挂到错的东西上"),
            );
            return;
        }
        self.origin.entry(slot).or_insert_with(|| label.to_string());
        into.insert(key.to_string(), value.clone());
    }

    /// 把片段里的**相对资源路径**重定基成"相对根场景目录"的路径。
    ///
    /// 不做这件事的话，片段里的 `"base_color_texture": "tex/brick.png"` 会按**根场景**
    /// 目录去找（`asset_candidates` 只有根一个 `base_dir`），于是"片段搬到哪、贴图就跟到哪"
    /// 这件事不成立。`include` 自己的路径**不重定基**：它由 `load` 用片段文档的
    /// `asset_candidates` 解析，基准本来就是片段自己的目录。
    fn rebase(&mut self, doc: &mut SceneDoc, file_dir: Option<&Path>) {
        let Some(file_dir) = file_dir else {
            return;
        };
        let Some(from) = canonical_dir(file_dir) else {
            return;
        };
        let root = self.root_dir.clone();
        for m in doc.materials.values_mut() {
            for slot in [
                &mut m.base_color_texture,
                &mut m.normal_texture,
                &mut m.metallic_roughness_texture,
                &mut m.emissive_texture,
            ]
            .into_iter()
            .flatten()
            {
                *slot = rebase_path(slot, &from, root.as_deref());
            }
        }
        for m in doc.models.iter_mut() {
            let s = m.path.to_string_lossy().to_string();
            m.path = PathBuf::from(rebase_path(&s, &from, root.as_deref()));
        }
        // `assets` 是根独有的段（片段写它会被判错），所以这里不需要重定基它。
    }

    /// 把片段自己的解析错误挂到引用点上（路径前缀成 `include[i]:objects[2]`），
    /// 这样一次 `validate` 就能看出"是哪一份片段、哪一行出的问题"。
    fn push_parse_error(&mut self, e: DocError, found: &Path, ref_path: &str) {
        match e {
            DocError::Invalid(diags) => {
                for mut d in diags {
                    d.path = format!("{ref_path}:{}", d.path);
                    let note = format!("（来自 {}）", found.display());
                    d.hint = Some(match d.hint {
                        Some(h) => format!("{h}{note}"),
                        None => note,
                    });
                    self.diags.push(d);
                }
            }
            other => {
                self.diags.push(Diagnostic::error_code(
                    ErrorCode::InvalidType,
                    ref_path,
                    format!("{} 解析失败：{other}", found.display()),
                ));
            }
        }
    }
}

/// 规范化成绝对目录（不存在或不是目录时 `None`）。
fn canonical_dir(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok().filter(|c| c.is_dir())
}

/// 把 `p`（相对 `from_dir`）改写成相对 `root_dir` 的路径；`root_dir` 缺失时退化成绝对路径。
///
/// 绝对路径原样返回（作者写了绝对路径就是他的意思，不去猜）。
fn rebase_path(p: &str, from_dir: &Path, root_dir: Option<&Path>) -> String {
    let raw = Path::new(p);
    if raw.is_absolute() {
        return p.to_string();
    }
    let abs = from_dir.join(raw);
    let out = match root_dir {
        Some(root) => relative_path(root, &abs),
        None => abs,
    };
    out.to_string_lossy().replace('\\', "/")
}

/// 纯字典序的相对路径（两边都必须是绝对路径）：`root` 到 `target` 要往上几层。
///
/// 不做符号链接解析（调用方已经 `canonicalize` 过），也不检查存在性 ——
/// 它只负责把"片段自己的目录"翻译成"相对根目录"。
fn relative_path(root: &Path, target: &Path) -> PathBuf {
    use std::path::Component;
    let r: Vec<Component> = root.components().collect();
    let t: Vec<Component> = target.components().collect();
    let mut i = 0;
    while i < r.len() && i < t.len() && r[i] == t[i] {
        i += 1;
    }
    let mut out = PathBuf::new();
    for _ in i..r.len() {
        out.push("..");
    }
    for c in &t[i..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
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
            DocError::Invalid(vec![Diagnostic::error_code(
                ErrorCode::IoError,
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
        doc.source_path = Some(path_ref.to_path_buf());
        Ok(doc)
    }

    /// 序列化为美观 JSON。
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 递归解析 `include`，返回**合并后**的文档与解析期诊断。
    ///
    /// 这是 include 语义的**唯一实现**（`validate()`、CLI 装载、`runner::build_world`
    /// 与编辑器导出全都走它）：不允许任何调用点自己再写一套"找文件/合并"的规则，
    /// 否则就会出现"校验一套、渲染一套"的偏差。
    ///
    /// **没有 `include` 时是纯粹的克隆**：不读盘、不重定基任何路径，所以老场景的
    /// 校验结果与运行结果逐字节不变。
    pub fn resolved(&self) -> ResolvedDoc {
        if self.include.is_empty() {
            return ResolvedDoc {
                doc: Some(self.clone()),
                diagnostics: Vec::new(),
            };
        }
        let mut ctx = IncludeCtx {
            diags: Vec::new(),
            seen: Vec::new(),
            stack: Vec::new(),
            origin: BTreeMap::new(),
            root_dir: self.base_dir.as_deref().and_then(canonical_dir),
        };
        let root_label = self
            .source_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<根场景>".to_string());
        // 根文件自己也要进"已包含/栈"：`a.json → b.json → a.json` 才不会退化成"把 a 再并一遍"。
        if let Some(p) = &self.source_path {
            if let Ok(c) = std::fs::canonicalize(p) {
                ctx.seen.push(c.clone());
                ctx.stack.push(c);
            }
        }
        let mut merged = SceneDoc::default();
        // 顺序：**本文件自己的内容在前**，被 include 的内容在后（每个文件都按这条规则，
        // 于是整体是"先序深度优先"）。这样重名冲突永远报在"后并进来的那一份"上 ——
        // 也就是片段上（`include[i].materials.x`），而不是让人以为是根文件写错了。
        ctx.merge_own(&mut merged, self, "$", &root_label);
        for (i, rel) in self.include.iter().enumerate() {
            let ref_path = format!("include[{i}]");
            if let Some(frag) = ctx.load(self, rel, &ref_path) {
                let label = frag
                    .source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| rel.clone());
                ctx.merge_own(&mut merged, &frag, &ref_path, &label);
            }
        }
        if ctx.has_errors() {
            return ResolvedDoc {
                doc: None,
                diagnostics: ctx.diags,
            };
        }
        // 根独有的段只可能来自根文件（片段写了会被判错），所以这里按根的值回填。
        merged.format = self.format.clone();
        merged.version = self.version;
        merged.name = self.name.clone();
        merged.gravity = self.gravity;
        merged.physics = self.physics.clone();
        merged.ground = self.ground.clone();
        merged.assets = self.assets.clone();
        merged.camera = self.camera.clone();
        merged.lighting = self.lighting.clone();
        merged.simulate = self.simulate.clone();
        merged.render = self.render.clone();
        merged.water = self.water.clone();
        merged.base_dir = self.base_dir.clone();
        merged.source_path = self.source_path.clone();
        merged.extra_asset_paths = self.extra_asset_paths.clone();
        // `merge_own` **不搬运** `include`（每个文件的 include 各自由它自己的 `load`
        // 递归处理完），所以合并结果必然是自包含的。这不是"顺手清一下"，而是一份契约：
        // 结果一旦还带着 `include`，`build_world` / 编辑器存盘就会把同一批内容再并一遍
        // （静默的数据繁殖）。可观察的那一条由
        // `include_merges_fragments_in_order_and_rebases_fragment_asset_paths` 断言，
        // 这里再补一道 debug 断言，把责任明确指到 `merge_own`。
        debug_assert!(
            merged.include.is_empty(),
            "merge_own 不该搬运 include：合并结果必须是自包含的"
        );
        ResolvedDoc {
            doc: Some(merged),
            diagnostics: ctx.diags,
        }
    }

    /// 把场景内的资源相对路径展开成候选路径，按解析优先级排列。
    ///
    /// 顺序：绝对路径原样 → `场景文件所在目录/rel` → `assets.search_paths` 展开 →
    /// `--asset-path` 展开 → `工作目录/rel` → 引擎的标准搜索路径（可执行文件旁、
    /// `QUENCH3D_ASSET_PATH` 等，见 `quench3d::assets::resolve_path`）。去重后返回。
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
        // `--asset-path`（CLI）：排在场景自己声明的搜索路径之后，理由见字段注释。
        // 进程级那份与字段那份都算上（去重由 push 负责），这样"命令内部又 from_file
        // 了一次"的路径也不会漏掉旗标。
        for sp in self.extra_asset_paths.iter().chain(
            CLI_ASSET_PATHS
                .get()
                .map(|v| v.iter())
                .into_iter()
                .flatten(),
        ) {
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

    /// 语义校验（**含** `include` 解析）；返回全部诊断（含警告），调用方自行判断是否有 `Error`。
    ///
    /// 有 `include` 时校验的是**合并之后**的文档：诊断路径是合并后的根路径
    /// （`objects[7]` 可能是片段里的物体），而"文件缺失/成环/重名"这类片段级问题
    /// 直接挂在 `include[i]` 上并带上文件名。
    pub fn validate(&self) -> Vec<Diagnostic> {
        let resolved = self.resolved();
        let mut out = resolved.diagnostics;
        if let Some(doc) = resolved.doc {
            out.extend(doc.validate_resolved());
        }
        out
    }

    /// 语义校验的**主体**：只校验这份文档本身，不解析 `include`、不读盘。
    ///
    /// 已经 `resolved()` 过的文档应当直接调它，免得为同一批文件再读一遍盘。
    pub fn validate_resolved(&self) -> Vec<Diagnostic> {
        // 命名相机（`cameras`）：格式与校验先落地；`--camera <名字>` 的选择留到下一步。
        // 名字只在两处会出问题：空名（无从引用）与保留名 `default`（它是 `camera` 段的别名）。
        let mut cam_diags = Vec::new();
        for name in self.cameras.keys() {
            if name.trim().is_empty() {
                cam_diags.push(
                    Diagnostic::error_code(ErrorCode::InvalidValue, "cameras", "相机名字不能为空")
                        .with_hint(
                            "空名字没法被 `--camera` 引用；给它起个名字，或直接写 `camera` 段",
                        ),
                );
            } else if name == "default" {
                cam_diags.push(
                    Diagnostic::error_code(
                        ErrorCode::ReservedName,
                        "cameras",
                        "`default` 是保留名（指 `camera` 段这台）",
                    )
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
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        "render.outputs",
                        "输出名字不能为空（要么不写，要么给个名字）",
                    ));
                }
                if name.contains('/') || name.contains('\\') || name.contains("..") {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        "render.outputs",
                        "输出名字不能含路径分隔符或 `..`（它就是文件名）",
                    ));
                }
                if seen.contains(&name.as_str()) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::DuplicateName,
                        "render.outputs",
                        "输出名字重复了（会互相覆盖）",
                    ));
                }
                seen.push(name.as_str());
            }
            if let Some(cam) = &o.camera {
                if cam != "default" && !self.cameras.contains_key(cam) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::UnknownReference,
                        "render.outputs",
                        "这一项指定的相机不存在（见 `cameras` 段，或用 `default` 指 `camera` 段）",
                    ));
                }
            }
            for v in [o.width, o.height].into_iter().flatten() {
                if v == 0 {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "render.outputs",
                        "宽/高不能为 0",
                    ));
                }
            }
            let _ = i;
        }
        // 水域：尺寸/密度/阻尼非法会让"浮力静默不生效"，必须当场报。
        // `water` 是画面风格，不影响物理；非法值必须在 validate 就挡下，
        // 否则 post uniform 里会出现 NaN/负强度，水面整块变黑或闪烁。
        if let Some(style) = &self.water {
            if let Some(c) = style.color {
                if !c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) {
                    cam_diags.push(
                        Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            "water.color",
                            "水色必须是 0~1 的有限线性 RGB",
                        )
                        .with_hint("例如 `[0.06, 0.28, 0.48]`"),
                    );
                }
            }
            for (key, value, max) in [
                ("clarity", style.clarity, 1.0),
                ("wave_strength", style.wave_strength, 4.0),
                ("foam", style.foam, 2.0),
                ("refraction", style.refraction, 2.0),
            ] {
                if let Some(v) = value {
                    if !v.is_finite() || v < 0.0 || v > max {
                        cam_diags.push(
                            Diagnostic::error_code(
                                ErrorCode::OutOfRange,
                                format!("water.{key}"),
                                format!("必须在 0~{max} 之间（有限数）"),
                            )
                            .with_hint("0 表示关闭这一项"),
                        );
                    }
                }
            }
        }
        for (i, w) in self.waters.iter().enumerate() {
            let p = format!("waters[{i}]");
            if let Some(c) = &w.center {
                if !c.iter().all(|v| v.is_finite()) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.center"),
                        "水域中心必须是有限数",
                    ));
                }
            }
            if !w.size.iter().all(|v| v.is_finite() && *v > 0.0) {
                cam_diags.push(
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.size"),
                        "水域 XZ 尺寸必须是有限正数",
                    )
                    .with_hint("例如 `\"size\": [10, 6]`；只写 0 或负数等于没有水"),
                );
            }
            if !w.level.is_finite() {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.level"),
                    "水面高度必须是有限数",
                ));
            }
            if !(w.density.is_finite() && w.density > 0.0) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.density"),
                    "密度必须是有限正数（水 = 1000）",
                ));
            }
            if !(w.drag.is_finite() && w.drag >= 0.0) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.drag"),
                    "阻尼必须是有限的非负数",
                ));
            }
        }
        // 粒子流体：参数非法会让"加不进去/晶格爆炸"，必须当场报；粒子上限截断只警告。
        for (i, f) in self.fluids.iter().enumerate() {
            let p = format!("fluids[{i}]");
            if let Some(c) = &f.center {
                if !c.iter().all(|v| v.is_finite()) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.center"),
                        "流体中心必须是有限数",
                    ));
                }
            }
            if !f.size.iter().all(|v| v.is_finite() && *v > 0.0) {
                cam_diags.push(
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.size"),
                        "流体发射体尺寸必须是有限正数",
                    )
                    .with_hint("例如 `\"size\": [2, 2, 2]`；只写 0 或负数等于没有粒子"),
                );
            }
            if !(f.spacing.is_finite() && f.spacing > 0.0) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.spacing"),
                    "粒子间距必须是有限正数",
                ));
            } else if f.spacing > f.size.iter().copied().fold(f32::INFINITY, f32::min) * 2.0 {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.spacing"),
                    "粒子间距不能大于发射体最小边长（否则一颗粒子都放不下）",
                ));
            }
            let r = f.radius();
            if !(r.is_finite() && r > 0.0 && r <= f.spacing) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.radius"),
                    "粒子半径必须是有限正数且不大于 spacing",
                ));
            }
            if !(f.rest_density.is_finite() && f.rest_density > 0.0) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.rest_density"),
                    "静止密度必须是有限正数（水 = 1000）",
                ));
            }
            if !(f.viscosity.is_finite() && (0.0..=1.0).contains(&f.viscosity)) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.viscosity"),
                    "粘性系数必须在 0~1 之间",
                ));
            }
            if !(f.drag.is_finite() && f.drag >= 0.0) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.drag"),
                    "浸没阻尼必须是有限的非负数",
                ));
            }
            if !(1..=8).contains(&f.iterations) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.iterations"),
                    "密度约束迭代次数必须在 1~8 之间",
                ));
            }
            if !(1..=20_000).contains(&f.max_particles) {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.max_particles"),
                    "粒子上限必须在 1~20000 之间",
                ));
            } else {
                let raw = f.raw_particles();
                if raw > f.max_particles {
                    cam_diags.push(
                        Diagnostic::warn(
                            format!("{p}.max_particles"),
                            format!(
                                "按 spacing 估算有 {raw} 颗粒子，会被截断到 {} 颗（调大 max_particles 或调大 spacing）",
                                f.max_particles
                            ),
                        ),
                    );
                }
            }
        }
        // 关键帧动画：轨道本身要自洽（长度一致、时间严格递增、>= 0、数值有限），
        // 目标名与"至少一条轨道"也要当场拦住——这些都是"跑起来才发现"的错。
        for a in &self.animations {
            let label = a.name.clone().unwrap_or_else(|| a.target.clone());
            if a.target.trim().is_empty() {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    "animations",
                    "动画的 target 不能为空",
                ));
            }
            if a.position.is_none() && a.rotation.is_none() {
                cam_diags.push(Diagnostic::error_code(
                    ErrorCode::MissingField,
                    "animations",
                    format!(
                        "动画 `{label}` 至少要有一条 position 或 rotation 轨道（否则它什么都不做）"
                    ),
                ));
            }
            if let Some(w) = a.weight {
                if !w.is_finite() || !(0.0..=1.0).contains(&w) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "animations",
                        "weight 必须在 0~1 之间（0 = 不生效，1 = 完全生效）",
                    ));
                }
            }
            if let Some(sp) = a.speed {
                if !(sp.is_finite() && sp > 0.0) {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "animations",
                        "speed 必须是正的有限值",
                    ));
                }
            }
            if let Some(st) = a.start {
                if !st.is_finite() {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "animations",
                        "start 必须是有限值",
                    ));
                }
            }
            for (what, tr) in [("position", &a.position), ("rotation", &a.rotation)] {
                let Some(tr) = tr else { continue };
                if tr.times.len() != tr.values.len() {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        "animations",
                        format!("{what} 轨道的关键帧 times 与 values 长度必须一致"),
                    ));
                }
                if tr.times.is_empty() {
                    cam_diags.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        "animations",
                        "关键帧不能是空的",
                    ));
                }
                let mut prev = f32::NEG_INFINITY;
                for (k, t) in tr.times.iter().enumerate() {
                    if !t.is_finite() || *t < 0.0 {
                        cam_diags.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            "animations",
                            format!("{what} 轨道的关键帧时间必须是非负有限值"),
                        ));
                    }
                    if *t <= prev {
                        cam_diags.push(Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            "animations",
                            format!(
                                "{what} 轨道的关键帧时间必须严格递增（同一时刻只能有一个取值）"
                            ),
                        ));
                    }
                    prev = *t;
                    if let Some(v) = tr.values.get(k) {
                        if v.iter().any(|c| !c.is_finite()) {
                            cam_diags.push(Diagnostic::error_code(
                                ErrorCode::OutOfRange,
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
                            cam_diags.push(Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
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
            out.extend(self.expanded().validate_resolved());
            return out;
        }
        let mut out = Vec::new();

        // --- 规模上限：宁可给一条明确诊断，也不要跑到内存耗尽 / 卡死 ---
        if self.objects.len() > MAX_OBJECTS {
            out.push(
                Diagnostic::error_code(ErrorCode::LimitExceeded,
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
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "physics.static_friction_speed",
                    "静/动摩擦的切换速度必须是非负的有限数",
                ));
            } else if v > 1.0 {
                out.push(
                    Diagnostic::warn_code(ErrorCode::SuspiciousValue,
                        "physics.static_friction_speed",
                        format!("阈值 {v} m/s 偏大：慢速滑行会被当成静止接触（表现为「滑到一半突然很黏」）"),
                    )
                    .with_hint("多数场景 0.01~0.1 就够"),
                );
            }
        }

        if let Some(v) = self.physics.speculative_margin {
            if !v.is_finite() || v < 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "physics.speculative_margin",
                    "接触余量必须是非负的有限数（米）",
                ));
            } else if v > 1.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "physics.speculative_margin",
                    format!("接触余量 {v} 米过大：它只是「提前一点点算接触」，正常是厘米量级"),
                ));
            } else if v > 0.25 {
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
                        "physics.speculative_margin",
                        format!("接触余量 {v} 米偏大：物体之间会「还没碰到就被推开」"),
                    )
                    .with_hint("多数场景 0.005~0.02 就够"),
                );
            }
        }

        if self.simulate.steps > MAX_STEPS {
            out.push(
                Diagnostic::error_code(
                    ErrorCode::LimitExceeded,
                    "simulate.steps",
                    format!("步数 {} 超过上限 {MAX_STEPS}", self.simulate.steps),
                )
                .with_hint("分多次 simulate 推进，或减小步数"),
            );
        }
        if !(self.simulate.dt.is_finite() && self.simulate.dt > 0.0) {
            out.push(Diagnostic::error_code(
                ErrorCode::OutOfRange,
                "simulate.dt",
                format!("时间步长必须是有限正数，当前为 {}", self.simulate.dt),
            ));
        }

        if self.format != FORMAT {
            out.push(
                Diagnostic::error_code(
                    ErrorCode::InvalidValue,
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
                Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    "version",
                    format!("不支持的版本 {}，当前支持 {VERSION}", self.version),
                )
                .with_hint(hint),
            );
        }
        if let Some(g) = &self.gravity {
            if !finite3(g) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "gravity",
                    "重力必须是有限数",
                ));
            }
        }

        // 材质
        for (name, m) in &self.materials {
            let p = format!("materials.{name}");
            if !finite3(&m.color) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.color"),
                    "颜色必须是有限数",
                ));
            }
            if m.color.iter().any(|c| *c < 0.0 || *c > 1.0) {
                out.push(Diagnostic::warn_code(
                    ErrorCode::SuspiciousValue,
                    format!("{p}.color"),
                    "颜色分量应在 0~1 之间",
                ));
            }
            if !(0.0..=1.0).contains(&m.metallic) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.metallic"),
                    "金属度应在 0~1 之间",
                ));
            }
            if !(0.05..=1.0).contains(&m.roughness) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.roughness"),
                    "粗糙度应在 0.05~1 之间",
                ));
            }
            if !m.emissive.is_finite() || m.emissive < 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.emissive"),
                    "自发光强度必须是非负有限数",
                ));
            }
            if let Some(a) = m.alpha {
                if !a.is_finite() || !(0.0..=1.0).contains(&a) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.alpha"),
                        format!("不透明度应在 0~1 之间，当前为 {a}"),
                    ));
                }
            }
            if let Some(c) = &m.emissive_color {
                if !finite3(c) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.emissive_color"),
                        "自发光颜色必须是有限数",
                    ));
                }
            }
            if let Some(u) = m.uv_scale {
                if !u.is_finite() || u <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.uv_scale"),
                        format!("UV 缩放必须是正有限数，当前为 {u}"),
                    ));
                }
            }
            if let Some(n) = m.normal_scale {
                if !n.is_finite() {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.normal_scale"),
                        "法线强度必须是有限数",
                    ));
                }
            }
            if let Some(cut) = m.alpha_cutoff {
                if !cut.is_finite() || cut < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.alpha_cutoff"),
                        format!("alpha 裁剪阈值必须是非负有限数，当前为 {cut}"),
                    ));
                } else if cut > 0.0 && m.alpha_blend == Some(true) {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::MutuallyExclusive,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        shown,
                        "贴图路径不能为空",
                    ));
                    continue;
                }
                let lower = path.to_ascii_lowercase();
                if !(lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg"))
                {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::InvalidValue,
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
                        Diagnostic::error_code(
                            ErrorCode::AssetNotFound,
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
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.position"),
                    "点光源位置必须是有限数",
                ));
            }
            if !finite3(&l.color) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.color"),
                    "点光源颜色必须是有限数",
                ));
            }
            if !l.intensity.is_finite() || l.intensity < 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.intensity"),
                    "点光源强度必须是非负有限数",
                ));
            }
            if !l.radius.is_finite() || l.radius <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.radius"),
                    "点光源半径必须是正有限数",
                ));
            }
        }
        // 聚光灯：角度范围、内锥 < 外锥、半径/强度、方向非零
        let spots = self.lighting.spot_lights.as_deref().unwrap_or(&[]);
        for (i, l) in spots.iter().enumerate() {
            let p = format!("lighting.spot_lights[{i}]");
            if l.radius <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.radius"),
                    "影响半径必须为正",
                ));
            }
            if l.intensity < 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.intensity"),
                    "强度不能为负",
                ));
            }
            let in_range = |d: f32| d > 0.0 && d < 90.0;
            if !in_range(l.inner_deg) || !in_range(l.outer_deg) {
                out.push(
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.inner_deg"),
                        format!(
                            "锥角必须落在 (0, 90) 度之间（现在内 {} 度、外 {} 度）",
                            l.inner_deg, l.outer_deg
                        ),
                    )
                    .with_hint("内锥是全强度区、外锥是光斑边界；90 度就已经是半球了"),
                );
            } else if l.inner_deg > l.outer_deg {
                out.push(
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.inner_deg"),
                        format!("内锥角 {} 度不能大于外锥角 {} 度", l.inner_deg, l.outer_deg),
                    )
                    .with_hint("把内锥角改小（光斑中心更实），或把外锥角改大（光斑边缘更散）"),
                );
            }
            let d = l.direction;
            if (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 1e-6 {
                out.push(
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.direction"),
                        "照射方向不能是零向量",
                    )
                    .with_hint("例如 [0, -1, 0] 表示向下照"),
                );
            }
        }
        // 点光源与聚光灯共享名额：说清楚"谁会被丢掉"，否则画面少一盏灯很难查
        let local_total = point_lights.len() + spots.len();
        if local_total > MAX_POINT_LIGHTS {
            out.push(
                Diagnostic::warn_code(ErrorCode::LimitExceeded,
                    "lighting.spot_lights",
                    format!(
                        "点光源 + 聚光灯共 {local_total} 盏，超过 {MAX_POINT_LIGHTS} 盏的部分不参与着色（点光源优先）"
                    ),
                )
                .with_hint("减少局部光数量，或把远处的灯合并成一盏"),
            );
        }
        if point_lights.len() > MAX_POINT_LIGHTS {
            out.push(
                Diagnostic::warn_code(
                    ErrorCode::LimitExceeded,
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
        // 点光源阴影：整帧只支持一盏（6 个面共用一张深度数组）。
        // 与聚光灯那条同形状，只是路径/名额不同——点光源**优先**占名额，所以
        // "被挤掉"只可能发生在"它自己排在第 9 盏以上"这一种情况。
        let point_casters: Vec<usize> = point_lights
            .iter()
            .enumerate()
            .filter(|(_, l)| l.cast_shadows == Some(true))
            .map(|(i, _)| i)
            .collect();
        if point_casters.len() > 1 {
            out.push(
                Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("lighting.point_lights[{}].cast_shadows", point_casters[0]),
                    format!(
                        "有 {} 盏点光源要求投影，但引擎只支持一盏：实际投影的是 point_lights[{}]（第一盏要求投影的），其余照常照明但不投影",
                        point_casters.len(),
                        point_casters[0]
                    ),
                )
                .with_hint("把其余点光源的 cast_shadows 改成 false，或把它们合并成一盏"),
            );
        }
        if let Some(&first) = point_casters.first() {
            if first >= MAX_POINT_LIGHTS {
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("lighting.point_lights[{first}].cast_shadows"),
                        format!(
                            "这一盏排在第 {} 个局部光名额（一共只有 {MAX_POINT_LIGHTS} 个），它既不会照明也不会投影",
                            first + 1
                        ),
                    )
                    .with_hint("减少点光源数量，或把这一盏挪到 point_lights 前面"),
                );
            }
        }
        // 聚光阴影：整帧只支持一盏（一张 2D 阴影图只能对应一个视锥）。
        // 说清"实际会用哪一盏"——否则用户勾了三盏、只亮一盏，只能靠猜。
        let casters: Vec<usize> = spots
            .iter()
            .enumerate()
            .filter(|(_, l)| l.cast_shadows == Some(true))
            .map(|(i, _)| i)
            .collect();
        if casters.len() > 1 {
            out.push(
                Diagnostic::warn_code(ErrorCode::Ineffective,
                    format!("lighting.spot_lights[{}].cast_shadows", casters[0]),
                    format!(
                        "有 {} 盏聚光灯要求投影，但引擎只支持一盏：实际投影的是 spot_lights[{}]（第一盏要求投影的），其余照常照明但不投影",
                        casters.len(),
                        casters[0]
                    ),
                )
                .with_hint("把其余聚光灯的 cast_shadows 改成 false，或把它们合并成一盏"),
            );
        }
        // 投影灯被局部光名额挤掉时，它既不照明也不投影——这条比"超过 8 盏"那条更具体
        if let Some(&first) = casters.first() {
            let packed = point_lights.len().min(MAX_POINT_LIGHTS) + first;
            if packed >= MAX_POINT_LIGHTS {
                out.push(
                    Diagnostic::warn_code(ErrorCode::Ineffective,
                        format!("lighting.spot_lights[{first}].cast_shadows"),
                        format!(
                            "这一盏排在第 {} 个局部光名额（点光源优先，一共只有 {MAX_POINT_LIGHTS} 个），它既不会照明也不会投影",
                            packed + 1
                        ),
                    )
                    .with_hint("减少点光源数量，或把这盏聚光灯挪到 spot_lights 前面"),
                );
            }
        }

        // 雾：颜色有限且非负、密度非负有限
        if let Some(fog) = &self.lighting.fog {
            if let Some(c) = fog.color {
                if !finite3(&c) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.fog.color",
                        "雾颜色必须是有限数",
                    ));
                } else if c.iter().any(|v| *v < 0.0) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.fog.color",
                        "雾颜色不能是负数（线性空间的辐射值没有负的）",
                    ));
                } else if c.iter().any(|v| *v > 4.0) {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
                            "lighting.fog.color",
                            format!("雾颜色 {c:?} 明显超过 1（线性空间），画面会很怪"),
                        )
                        .with_hint("雾颜色是线性值不是 sRGB：常见区间 0~1"),
                    );
                }
            }
            match fog.density {
                Some(d) if !d.is_finite() || d < 0.0 => out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "lighting.fog.density",
                    "雾密度必须是非负有限数",
                )),
                Some(d) if d > 1.0 => out.push(
                    Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
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
                    Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.shadow_quality.strength",
                        "阴影强度必须在 0~1 之间",
                    )
                    .with_hint("0 = 阴影不压暗（等于没有），1 = 全黑；真实场景 0.6~0.9 更自然"),
                ),
                _ => {}
            }
            match q.radius {
                Some(v) if !v.is_finite() || v <= 0.0 => out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "lighting.shadow_quality.radius",
                    "阴影覆盖半径必须是正有限数（米）",
                )),
                Some(v) if v > 400.0 => out.push(
                    Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
                        "lighting.shadow_quality.radius",
                        format!("阴影覆盖半径 {v} 米太大：分辨率摊得太薄，阴影会糊成一片"),
                    )
                    .with_hint("引擎会在 2~4000 米之间钳制；常规模拟给 10~60"),
                ),
                _ => {}
            }
            if let Some(n) = q.cascades {
                if !(1..=4).contains(&n) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.shadow_quality.cascades",
                        "cascades 必须在 1~4 之间（1 = 单级；4 是 2x2 图集的上限）",
                    ));
                }
            }
            if let Some(l) = q.split_lambda {
                if !l.is_finite() || !(0.0..=1.0).contains(&l) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("lighting.sky_colors.{name}"),
                        "天空颜色必须是有限数",
                    ));
                } else if c.iter().any(|v| *v < 0.0) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("lighting.sky_colors.{name}"),
                        "天空颜色不能是负数",
                    ));
                }
            }
        }

        if let Some(g) = &self.ground {
            if !g.size.is_finite() || g.size <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "ground.size",
                    "地面边长必须为正数",
                ));
            }
            if !g.y.is_finite() {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "ground.y",
                    "地面高度必须是有限数",
                ));
            }
            if !g.friction.is_finite() || g.friction < 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "ground.friction",
                    "地面摩擦系数必须是非负数",
                ));
            }
            if !(0.0..=1.0).contains(&g.restitution) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "ground.restitution",
                    "地面恢复系数应在 0~1 之间",
                ));
            }
            if let Some(m) = &g.material {
                if !self.materials.contains_key(m) {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            "ground.material",
                            format!("未定义的材质 \"{m}\""),
                        )
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
                    Diagnostic::error_code(
                        ErrorCode::MutuallyExclusive,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            format!("{p}.shapes"),
                            "shapes 不能是空数组（至少一个部件）",
                        ));
                    }
                    for (i, part) in parts.iter().enumerate() {
                        let pp = format!("{p}.shapes[{i}]");
                        validate_shape(&part.shape, &format!("{pp}.shape"), &mut out);
                        if let Some(v) = &part.position {
                            if !finite3(v) {
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{pp}.position"),
                                    "部件位置必须是有限数",
                                ));
                            }
                        }
                        if let Some(v) = &part.rotation_deg {
                            if !finite3(v) {
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{pp}.rotation_deg"),
                                    "部件欧拉角必须是有限数",
                                ));
                            }
                        }
                        if part.shape.kind == ShapeKind::Plane {
                            out.push(Diagnostic::warn_code(ErrorCode::SuspiciousValue,
                                &pp,
                                "无限半空间当复合形状的一个部件：整个刚体都会被它撑成无限大（通常只想当整个物体用）",
                            ));
                        }
                    }
                    if parts.len() == 1 {
                        out.push(Diagnostic::warn_code(
                            ErrorCode::Ineffective,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.position"),
                        "位置必须是有限数",
                    ));
                }
            }
            if let Some(r) = &o.rotation_deg {
                if !finite3(r) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.rotation_deg"),
                        "旋转必须是有限数",
                    ));
                }
            }
            if let Some(s) = &o.scale {
                if !finite3(s) || s.iter().any(|v| *v <= 0.0) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.scale"),
                        "缩放必须是正有限数",
                    ));
                }
            }
            if let Some(m) = &o.material {
                if !self.materials.contains_key(m) {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            format!("{p}.material"),
                            format!("未定义的材质 \"{m}\""),
                        )
                        .with_hint("在 materials 里补上，或删掉该字段"),
                    );
                }
            }
            if let Some(d) = o.density {
                if !d.is_finite() || d <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.density"),
                        "密度必须为正数",
                    ));
                }
            }
            if let Some(e) = o.restitution {
                if !(0.0..=1.0).contains(&e) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.restitution"),
                        "恢复系数应在 0~1 之间",
                    ));
                }
            }
            if let Some(f) = o.friction {
                if !f.is_finite() || f < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.friction"),
                        "摩擦系数必须是非负数",
                    ));
                }
            }
            if let Some(sf) = o.static_friction {
                if !sf.is_finite() || sf < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.static_friction"),
                        "静摩擦系数必须是非负数",
                    ));
                } else if let Some(df) = o.friction {
                    if df.is_finite() && sf < df - 1e-6 {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
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
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    &p,
                    "这个物体没有刚体（body = none），friction_combine 不会生效",
                ));
            }
            if o.restitution_combine.is_some() && o.body == BodyKind::None {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    &p,
                    "这个物体没有刚体（body = none），restitution_combine 不会生效",
                ));
            }
            if let Some(v) = &o.velocity {
                if !finite3(v) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.velocity"),
                        "速度必须是有限数",
                    ));
                }
            }
            if let Some(v) = &o.angular_velocity {
                if !finite3(v) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.{key}"),
                            format!("{key} 必须是非负数"),
                        ));
                    }
                }
            }
            if let Some(g) = o.gravity_scale {
                if !g.is_finite() {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.gravity_scale"),
                        "重力缩放必须是有限数",
                    ));
                }
            }
            if let Some(m) = o.mass {
                if !m.is_finite() || m <= 0.0 {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.mass"),
                            "质量必须为正数",
                        )
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
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
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
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        format!("{p}.body"),
                        "平面不能是动态刚体（无限大物体没有有限质量）",
                    )
                    .with_hint("把 body 改成 \"static\""),
                );
            }
            let has_heightfield = o
                .parts()
                .iter()
                .any(|part| part.shape.kind == ShapeKind::HeightField);
            if has_heightfield && matches!(o.body, BodyKind::Dynamic | BodyKind::Kinematic) {
                out.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        format!("{p}.body"),
                        "高度场是静态三角网，只能是 static 刚体（它没有质量属性，硬写成动态会\
                         被引擎静默降级成静态）",
                    )
                    .with_hint("把 body 改成 \"static\"；要会动的地形请用 box 拼或等动态网格支持"),
                );
            }
            if o.body == BodyKind::None && o.velocity.is_some() {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.velocity"),
                    "物体没有刚体，速度不会生效",
                ));
            }
            // 碰撞过滤：层号只有 0~31（掩码是 32 位）
            if let Some(layer) = o.collision_layer {
                if layer > 31 {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::OutOfRange,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
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
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.collides_with"),
                        "层号有重复，重复项没有额外作用",
                    ));
                }
                if mask.is_empty() {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
                            format!("{p}.collides_with"),
                            "空列表 = 谁也不碰：这个物体会穿过所有东西",
                        )
                        .with_hint("想让它和所有层碰就删掉本字段"),
                    );
                }
                if o.body == BodyKind::None {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.collides_with"),
                        "物体没有刚体，碰撞过滤不会生效",
                    ));
                }
                // 只和自己那一层碰是常见写法；但若连自己那层都不含，容易看漏
                if let Some(layer) = o.collision_layer {
                    if !mask.contains(&layer) {
                        out.push(Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
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
                        Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            format!("{p}.sensor"),
                            "传感器需要刚体（body 是 \"none\" 的物体不参与物理）",
                        )
                        .with_hint("写成 \"static\"（固定触发区）或 \"dynamic\"（跟着动的触发区）"),
                    );
                }
                if o.density.is_some() || o.mass.is_some() {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        &p,
                        "传感器不产生冲量，密度/质量对它没有意义",
                    ));
                }
                if o.restitution.is_some() || o.friction.is_some() {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        &p,
                        "传感器不产生冲量，恢复系数/摩擦对它没有意义",
                    ));
                }
                if has_plane {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
                        format!("{p}.sensor"),
                        "无限半空间当传感器：整个空间永远在里面，事件只在开始时报一次",
                    ));
                }
            }
            // CCD：只对动态刚体有意义
            if o.ccd == Some(true) {
                if o.body != BodyKind::Dynamic {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.ccd"),
                        format!("body 是 {:?}，CCD 只对 dynamic 生效，会被忽略", o.body),
                    ));
                }
                if has_plane {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.ccd"),
                        "无限半空间不需要 CCD（它不会动）",
                    ));
                }
            }
            // scale 是渲染专用的：碰撞形状取 shape 里的原始尺寸，这里放大会静默对不上
            if o.scale.is_some() && o.body != BodyKind::None {
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
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
                        Diagnostic::error_code(
                            ErrorCode::DuplicateName,
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
                out.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    p,
                    "父物体名不能为空",
                ));
                continue;
            }
            if o.name.as_deref() == Some(parent) {
                out.push(
                    Diagnostic::error_code(ErrorCode::Cycle, &p, "物体不能以自己为父")
                        .with_hint("删掉 parent 字段，或用另一个物体的名字"),
                );
                continue;
            }
            if !seen_names.contains_key(parent) {
                out.push(
                    Diagnostic::error_code(ErrorCode::UnknownReference,
                        &p,
                        format!("找不到名为 \"{parent}\" 的父物体（父物体必须写在 objects 里并显式命名）"),
                    )
                    .with_hint("给父物体补一个 name，或把 parent 改成它的名字"),
                );
                continue;
            }
            if o.body != BodyKind::None {
                out.push(
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
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
                                Diagnostic::error_code(
                                    ErrorCode::Cycle,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        p,
                        "搜索路径不能为空",
                    ));
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
                        Diagnostic::warn_code(
                            ErrorCode::AssetNotFound,
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
                out.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    format!("{p}.path"),
                    "模型路径不能为空",
                ));
            } else {
                let ext = m
                    .path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if !matches!(ext.as_str(), "gltf" | "glb" | "obj") {
                    out.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
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
                            out.push(Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
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
                            Diagnostic::error_code(
                                ErrorCode::AssetNotFound,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.scale"),
                        "缩放必须是正数",
                    ));
                }
            }
            // 模型动画：结构上能查的只有"名字非空 / 倍速为正 / 起始非负"。
            // 名字在不在资产里、时长多少，`validate` 读不到（它不装载资产）——
            // 那一条由 `simulate` / `render` 报告的 `models[].animation` 给出。
            if let Some(a) = &m.animation {
                let ap = format!("{p}.animation");
                if a.name.trim().is_empty() {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            format!("{ap}.name"),
                            "动画名不能为空",
                        )
                        .with_hint(
                            "写 glTF 里的 `animations[].name`；\
                             用 `quench3d info 模型.glb` 能看到有哪些",
                        ),
                    );
                }
                if let Some(sp) = a.speed {
                    if !(sp.is_finite() && sp > 0.0) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{ap}.speed"),
                            "播放倍速必须是正的有限值",
                        ));
                    }
                }
                if let Some(st) = a.start {
                    if !st.is_finite() || st < 0.0 {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{ap}.start"),
                            "起始时间必须是非负有限值",
                        ));
                    }
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
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            format!("{p}.{field}"),
                            format!("找不到名为 \"{name}\" 的物体"),
                        )
                        .with_hint("关节两端必须是 objects 里带 name 的物体"),
                    ),
                    Some(o) => {
                        if o.body == BodyKind::None {
                            out.push(
                                Diagnostic::error_code(
                                    ErrorCode::InvalidValue,
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
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    &p,
                    "关节两端都是静态刚体，不会有任何运动",
                ));
            }
            if j.kind == JointKind::Distance && j.rest_length.is_some_and(|l| l < 0.0) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.rest_length"),
                    "静止长度不能为负",
                ));
            }
            if let Some(l) = &j.limits {
                if !l[0].is_finite() || !l[1].is_finite() {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.limits"),
                        "限位必须是有限数",
                    ));
                } else if l[0] > l[1] {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.limits"),
                        format!("下限 {} 大于上限 {}", l[0], l[1]),
                    ));
                }
            }
            // 零向量/非有限轴会被 `normalize_or_zero` 静默变成零轴，关节行为不可预期
            for (key, given) in [
                ("axis", &j.axis),
                ("axis_a", &j.axis_a),
                ("axis_b", &j.axis_b),
            ] {
                let Some(axis) = given else { continue };
                let bad = axis.iter().any(|v| !v.is_finite());
                let zero =
                    !bad && (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]) < 1e-12;
                if bad {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.{key}"),
                        "轴向必须是有限数",
                    ));
                } else if zero {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.{key}"),
                            "轴向不能是零向量",
                        )
                        .with_hint("铰链/滑块的轴必须是某个方向，例如 [0, 1, 0]"),
                    );
                }
            }
            // `axis_a`/`axis_b` 只有铰链与滑块读；写在别的关节上等于没写（静默忽略是最难查的坑）
            if !matches!(j.kind, JointKind::Hinge | JointKind::Slider) {
                for (key, given) in [
                    ("axis_a", j.axis_a.is_some()),
                    ("axis_b", j.axis_b.is_some()),
                ] {
                    if given {
                        let d = Diagnostic::warn_code(
                            ErrorCode::Ineffective,
                            format!("{p}.{key}"),
                            "只有铰链/滑块有转轴/滑轴，这个字段会被忽略",
                        );
                        out.push(if j.kind == JointKind::Ball {
                            d.with_hint(
                                "球窝的锥形限位轴请写进 cone_limit.axis_a / cone_limit.axis_b",
                            )
                        } else {
                            d
                        });
                    }
                }
            }
            // 同一个**局部**轴写在两端 ≠ 同一个世界轴：两端初始朝向不同时，求解器会先
            // 把 B 扭到与 A 对齐（"门装上去自己弹回原位"）。这正是 `axis_a`/`axis_b`
            // 存在的理由，所以这里必须吵一声，而不是等用户看画面猜。
            if matches!(j.kind, JointKind::Hinge | JointKind::Slider)
                && j.axis_a.is_none()
                && j.axis_b.is_none()
            {
                let axis = v3(j.axis.unwrap_or([0.0, 1.0, 0.0]));
                if axis.length_squared() > 1e-12 {
                    let world_rot = |name: &str| -> Quat {
                        // 关节两端必须有刚体，而"带刚体的物体不能挂 parent"（见 objects[].parent
                        // 的校验），所以这里的 `rotation_deg` 就是世界朝向，不用沿父链累乘。
                        self.objects
                            .iter()
                            .find(|o| o.name.as_deref() == Some(name))
                            .map(|o| {
                                let d = o.rotation_deg.unwrap_or([0.0, 0.0, 0.0]);
                                let r = d.map(f32::to_radians);
                                Quat::from_euler_xyz(r[0], r[1], r[2])
                            })
                            .unwrap_or(Quat::IDENTITY)
                    };
                    let wa = world_rot(&j.a).mul_vec3(axis).normalize_or_zero();
                    let wb = world_rot(&j.b).mul_vec3(axis).normalize_or_zero();
                    let deg = wa.dot(wb).clamp(-1.0, 1.0).acos().to_degrees();
                    if deg > 10.0 {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
                                format!("{p}.axis"),
                                format!(
                                    "两端物体朝向相差 {deg:.0}°：同一个局部轴在两端指向不同的世界方向，\
                                     铰链/滑块会先把 B 扭到与 A 对齐（门/摆锤装不上）"
                                ),
                            )
                            .with_hint(
                                "把世界轴按各端朝向拆开写：`axis_a`（A 的局部轴）与 `axis_b`（B 的局部轴）",
                            ),
                        );
                    }
                }
            }
            for (key, anchor) in [("anchor_a", &j.anchor_a), ("anchor_b", &j.anchor_b)] {
                if let Some(v) = anchor {
                    if !finite3(v) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.{key}"),
                            "锚点必须是有限数",
                        ));
                    }
                }
            }
            if j.kind != JointKind::Hinge && j.motor_velocity.is_some() {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.motor_velocity"),
                    "只有铰链关节支持马达",
                ));
            }
            // 参考姿态（`frame_a`/`frame_b`）：只有 fixed 读它，而且**生效判据与求解器
            // 完全同一份**（`crate::runner::joint_frame_quat` + `Joint::fixed_uses_reference_frames`）
            // —— 否则诊断和实跑会各说一套。
            for (key, given) in [("frame_a", &j.frame_a), ("frame_b", &j.frame_b)] {
                if let Some(v) = given {
                    if !finite3(v) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.{key}"),
                            "参考姿态必须是有限数（欧拉角，度）",
                        ));
                    }
                }
            }
            let frames_given = j.frame_a.is_some() || j.frame_b.is_some();
            if frames_given && j.kind != JointKind::Fixed {
                for (key, is_some) in [
                    ("frame_a", j.frame_a.is_some()),
                    ("frame_b", j.frame_b.is_some()),
                ] {
                    if is_some {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::Ineffective,
                                format!("{p}.{key}"),
                                "只有固定关节（type = fixed）有参考姿态，这个字段会被忽略",
                            )
                            .with_hint(
                                "铰链/滑块/球窝的方向请用 axis_a / axis_b（或球窝的 cone_limit）",
                            ),
                        );
                    }
                }
            } else if frames_given
                && crate::runner::joint_frame_quat(j.frame_a) == Quat::IDENTITY
                && crate::runner::joint_frame_quat(j.frame_b) == Quat::IDENTITY
            {
                // 两端参考姿态都是单位旋转（不写或全 0）：与完全不写**逐位等价**，
                // 求解器仍然锁定创建时的相对姿态。写了却不生效是最难查的坑。
                let key = if j.frame_a.is_some() {
                    "frame_a"
                } else {
                    "frame_b"
                };
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.{key}"),
                        "两端参考姿态都是 0（单位旋转）：固定关节仍然锁定创建时的相对姿态，\
                         这两个字段等于没写",
                    )
                    .with_hint(
                        "想让求解器把 B 扭到某个参考姿态，至少给一端一个非零角度；\
                         想要老行为就直接删掉 frame_a / frame_b",
                    ),
                );
            }
            // 单位陷阱：铰链的角度量都是弧度，而物体的 rotation_deg 是角度
            if j.kind == JointKind::Hinge {
                if let Some(l) = &j.limits {
                    if l.iter().any(|v| v.abs() > 6.5) {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::UnitAmbiguity,
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
                            Diagnostic::warn_code(
                                ErrorCode::UnitAmbiguity,
                                format!("{p}.motor_velocity"),
                                format!("马达速度单位是弧度/秒，当前 {v}"),
                            )
                            .with_hint("写的是角度/秒就除以 57.2958"),
                        );
                    }
                }
                if j.motor_target_deg.is_some() && j.motor_velocity.is_some() {
                    out.push(
                        Diagnostic::warn_code(ErrorCode::MutuallyExclusive,
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
                        out.push(Diagnostic::warn_code(
                            ErrorCode::Ineffective,
                            format!("{p}.{key}"),
                            "只有铰链关节支持马达",
                        ));
                    }
                }
            }
            if let Some(deg) = j.motor_target_deg {
                if !deg.is_finite() {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.motor_target_deg"),
                        "伺服目标角必须是有限数",
                    ));
                } else if deg.abs() > 3600.0 {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::UnitAmbiguity,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.cone_limit.half_angle_deg"),
                            "锥形限位的半角必须在 0~180 度之间",
                        ));
                    } else if cone.half_angle_deg < 0.5 {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
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
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{p}.cone_limit.{key}"),
                                    "锥形限位的轴必须是有限数",
                                ));
                            } else if zero {
                                out.push(Diagnostic::error_code(
                                    ErrorCode::OutOfRange,
                                    format!("{p}.cone_limit.{key}"),
                                    "锥形限位的轴不能是零向量",
                                ));
                            }
                        }
                    }
                }
                (Some(_), _) => out.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
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
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.{key}"),
                            format!("断裂阈值必须是正数（{unit}）；不想让它断就别写这个字段"),
                        ));
                    } else if v < 0.5 {
                        out.push(Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
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
                Diagnostic::error_code(
                    ErrorCode::LimitExceeded,
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
                    Diagnostic::error_code(
                        ErrorCode::MutuallyExclusive,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.at_time"),
                        format!("时间点必须是有限的非负数，当前为 {t}"),
                    ));
                }
            }
            if self.simulate.steps == 0 {
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        &p,
                        "simulate.steps = 0，一步都不跑，动作不会触发",
                    )
                    .with_hint("给 simulate.steps 一个正数（例如 120）"),
                );
            }
            if let Some(step) = eff_step {
                if self.simulate.steps > 0 && step > self.simulate.steps {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::Ineffective,
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
                    Diagnostic::error_code(
                        ErrorCode::MissingField,
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
                Some(v) if !finite3(v) => out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.value"),
                    "value 必须是有限数",
                )),
                Some(_) if !a.kind.needs_value() => out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.value"),
                    format!("{kind} 动作不需要 value，会被忽略"),
                )),
                _ => {}
            }
            if a.kind.uses_point() {
                match &a.point {
                    None => out.push(Diagnostic::error_code(
                        ErrorCode::MissingField,
                        format!("{p}.point"),
                        "impulse_at_point 需要 point（世界坐标的作用点）",
                    )),
                    Some(q) if !finite3(q) => out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.point"),
                        "作用点必须是有限数",
                    )),
                    Some(_) => {}
                }
            } else if a.point.is_some() {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.point"),
                    format!("只有 impulse_at_point 用 point，{kind} 会忽略它"),
                ));
            }
            if a.kind.uses_position() {
                match &a.position {
                    None => out.push(Diagnostic::error_code(
                        ErrorCode::MissingField,
                        format!("{p}.position"),
                        "teleport 需要 position（目标位置）",
                    )),
                    Some(v) if !finite3(v) => out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.position"),
                        "目标位置必须是有限数",
                    )),
                    Some(_) => {}
                }
                if a.rotation_deg.as_ref().is_some_and(|r| !finite3(r)) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{p}.rotation_deg"),
                        "目标朝向必须是有限数",
                    ));
                }
            } else {
                if a.position.is_some() {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.position"),
                        format!("只有 teleport 用 position，{kind} 会忽略它"),
                    ));
                }
                if a.rotation_deg.is_some() {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.rotation_deg"),
                        format!("只有 teleport 用 rotation_deg，{kind} 会忽略它"),
                    ));
                }
            }
            if a.kind.uses_steps() {
                if a.steps == 0 {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        format!("{p}.steps"),
                        "steps = 0，这个力一步都不会施加",
                    ));
                }
            } else if a.steps != 1 {
                out.push(Diagnostic::warn_code(
                    ErrorCode::Ineffective,
                    format!("{p}.steps"),
                    format!("steps 只对 force 生效，{kind} 会忽略它"),
                ));
            }

            // `target: "*"` 是 `wake` 专用的特殊值：唤醒全部刚体
            if a.target == "*" {
                if a.kind != ActionKind::Wake {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::InvalidValue,
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
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            format!("{p}.target"),
                            format!("找不到名为 \"{}\" 的刚体", a.target),
                        )
                        .with_hint(body_name_hint),
                    ),
                    Some(BodyKind::None) => out.push(
                        Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            format!("{p}.target"),
                            format!(
                                "物体 \"{}\" 没有刚体（body = none），动作不会生效",
                                a.target
                            ),
                        )
                        .with_hint("把它的 body 改成 dynamic"),
                    ),
                    Some(BodyKind::Static) if a.kind.pushes() => out.push(
                        Diagnostic::warn_code(
                            ErrorCode::Ineffective,
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
                Diagnostic::error_code(
                    ErrorCode::LimitExceeded,
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
                out.push(Diagnostic::error_code(
                    ErrorCode::InvalidValue,
                    format!("{p}.name"),
                    "探针名不能为空（报告里用它标识）",
                ));
            } else if !probe_names.insert(pr.name.as_str()) {
                out.push(Diagnostic::error_code(
                    ErrorCode::DuplicateName,
                    format!("{p}.name"),
                    format!("探针名 \"{}\" 重复，报告里无法区分", pr.name),
                ));
            }
            if !finite3(&pr.position) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    format!("{p}.position"),
                    "探针中心必须是有限数",
                ));
            }
            match pr.shape {
                ProbeShapeDoc::Sphere => {
                    if !(pr.radius.is_finite() && pr.radius > 0.0) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{p}.radius"),
                            format!("球形探针半径必须是有限正数，当前为 {}", pr.radius),
                        ));
                    }
                    if pr.size.is_some() {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::Ineffective,
                                format!("{p}.size"),
                                "球形探针不用 size，会被忽略",
                            )
                            .with_hint("要盒形区域就把 shape 写成 \"box\""),
                        );
                    }
                }
                ProbeShapeDoc::Box => {
                    if let Some(sz) = pr.size {
                        if !finite3(&sz) || sz.iter().any(|v| *v <= 0.0) {
                            out.push(Diagnostic::error_code(
                                ErrorCode::OutOfRange,
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
                        Diagnostic::error_code(
                            ErrorCode::UnknownReference,
                            format!("{p}.watch[{k}]"),
                            format!("找不到名为 \"{name}\" 的刚体"),
                        )
                        .with_hint(body_name_hint),
                    ),
                    Some(BodyKind::None) => out.push(Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        format!("{p}.watch[{k}]"),
                        format!("物体 \"{name}\" 没有刚体，位置永远不会变"),
                    )),
                    Some(_) => {}
                }
            }
            if self.simulate.steps == 0 {
                out.push(
                    Diagnostic::warn_code(
                        ErrorCode::Ineffective,
                        &p,
                        "simulate.steps = 0，探针不会观测到任何东西",
                    )
                    .with_hint("给 simulate.steps 一个正数（例如 120）"),
                );
            }
        }

        // Bloom：数值必须合理，且"开了但强度为 0"要提醒（那等于没开）
        if let Some(b) = &self.lighting.bloom {
            let enabled = b.enabled.unwrap_or(true);
            if let Some(i) = b.intensity {
                if !i.is_finite() || i < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.bloom.intensity",
                        "bloom 强度必须是非负数",
                    ));
                } else if i == 0.0 && enabled {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::Ineffective,
                            "lighting.bloom.intensity",
                            "bloom 开着但强度为 0，等于没开",
                        )
                        .with_hint("想要亮区外溢就给个正数（例如 0.6），或者直接去掉 bloom 这一节"),
                    );
                }
            }
            if let Some(t) = b.threshold {
                if !t.is_finite() || t < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.bloom.threshold",
                        "bloom 阈值必须是非负数",
                    ));
                } else if t > 20.0 {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        key,
                        "FXAA 阈值必须是非负数",
                    ));
                } else if v > 1.0 && enabled {
                    out.push(
                        Diagnostic::warn_code(ErrorCode::SuspiciousValue, key, format!("阈值 {v} 大于 1，几乎不会判定出边缘"))
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.ssao.radius",
                        "SSAO 半径必须是正数（单位：米）",
                    ));
                } else if r > 20.0 {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::SuspiciousValue,
                            "lighting.ssao.radius",
                            format!("半径 {r} 米太大，几乎整屏都会被压暗"),
                        )
                        .with_hint("物体尺度 1~2 米时用 0.5~1.0"),
                    );
                }
            }
            if let Some(b) = a.bias {
                if !b.is_finite() || b < 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.ssao.bias",
                        "SSAO 深度偏移不能是负数",
                    ));
                } else if let Some(r) = a.radius {
                    if r.is_finite() && r > 0.0 && b >= r {
                        out.push(
                            Diagnostic::warn_code(
                                ErrorCode::SuspiciousValue,
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        "lighting.ssao.strength",
                        "SSAO 强度不能是负数",
                    ));
                } else if st == 0.0 && enabled {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::Ineffective,
                            "lighting.ssao.strength",
                            "SSAO 开着但强度为 0，等于没开",
                        )
                        .with_hint("想要接触阴影就给个正数（例如 1.0），或者去掉 ssao 这一节"),
                    );
                } else if st > 1.0 {
                    out.push(Diagnostic::warn_code(
                        ErrorCode::SuspiciousValue,
                        "lighting.ssao.strength",
                        format!("强度 {st} 大于 1，接触处会被压得过黑"),
                    ));
                }
            }
            if let Some(n) = a.samples {
                if !(1..=64).contains(&n) {
                    out.push(
                        Diagnostic::error_code(
                            ErrorCode::OutOfRange,
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
                    Diagnostic::error_code(
                        ErrorCode::InvalidValue,
                        "lighting.preset",
                        format!("未知的光照预设 \"{preset}\""),
                    )
                    .with_hint("可选：studio（缺省）/ day / night / none（无主光、环境光为 0）"),
                );
            }
        }

        // 相机
        if let Some(f) = self.camera.fov_deg {
            if !f.is_finite() || !(1.0..=179.0).contains(&f) {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.fov_deg",
                    "视场角应在 1~179 度之间",
                ));
            }
        }
        if let Some(n) = self.camera.near {
            if !n.is_finite() || n <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.near",
                    "近裁剪面必须为正数",
                ));
            }
        }
        if let Some(f) = self.camera.far {
            if !f.is_finite() || f <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.far",
                    "远裁剪面必须为正数",
                ));
            }
        }
        if let (Some(n), Some(f)) = (self.camera.near, self.camera.far) {
            if n.is_finite() && f.is_finite() && f <= n {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.far",
                    format!("远裁剪面必须大于近裁剪面（near={n}, far={f}）"),
                ));
            }
        }
        if let Some(h) = self.camera.orthographic_height {
            if !h.is_finite() || h <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.orthographic_height",
                    "正交投影的可视高度必须为正数",
                ));
            }
        }
        if let Some(d) = self.camera.distance {
            if !d.is_finite() || d <= 0.0 {
                out.push(Diagnostic::error_code(
                    ErrorCode::OutOfRange,
                    "camera.distance",
                    "环绕距离必须为正数",
                ));
            }
        }
        if self.camera.position.is_some() && self.camera.look_at.is_none() {
            out.push(Diagnostic::warn_code(
                ErrorCode::SuspiciousValue,
                "camera.look_at",
                "只给了位置没给注视点，相机会看向原点",
            ));
        }

        // 仿真 / 渲染
        if !self.simulate.dt.is_finite() || self.simulate.dt <= 0.0 || self.simulate.dt > 0.5 {
            out.push(Diagnostic::error_code(
                ErrorCode::OutOfRange,
                "simulate.dt",
                "步长必须在 (0, 0.5] 秒之间",
            ));
        }
        if self.render.width == 0 || self.render.height == 0 {
            out.push(Diagnostic::error_code(
                ErrorCode::OutOfRange,
                "render.width/height",
                "渲染尺寸必须大于 0",
            ));
        }
        if self.render.width > 8192 || self.render.height > 8192 {
            out.push(Diagnostic::warn_code(
                ErrorCode::SuspiciousValue,
                "render.width/height",
                "渲染尺寸过大，软件光栅下可能非常慢",
            ));
        }
        if !matches!(self.render.msaa, 1 | 2 | 4 | 8) {
            out.push(Diagnostic::error_code(
                ErrorCode::InvalidValue,
                "render.msaa",
                "MSAA 只能是 1 / 2 / 4 / 8",
            ));
        }
        if self.render.output.as_os_str().is_empty() {
            out.push(Diagnostic::error_code(
                ErrorCode::InvalidValue,
                "render.output",
                "输出路径不能为空",
            ));
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
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.size"),
                        "边长必须是正有限数",
                    ));
                }
            }
        }
        ShapeKind::Sphere => {
            if let Some(r) = s.radius {
                if !r.is_finite() || r <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.radius"),
                        "半径必须为正数",
                    ));
                }
            }
        }
        ShapeKind::Capsule | ShapeKind::Cylinder | ShapeKind::Cone => {
            if let Some(h) = s.half_height {
                if !h.is_finite() || h <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.half_height"),
                        "半高必须为正数",
                    ));
                }
            }
            if let Some(r) = s.radius {
                if !r.is_finite() || r <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.radius"),
                        "半径必须为正数",
                    ));
                }
            }
        }
        ShapeKind::RoundedBox => {
            if let Some(size) = s.size {
                if !finite3(&size) || size.iter().any(|v| *v <= 0.0) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.size"),
                        "边长必须是正有限数",
                    ));
                }
            }
            let min_edge = s
                .size
                .map(|v| v.iter().cloned().fold(f32::INFINITY, f32::min))
                .unwrap_or(1.0);
            match s.radius {
                None => out.push(Diagnostic::error_code(
                    ErrorCode::MissingField,
                    format!("{path}.radius"),
                    "圆角盒必须给出 radius（圆角半径）：不知道给多少就试「最短边长 × 0.1」",
                )),
                Some(r) => {
                    if !r.is_finite() || r <= 0.0 {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{path}.radius"),
                            "圆角半径必须为正数",
                        ));
                    } else if min_edge.is_finite() && r > min_edge * 0.5 + 1e-6 {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{path}.radius"),
                            format!(
                                "圆角半径不能超过最短边长的一半（{} / 2 = {}）：再大就滚成胶囊/球了，改用 sphere 或 capsule",
                                min_edge,
                                min_edge * 0.5
                            ),
                        ));
                    }
                }
            }
        }
        ShapeKind::HeightField => {
            match &s.heights {
                None => out.push(Diagnostic::error_code(
                    ErrorCode::MissingField,
                    format!("{path}.heights"),
                    "高度场必须提供 heights（行主序的二维数组，至少 2 行 2 列）",
                )),
                Some(rows) => {
                    let cols = rows.first().map(|r| r.len()).unwrap_or(0);
                    if rows.len() < 2 || cols < 2 {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{path}.heights"),
                            format!("高度场至少 2×2，当前 {} 行 {} 列", rows.len(), cols),
                        ));
                    }
                    if rows.iter().any(|r| r.len() != cols) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::InvalidValue,
                            format!("{path}.heights"),
                            "每一行的列数必须相同（矩形高度场）",
                        ));
                    }
                    let points = rows.len().saturating_mul(cols);
                    if points > MAX_HEIGHTFIELD_POINTS {
                        out.push(
                            Diagnostic::error_code(
                                ErrorCode::LimitExceeded,
                                format!("{path}.heights"),
                                format!("高度场格点数 {points} 超过上限 {MAX_HEIGHTFIELD_POINTS}"),
                            )
                            .with_hint(
                                "物理上是静态三角网，窄相逐三角形（没有层次结构）：先用粗网格，\
                                 需要细节时把地形切块",
                            ),
                        );
                    }
                    if rows.iter().flatten().any(|h| !h.is_finite()) {
                        out.push(Diagnostic::error_code(
                            ErrorCode::OutOfRange,
                            format!("{path}.heights"),
                            "高度必须是有限数",
                        ));
                    }
                }
            }
            if let Some(c) = s.cell_size {
                if !c.is_finite() || c <= 0.0 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.cell_size"),
                        "格子边长必须是正有限数",
                    ));
                }
            }
        }
        ShapeKind::Plane => {
            if let Some(n) = s.normal {
                if !finite3(&n) || Vec3::from_array(n).length_squared() < 1e-6 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.normal"),
                        "法线必须是非零有限向量",
                    ));
                }
            }
        }
        ShapeKind::Convex => match &s.points {
            None => out.push(Diagnostic::error_code(
                ErrorCode::MissingField,
                format!("{path}.points"),
                "凸包必须提供 points",
            )),
            Some(pts) => {
                if pts.len() < 4 {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.points"),
                        format!("凸包至少需要 4 个顶点，当前 {}", pts.len()),
                    ));
                }
                if pts.iter().any(|p| !finite3(p)) {
                    out.push(Diagnostic::error_code(
                        ErrorCode::OutOfRange,
                        format!("{path}.points"),
                        "顶点必须是有限数",
                    ));
                }
                if pts.len() >= 4 && pts.iter().all(finite3) {
                    out.push(
                        Diagnostic::warn_code(
                            ErrorCode::Ineffective,
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
    fn waters_parse_validate_and_roundtrip() {
        let doc = SceneDoc::from_value(json!({
            "waters": [
                { "center": [1, 2], "size": [8, 6], "level": 0.4 },
                { "size": [4, 4], "level": 0.0, "density": 800, "drag": 0.0 }
            ]
        }))
        .expect("waters 要能解析");
        assert!(doc.validate().is_empty(), "{:?}", doc.validate());
        assert_eq!(doc.waters[0].density, 1000.0, "缺省密度是水");
        assert_eq!(doc.waters[0].drag, 6.0, "缺省阻尼");
        let text = serde_json::to_string(&doc).expect("序列化");
        let again = SceneDoc::from_json(&text).expect("往返");
        assert_eq!(again.waters.len(), 2);
        assert_eq!(again.waters[0].center, Some([1.0, 2.0]));
        assert_eq!(again.waters[0].size, [8.0, 6.0]);
        assert_eq!(again.waters[0].level, 0.4);
        assert_eq!(again.waters[1].density, 800.0);
        assert_eq!(again.waters[1].drag, 0.0);

        for (water, path) in [
            (json!({"size": [0, 4], "level": 0}), "waters[0].size"),
            (
                json!({"size": [4, 4], "level": 0, "density": 0}),
                "waters[0].density",
            ),
            (
                json!({"size": [4, 4], "level": 0, "drag": -1}),
                "waters[0].drag",
            ),
        ] {
            let doc = SceneDoc::from_value(json!({ "waters": [water] })).expect("结构合法");
            let diags = doc.validate();
            assert!(
                diags.iter().any(|d| d.path == path),
                "要报 {path}: {diags:?}"
            );
        }
    }

    #[test]
    fn fluids_parse_validate_and_roundtrip() {
        let doc = SceneDoc::from_value(json!({
            "fluids": [
                { "center": [0, 2, 0], "size": [1, 1, 1] },
                { "size": [2, 1, 1], "spacing": 0.2, "radius": 0.08,
                  "rest_density": 900, "viscosity": 0.1, "iterations": 6,
                  "max_particles": 500 }
            ]
        }))
        .expect("fluids 要能解析");
        assert!(doc.validate().is_empty(), "{:?}", doc.validate());
        assert_eq!(doc.fluids[0].spacing, 0.1, "缺省间距");
        assert_eq!(doc.fluids[0].radius(), 0.05, "缺省半径 = spacing/2");
        assert_eq!(doc.fluids[0].rest_density, 1000.0);
        assert_eq!(doc.fluids[0].iterations, 4);
        assert_eq!(doc.fluids[0].drag, 6.0, "缺省浸没阻尼");
        let text = serde_json::to_string(&doc).expect("序列化");
        let again = SceneDoc::from_json(&text).expect("往返");
        assert_eq!(again.fluids.len(), 2);
        assert_eq!(again.fluids[1].spacing, 0.2);
        assert_eq!(again.fluids[1].radius, Some(0.08));
        assert_eq!(again.fluids[1].max_particles, 500);

        for (fluid, path) in [
            (json!({"size": [0, 1, 1]}), "fluids[0].size"),
            (
                json!({"size": [1, 1, 1], "spacing": 0}),
                "fluids[0].spacing",
            ),
            (
                json!({"size": [1, 1, 1], "spacing": 3.0}),
                "fluids[0].spacing",
            ),
            (
                json!({"size": [1, 1, 1], "radius": 0.2}),
                "fluids[0].radius",
            ),
            (
                json!({"size": [1, 1, 1], "rest_density": 0}),
                "fluids[0].rest_density",
            ),
            (
                json!({"size": [1, 1, 1], "viscosity": 2}),
                "fluids[0].viscosity",
            ),
            (json!({"size": [1, 1, 1], "drag": -1}), "fluids[0].drag"),
            (
                json!({"size": [1, 1, 1], "iterations": 0}),
                "fluids[0].iterations",
            ),
            (
                json!({"size": [1, 1, 1], "max_particles": 0}),
                "fluids[0].max_particles",
            ),
        ] {
            let doc = SceneDoc::from_value(json!({ "fluids": [fluid] })).expect("结构合法");
            let diags = doc.validate();
            assert!(
                diags.iter().any(|d| d.path == path),
                "要报 {path}: {diags:?}"
            );
        }
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

    #[test]
    fn cli_asset_paths_are_tried_after_the_scene_ones() {
        let dir = std::env::temp_dir().join(format!("quench3d_cli_assets_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let scene = dir.join("level.json");
        std::fs::write(
            &scene,
            serde_json::to_string(&json!({
                "assets": { "search_paths": ["from_scene"] },
                "models": [ { "path": "robot.obj" } ]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut doc = SceneDoc::from_file(&scene).unwrap();
        doc.extra_asset_paths = vec![PathBuf::from("from_cli"), dir.join("abs_dir")];
        let c = doc.model_candidates(std::path::Path::new("robot.obj"));
        assert_eq!(c[0], dir.join("robot.obj"), "场景目录优先：{c:?}");
        assert_eq!(c[1], dir.join("from_scene").join("robot.obj"), "{c:?}");
        assert_eq!(
            c[2],
            dir.join("from_cli").join("robot.obj"),
            "CLI 的排在场景声明之后（相对路径以场景目录为基准）：{c:?}"
        );
        assert_eq!(
            c[3],
            dir.join("abs_dir").join("robot.obj"),
            "绝对路径原样（用真正绝对的真实目录：Windows 上 \"/abs\" 不是绝对路径）：{c:?}"
        );
        // 不写 --asset-path 时这一个键不会被序列化出来（老场景逐字节不变）
        let plain = SceneDoc::from_file(&scene).unwrap();
        assert!(plain.extra_asset_paths.is_empty());
        let round = SceneDoc::from_json(&plain.to_json_pretty()).unwrap();
        assert!(round.extra_asset_paths.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spot_light_validation_catches_the_ways_cones_go_wrong() {
        // 缺省值要能直接用：只写位置与方向就是一盏合法的聚光灯
        let ok = SceneDoc::from_value(json!({ "lighting": { "spot_lights": [
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0] } ] } }))
        .unwrap();
        let d = ok.validate();
        assert!(
            d.iter().all(|x| x.severity != Severity::Error),
            "只写位置+方向应当合法：{d:?}"
        );
        let bad = SceneDoc::from_value(json!({ "lighting": { "spot_lights": [
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, 0.0, 0.0],
              "inner_deg": 30.0, "outer_deg": 20.0, "radius": 0.0 },
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0], "inner_deg": 95.0 } ] } }))
        .unwrap();
        let d = bad.validate();
        for path in [
            "lighting.spot_lights[0].direction",
            "lighting.spot_lights[0].inner_deg",
            "lighting.spot_lights[0].radius",
            "lighting.spot_lights[1].inner_deg",
        ] {
            assert!(
                d.iter()
                    .any(|x| x.path == path && x.severity == Severity::Error),
                "{path} 应当报错：{d:?}"
            );
        }
        // 局部光共享名额：超过 8 盏要警告（点光源优先，超出的不参与着色）
        let many: Vec<serde_json::Value> = (0..9)
            .map(|i| {
                json!({ "position": [i as f32, 5.0, 0.0], "direction": [0.0, -1.0, 0.0],
                             "radius": 10.0 })
            })
            .collect();
        let over = SceneDoc::from_value(json!({ "lighting": { "spot_lights": many } })).unwrap();
        assert!(
            over.validate()
                .iter()
                .any(|x| x.severity == Severity::Warning && x.path == "lighting.spot_lights"),
            "9 盏聚光灯要警告超预算：{:?}",
            over.validate()
        );
    }

    /// 聚光阴影：只支持一盏，校验必须**说清用哪一盏**（否则用户只能靠猜）。
    #[test]
    fn spot_shadow_validation_allows_only_one_caster_and_says_which() {
        let one = SceneDoc::from_value(json!({ "lighting": { "spot_lights": [
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0], "cast_shadows": true } ] } }))
        .unwrap();
        let d = one.validate();
        assert!(
            d.iter().all(|x| x.severity != Severity::Warning),
            "一盏投影灯应当完全合法、也没有警告：{d:?}"
        );

        // 两盏都勾：警告落在**第一盏**上，并指出实际用的是它
        let two = SceneDoc::from_value(json!({ "lighting": { "spot_lights": [
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0] },
            { "position": [3.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0], "cast_shadows": true },
            { "position": [6.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0], "cast_shadows": true } ] } }))
        .unwrap();
        let d = two.validate();
        let w = d
            .iter()
            .find(|x| x.severity == Severity::Warning && x.path.ends_with("cast_shadows"))
            .unwrap_or_else(|| panic!("两盏要求投影必须警告：{d:?}"));
        assert_eq!(
            w.path, "lighting.spot_lights[1].cast_shadows",
            "警告要落在实际投影的那一盏（第一盏要求投影的）上：{d:?}"
        );
        assert!(
            w.message.contains("spot_lights[1]"),
            "警告里必须写清用哪一盏：{}",
            w.message
        );
        assert!(w.hint.is_some(), "要给一条可执行的建议");

        // 投影灯被局部光名额挤掉：8 盏点光已经占满 → 它既不照明也不投影
        let points: Vec<serde_json::Value> = (0..8)
            .map(|i| json!({ "position": [i as f32, 5.0, 0.0] }))
            .collect();
        let starved = SceneDoc::from_value(json!({ "lighting": {
            "point_lights": points,
            "spot_lights": [ { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0],
                               "cast_shadows": true } ] } }))
        .unwrap();
        let d = starved.validate();
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning
                && x.path == "lighting.spot_lights[0].cast_shadows"
                && x.message.contains("名额")),
            "投影灯被名额挤掉时必须单独警告：{d:?}"
        );

        // 老场景（不写这个字段）= 不投影、零警告、零错误
        let old = SceneDoc::from_value(json!({ "lighting": { "spot_lights": [
            { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0] },
            { "position": [3.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0] } ] } }))
        .unwrap();
        assert!(
            old.validate()
                .iter()
                .all(|x| x.severity != Severity::Warning),
            "不写 cast_shadows 的老场景不许多出任何警告：{:?}",
            old.validate()
        );
        // 存盘文本里也不该冒出这个字段（不写 = 老行为）
        assert!(!old.to_json_pretty().contains("cast_shadows"));
        assert!(old.lighting.spot_lights.as_ref().unwrap()[0]
            .cast_shadows
            .is_none());
    }

    /// 点光阴影：与聚光那条同形状，但**名额规则相反**——点光源优先占名额，
    /// 所以"被挤掉"只可能是"它自己排在第 9 盏以上"。
    #[test]
    fn point_shadow_validation_allows_only_one_caster_and_says_which() {
        let one = SceneDoc::from_value(json!({ "lighting": { "point_lights": [
            { "position": [0.0, 3.0, 0.0], "cast_shadows": true } ] } }))
        .unwrap();
        let d = one.validate();
        assert!(
            d.iter().all(|x| x.severity != Severity::Warning),
            "一盏投影点光源应当完全合法、也没有警告：{d:?}"
        );

        // 两盏都勾：警告落在**第一盏**上，并指出实际用的是它
        let two = SceneDoc::from_value(json!({ "lighting": { "point_lights": [
            { "position": [0.0, 3.0, 0.0] },
            { "position": [3.0, 3.0, 0.0], "cast_shadows": true },
            { "position": [6.0, 3.0, 0.0], "cast_shadows": true } ] } }))
        .unwrap();
        let d = two.validate();
        let w = d
            .iter()
            .find(|x| x.severity == Severity::Warning && x.path.ends_with("cast_shadows"))
            .unwrap_or_else(|| panic!("两盏要求投影必须警告：{d:?}"));
        assert_eq!(
            w.path, "lighting.point_lights[1].cast_shadows",
            "警告要落在实际投影的那一盏上：{d:?}"
        );
        assert!(
            w.message.contains("point_lights[1]"),
            "警告里必须写清用哪一盏：{}",
            w.message
        );
        assert!(w.hint.is_some(), "要给一条可执行的建议");

        // 排在名额之外（第 9 盏）要求投影：它既不会照明也不会投影
        let mut lights: Vec<serde_json::Value> = (0..8)
            .map(|i| json!({ "position": [i as f32, 3.0, 0.0] }))
            .collect();
        lights.push(json!({ "position": [9.0, 3.0, 0.0], "cast_shadows": true }));
        let starved =
            SceneDoc::from_value(json!({ "lighting": { "point_lights": lights } })).unwrap();
        let d = starved.validate();
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning
                && x.path == "lighting.point_lights[8].cast_shadows"
                && x.message.contains("名额")),
            "名额外的投影点光源必须单独警告：{d:?}"
        );

        // 点光 + 聚光各自投影：两套阴影图互不影响，各自一盏都不该有"多盏"警告
        let both = SceneDoc::from_value(json!({ "lighting": {
            "point_lights": [ { "position": [0.0, 3.0, 0.0], "cast_shadows": true } ],
            "spot_lights": [ { "position": [0.0, 5.0, 0.0], "direction": [0.0, -1.0, 0.0],
                               "cast_shadows": true } ] } }))
        .unwrap();
        let d = both.validate();
        assert!(
            d.iter().all(|x| x.severity != Severity::Warning),
            "一盏投影点光 + 一盏投影聚光是允许的组合：{d:?}"
        );

        // 老场景（不写这个字段）= 不投影、零警告、零错误、存盘文本里不冒出这个字段
        let old = SceneDoc::from_value(json!({ "lighting": { "point_lights": [
            { "position": [0.0, 3.0, 0.0] },
            { "position": [3.0, 3.0, 0.0] } ] } }))
        .unwrap();
        assert!(
            old.validate()
                .iter()
                .all(|x| x.severity != Severity::Warning),
            "不写 cast_shadows 的老场景不许多出任何警告：{:?}",
            old.validate()
        );
        assert!(!old.to_json_pretty().contains("cast_shadows"));
        assert!(old.lighting.point_lights.as_ref().unwrap()[0]
            .cast_shadows
            .is_none());
    }

    #[test]
    fn post_processing_survives_the_round_trip_through_renderer_options() {
        // 与级联那条同理：编辑器载入时用 runner 的映射写进 RendererOptions，存盘时从
        // RendererOptions 写回。少了这条，"编辑器里勾掉 FXAA、存盘却还开着"只能靠肉眼发现。
        use crate::runner::{bloom_options, fxaa_options, ssao_options};
        let doc = SceneDoc::from_value(json!({ "lighting": {
            "bloom": { "enabled": false, "intensity": 1.25, "threshold": 2.5 },
            "fxaa": { "enabled": true, "edge_threshold": 0.08, "edge_threshold_min": 0.02 },
            "ssao": { "enabled": true, "radius": 1.5, "bias": 0.05, "strength": 0.9, "samples": 24 }
        }}))
        .unwrap();
        let (b, f, a) = (bloom_options(&doc), fxaa_options(&doc), ssao_options(&doc));
        assert!(
            !b.enabled && (b.intensity - 1.25).abs() < 1e-6 && (b.threshold - 2.5).abs() < 1e-6
        );
        assert!(f.enabled && (f.edge_threshold - 0.08).abs() < 1e-6);
        assert!((f.edge_threshold_min - 0.02).abs() < 1e-6);
        assert!(a.enabled && (a.radius - 1.5).abs() < 1e-6 && (a.bias - 0.05).abs() < 1e-6);
        assert!((a.strength - 0.9).abs() < 1e-6 && a.samples == 24);
        // 写回（编辑器就是这么干的）：值从渲染器选项回到文档，一个不丢
        let written = (
            BloomDoc {
                enabled: Some(b.enabled),
                intensity: Some(b.intensity),
                threshold: Some(b.threshold),
            },
            FxaaDoc {
                enabled: Some(f.enabled),
                edge_threshold: Some(f.edge_threshold),
                edge_threshold_min: Some(f.edge_threshold_min),
            },
            SsaoDoc {
                enabled: Some(a.enabled),
                radius: Some(a.radius),
                bias: Some(a.bias),
                strength: Some(a.strength),
                samples: Some(a.samples),
            },
        );
        assert_eq!(written.0.intensity, Some(1.25));
        assert_eq!(written.1.edge_threshold, Some(0.08));
        assert_eq!(written.2.samples, Some(24));
        // 不写 = 三项都关（老场景逐像素不变）
        let plain = SceneDoc::from_value(json!({})).unwrap();
        assert!(!bloom_options(&plain).enabled);
        assert!(!fxaa_options(&plain).enabled);
        assert!(!ssao_options(&plain).enabled);
        // 写了 `{}` 空节 = "就是想开"，其余字段取缺省（文档里写明的语义）
        let bare =
            SceneDoc::from_value(json!({ "lighting": { "bloom": {}, "fxaa": {}, "ssao": {} } }))
                .unwrap();
        assert!(bloom_options(&bare).enabled);
        assert!(fxaa_options(&bare).enabled);
        assert!(ssao_options(&bare).enabled);
        assert!((bloom_options(&bare).intensity - 0.6).abs() < 1e-6);
    }

    #[test]
    fn shadow_quality_survives_the_round_trip_through_renderer_options() {
        // 编辑器从 `engine.renderer.options` 写回级联数与切分系数，而载入时用的是
        // `runner` 的同一份映射。这条测试钉住那个往返：场景写多少 → 渲染器用多少 →
        // 存回去还是多少。少了它，"编辑器把 cascades: 4 悄悄存回 1"这种丢失只能靠肉眼发现。
        use crate::runner::{shadow_cascades, shadow_split_lambda};
        for n in 1..=4u32 {
            for lambda in [0.0f32, 0.35, 0.5, 1.0] {
                let doc = SceneDoc::from_value(json!({ "lighting": { "shadow_quality": {
                    "cascades": n, "split_lambda": lambda } } }))
                .unwrap();
                assert_eq!(shadow_cascades(&doc), n);
                assert_eq!(shadow_split_lambda(&doc), lambda);
                let written = ShadowQualityDoc {
                    strength: Some(1.0),
                    radius: None,
                    cascades: Some(shadow_cascades(&doc)),
                    split_lambda: Some(shadow_split_lambda(&doc)),
                };
                assert_eq!(written.cascades, Some(n));
                assert_eq!(written.split_lambda, Some(lambda));
            }
        }
        // 不写 = 单级 + 0.5（老场景逐像素不变）
        let plain = SceneDoc::from_value(json!({})).unwrap();
        assert_eq!(shadow_cascades(&plain), 1);
        assert_eq!(shadow_split_lambda(&plain), 0.5);
        // 手改 JSON 绕过校验时的兜底：映射函数自己再夹一次范围
        let wild = SceneDoc::from_value(json!({ "lighting": { "shadow_quality": {
            "cascades": 99, "split_lambda": 5.0 } } }))
        .unwrap();
        assert_eq!(shadow_cascades(&wild), 4);
        assert_eq!(shadow_split_lambda(&wild), 1.0);
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

    /// 关节局部轴按端拆分（`axis_a` / `axis_b`）：解析、往返、默认不落盘、诊断。
    ///
    /// 这三件事各有各的失败方式：少了往返 → 编辑器存盘丢字段；缺省落盘 → 老场景存盘
    /// 后不再逐字节相同；少了诊断 → 用户在 `fixed` 上写 `axis_a` 时以为生效了。
    #[test]
    fn joint_local_axes_are_per_side_and_diagnosed() {
        // ① 按端给轴：解析 + 往返，且不报错
        let doc = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "框", "body": "static" },
                              { "name": "门", "body": "dynamic", "rotation_deg": [90, 0, 0] } ],
                 "joints": [ { "type": "hinge", "a": "框", "b": "门",
                               "axis_a": [0, 1, 0], "axis_b": [0, 0, 1] } ] }"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        assert!(
            !diags
                .iter()
                .any(|d| d.severity == Severity::Warning && d.path == "joints[0].axis"),
            "按端给轴之后不该再有「同一个局部轴」的提醒：{diags:?}"
        );
        assert_eq!(doc.joints[0].axis_a, Some([0.0, 1.0, 0.0]));
        assert_eq!(doc.joints[0].axis_b, Some([0.0, 0.0, 1.0]));
        let text = doc.to_json_pretty();
        let back = SceneDoc::from_json(&text).unwrap();
        assert_eq!(back.joints[0].axis_a, Some([0.0, 1.0, 0.0]));
        assert_eq!(back.joints[0].axis_b, Some([0.0, 0.0, 1.0]));

        // ② 不写 = 不落盘（老场景存盘仍然逐字节不变）
        let plain = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "hinge", "a": "a", "b": "b", "axis": [0, 1, 0] } ] }"#,
        )
        .unwrap();
        assert!(plain.joints[0].axis_a.is_none() && plain.joints[0].axis_b.is_none());
        let plain_text = plain.to_json_pretty();
        assert!(
            !plain_text.contains("axis_a") && !plain_text.contains("axis_b"),
            "没写就不许落盘：{plain_text}"
        );

        // ③ 零向量轴：报在自己的路径上（不能只查 `axis`）
        let bad = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "hinge", "a": "a", "b": "b", "axis_b": [0, 0, 0] } ] }"#,
        )
        .unwrap();
        let diags = bad.validate();
        let d = diag_at(&diags, "joints[0].axis_b").expect("零向量轴必须报错");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, ErrorCode::OutOfRange);

        // ④ 给没有轴的关节（fixed/ball/distance）写 axis_a → `ineffective` 提醒，不是静默忽略
        let wrong_kind = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "fixed", "a": "a", "b": "b", "axis_a": [0, 1, 0] } ] }"#,
        )
        .unwrap();
        let diags = wrong_kind.validate();
        let d = diag_at(&diags, "joints[0].axis_a").expect("必须提醒会被忽略");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, ErrorCode::Ineffective);

        // ⑤ 两端朝向不同却只给一根局部轴 → `suspicious_value`：这正是 axis_a/axis_b 的理由
        let trap = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "框", "body": "static" },
                              { "name": "门", "body": "dynamic", "rotation_deg": [90, 0, 0] } ],
                 "joints": [ { "type": "hinge", "a": "框", "b": "门", "axis": [0, 1, 0] } ] }"#,
        )
        .unwrap();
        let diags = trap.validate();
        let d = diag_at(&diags, "joints[0].axis").expect("朝向不同必须提醒");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, ErrorCode::SuspiciousValue);

        // ⑥ 两端朝向相同：同样的写法不该报（否则老场景全被吵醒）
        let same = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "hinge", "a": "a", "b": "b", "axis": [0, 1, 0] } ] }"#,
        )
        .unwrap();
        assert!(
            !same.validate().iter().any(|d| d.path == "joints[0].axis"),
            "两端朝向相同不该报轴不同轴"
        );
    }

    /// 固定关节的**参考姿态**（`frame_a` / `frame_b`）：解析、往返、默认不落盘、诊断。
    ///
    /// 与 `axis_a`/`axis_b` 一样的三件事，外加这一节独有的第二条：**写了却等于没写**
    /// （两端都是 0）必须吵一声 —— 那种写法读起来像"生效了"，实际走的是老行为。
    #[test]
    fn joint_reference_frames_parse_round_trip_and_diagnose() {
        // ① fixed 上给参考姿态：解析 + 往返，且不报错
        let doc = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "fixed", "a": "a", "b": "b",
                               "frame_a": [0, 90, 0], "frame_b": [0, 0, 0] } ] }"#,
        )
        .unwrap();
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        assert!(
            !diags.iter().any(|d| d.path.starts_with("joints[0].frame")),
            "有意义的参考姿态不该被警告：{diags:?}"
        );
        assert_eq!(doc.joints[0].frame_a, Some([0.0, 90.0, 0.0]));
        assert_eq!(doc.joints[0].frame_b, Some([0.0, 0.0, 0.0]));
        let text = doc.to_json_pretty();
        let back = SceneDoc::from_json(&text).unwrap();
        assert_eq!(back.joints[0].frame_a, Some([0.0, 90.0, 0.0]));
        assert_eq!(back.joints[0].frame_b, Some([0.0, 0.0, 0.0]));

        // ② 不写 = 不落盘（老场景存盘仍然逐字节不变）
        let plain = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "fixed", "a": "a", "b": "b" } ] }"#,
        )
        .unwrap();
        assert!(plain.joints[0].frame_a.is_none() && plain.joints[0].frame_b.is_none());
        let plain_text = plain.to_json_pretty();
        assert!(
            !plain_text.contains("frame_a") && !plain_text.contains("frame_b"),
            "没写就不许落盘：{plain_text}"
        );

        // ③ 非有限数：报在自己的路径上（结构解析进不来的值走语义校验）
        let mut nan = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "fixed", "a": "a", "b": "b", "frame_b": [0, 0, 0] } ] }"#,
        )
        .unwrap();
        nan.joints[0].frame_a = Some([f32::NAN, 0.0, 0.0]);
        let diags = nan.validate();
        let d = diag_at(&diags, "joints[0].frame_a").expect("非有限参考姿态必须报错");
        assert_eq!(d.severity, Severity::Error);
        assert_eq!(d.code, ErrorCode::OutOfRange);

        // ④ 写在非 fixed 关节上 → `ineffective`（字段会被忽略，不是静默失效）
        let wrong_kind = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "hinge", "a": "a", "b": "b", "frame_a": [0, 90, 0] } ] }"#,
        )
        .unwrap();
        let diags = wrong_kind.validate();
        let d = diag_at(&diags, "joints[0].frame_a").expect("只有 fixed 读它，必须提醒");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, ErrorCode::Ineffective);

        // ⑤ **两端都是 0** = 与不写逐位等价 → 也是 `ineffective`（最难查的那种"写了没用"）
        let zero = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "a", "body": "static" }, { "name": "b", "body": "dynamic" } ],
                 "joints": [ { "type": "fixed", "a": "a", "b": "b", "frame_a": [0, 0, 0] } ] }"#,
        )
        .unwrap();
        let diags = zero.validate();
        let d = diag_at(&diags, "joints[0].frame_a").expect("全 0 的参考姿态等于没写，必须提醒");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code, ErrorCode::Ineffective);
        assert!(
            d.message.contains("没写") || d.message.contains("锁定"),
            "要说清它等于老行为：{}",
            d.message
        );

        // ⑥ 老场景（完全不给 frame_*）不许被这条新警告吵醒
        assert!(
            !plain
                .validate()
                .iter()
                .any(|d| d.path.starts_with("joints[0].frame")),
            "不给参考姿态的老场景不该有任何新诊断"
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

    // ---------- 诊断码：稳定契约（第 55 轮） ----------

    /// 把一段场景 JSON 的**全部**诊断收集成 `(code, path)`：结构错误（`from_json` 就失败）
    /// 与语义错误（`validate`）都算。
    fn codes_of(json_text: &str) -> Vec<(String, String)> {
        let diags = match SceneDoc::from_json(json_text) {
            Ok(doc) => doc.validate(),
            Err(DocError::Invalid(diags)) => diags,
            Err(e) => panic!("既不是结构诊断也不是合法场景：{e}"),
        };
        diags
            .into_iter()
            .map(|d| (d.code.as_str().to_string(), d.path))
            .collect()
    }

    #[test]
    fn error_code_table_is_self_consistent() {
        // 宏从同一份声明生成枚举/ALL/TABLE，所以这里主要盯"表里的内容本身能不能当契约用"。
        assert_eq!(
            ErrorCode::ALL.len(),
            ErrorCode::TABLE.len(),
            "ALL 与 TABLE 长度不一致"
        );
        assert!(
            ErrorCode::ALL.contains(&ErrorCode::Invalid),
            "兜底码必须在表里"
        );
        let mut seen = std::collections::BTreeSet::new();
        for (i, (code, wire, meaning, action)) in ErrorCode::TABLE.iter().enumerate() {
            assert_eq!(*code, ErrorCode::ALL[i], "ALL 与 TABLE 顺序不一致");
            assert_eq!(code.as_str(), *wire, "as_str 与表里的名字不一致");
            assert!(
                !wire.is_empty() && !meaning.is_empty() && !action.is_empty(),
                "{wire} 有空字段"
            );
            // 竖线会破坏 docs/error_codes.md 的表格解析（对账测试会静默漏行）。
            assert!(
                !wire.contains('|') && !meaning.contains('|') && !action.contains('|'),
                "{wire} 的字段里不能有 `|`"
            );
            assert!(
                wire.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{wire} 不是 snake_case 的线上名字"
            );
            assert!(seen.insert(*wire), "码名重复：{wire}");
            // 线上名字必须真的是 serde 序列化结果（否则 schema 与 JSON 会对不上）
            let value = serde_json::to_value(code).expect("序列化诊断码");
            assert_eq!(value, serde_json::json!(wire), "{wire} 的 serde 名字不一致");
            let back: ErrorCode = serde_json::from_value(value).expect("反序列化诊断码");
            assert_eq!(back, *code);
        }
        assert_eq!(seen.len(), ErrorCode::ALL.len());
        assert_eq!(ErrorCode::default(), ErrorCode::Invalid, "缺省必须是兜底码");
    }

    #[test]
    fn error_codes_doc_matches_the_table() {
        // 文档表与码表**逐格**对账：改了 `define_error_codes!` 而没重算 docs/error_codes.md
        // 就会红。生成命令写在文档开头（quench3d schema --json 一行管道）。
        const DOC: &str = include_str!("../../../docs/error_codes.md");
        let body = DOC
            .split("<!-- BEGIN GENERATED TABLE")
            .nth(1)
            .expect("docs/error_codes.md 缺少 BEGIN 标记")
            .split("<!-- END GENERATED TABLE -->")
            .next()
            .expect("docs/error_codes.md 缺少 END 标记");
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if !line.starts_with("| `") {
                continue;
            }
            let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 3, "文档表这一行不是 3 列：{line}");
            rows.push((
                cells[0].trim_matches('`').to_string(),
                cells[1].to_string(),
                cells[2].to_string(),
            ));
        }
        assert_eq!(
            rows.len(),
            ErrorCode::TABLE.len(),
            "docs/error_codes.md 里的码数量与 ErrorCode::TABLE 不一致"
        );
        for ((_, wire, meaning, action), (dw, dm, da)) in ErrorCode::TABLE.iter().zip(rows.iter()) {
            assert_eq!(wire, dw, "码名不一致");
            assert_eq!(meaning, dm, "{wire} 的含义与文档不一致");
            assert_eq!(action, da, "{wire} 的 agent 动作与文档不一致");
        }
    }

    /// 断言某段场景在某个路径上给出某个码，且不出现兜底码。
    /// 断言"这段 JSON 在 `path` 上给出 `code`"，并返回该码以便测试收集覆盖集。
    ///
    /// 返回值不是装饰：下面用它拼出"这份契约覆盖了哪些码"，从而能反过来断言
    /// **码表里除兜底码外的每一个码都在这份契约里有真实触发场景**（见测试末尾）。
    #[track_caller]
    fn assert_code(json_text: &str, code: &'static str, path: &str) -> &'static str {
        let got = codes_of(json_text);
        assert!(
            got.iter().any(|(c, p)| c == code && p == path),
            "期望在 {path} 上得到 {code}，实际：{got:?}"
        );
        assert!(
            !got.iter().any(|(c, _)| c == "invalid"),
            "这段场景不该出现兜底码 invalid：{got:?}"
        );
        code
    }

    /// **稳定契约**：哪种问题给哪个码。这里每一条都是 agent 会用来分支的判定，
    /// 改了码（或改了 path）必须是有意识的决定，而不是顺手。
    ///
    /// 测试末尾做了**双向对账**：
    /// - 正向：每条断言里的码都必须是码表里的码（`assert_code` 只接受 `&'static str`，
    ///   但"码表里有"由 `error_codes_doc_matches_the_table` + 下面的反向断言共同锁住）；
    /// - 反向：**码表里除兜底码 `invalid` 外的每一个码，都必须在这里有真实触发场景** ——
    ///   这样"加了码但没有任何场景真的会发出它"（文档里躺着的死码）本地就红。
    ///
    /// `invalid` 是唯一豁免：它是老 JSON / 兜底路径的码，由
    /// `diagnostics_without_a_code_deserialize_to_invalid` 单独守着。
    #[test]
    fn diagnostic_codes_are_stable_and_specific() {
        let mut covered = vec![
            // 结构层：JSON 一解析就失败
            assert_code(r#"{"object": []}"#, "unknown_field", "object"),
            assert_code(r#"{"gravity": "up"}"#, "invalid_type", "gravity"),
            assert_code(
                r#"{"objects":[{"shape":{"type":"pyramid"}}]}"#,
                "invalid_value",
                "objects[0].shape.type",
            ),
            assert_code(
                r#"{"objects":[{"name":"a","shape":{}}]}"#,
                "missing_field",
                "objects[0].shape",
            ),
            // 引用 / 命名
            assert_code(
                r#"{"objects":[{"name":"a","body":"dynamic","shape":{"type":"box","size":[1,1,1]}}],
                "joints":[{"type":"fixed","a":"a","b":"nope"}]}"#,
                "unknown_reference",
                "joints[0].b",
            ),
            assert_code(
                r#"{"objects":[{"name":"a"},{"name":"a"}]}"#,
                "duplicate_name",
                "objects[1].name",
            ),
            assert_code(
                r#"{"objects":[{"name":"a","parent":"b"},{"name":"b","parent":"a"}]}"#,
                "cycle",
                "objects[0].parent",
            ),
            assert_code(r#"{"cameras":{"default":{}}}"#, "reserved_name", "cameras"),
            // 取值 / 互斥 / 上限
            assert_code(
                r#"{"objects":[{"name":"a","shape":{"type":"box"},"shapes":[{"shape":{"type":"box"}}]}]}"#,
                "mutually_exclusive",
                "objects[0].shapes",
            ),
            assert_code(
                r#"{"camera":{"fov_deg":400}}"#,
                "out_of_range",
                "camera.fov_deg",
            ),
            assert_code(
                r#"{"simulate":{"steps":2000000}}"#,
                "limit_exceeded",
                "simulate.steps",
            ),
            // 语义：写了不生效 / 单位易混 / 资源找不到 / 合法但很可能不是本意
            assert_code(
                r#"{"objects":[{"name":"a","body":"none","velocity":[1,0,0]}]}"#,
                "ineffective",
                "objects[0].velocity",
            ),
            assert_code(
                r#"{"objects":[{"name":"a","body":"dynamic","shape":{"type":"box","size":[1,1,1]}},
                 {"name":"b","body":"dynamic","shape":{"type":"box","size":[1,1,1]}}],
                "joints":[{"type":"hinge","a":"a","b":"b","limits":[0,90]}]}"#,
                "unit_ambiguity",
                "joints[0].limits",
            ),
            assert_code(
                r#"{"models":[{"path":"definitely_missing_xyz.glb"}]}"#,
                "asset_not_found",
                "models[0].path",
            ),
            assert_code(
                r#"{"lighting":{"fog":{"density":5.0}}}"#,
                "suspicious_value",
                "lighting.fog.density",
            ),
        ];
        // io_error 走的是 `from_file`（读盘失败），不是 `from_json`
        let missing = std::env::temp_dir().join("quench3d_no_such_dir_55b/scene.json");
        let diags = match SceneDoc::from_file(&missing) {
            Err(DocError::Invalid(diags)) => diags,
            other => panic!("读不存在的文件应当给结构诊断，实际：{other:?}"),
        };
        assert!(
            diags.iter().any(|d| d.code == ErrorCode::IoError),
            "读盘失败应当是 io_error：{diags:?}"
        );
        covered.push("io_error");

        // include_cycle 走的是 `resolved()`（要真的有两个互相包含的文件），也不是 from_json。
        let dir = std::env::temp_dir().join(format!("quench3d_cycle_55c_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("cycle_a.json"),
            r#"{"format":"quench3d.scene","version":2,"include":["cycle_b.json"]}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("cycle_b.json"),
            r#"{"format":"quench3d.scene","version":2,"include":["cycle_a.json"]}"#,
        )
        .unwrap();
        let a = SceneDoc::from_file(dir.join("cycle_a.json")).unwrap();
        let resolved = a.resolved();
        assert!(resolved.doc.is_none(), "成环必须失败");
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| d.code == ErrorCode::IncludeCycle),
            "成环应当是 include_cycle：{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);
        covered.push("include_cycle");

        // 反向对账：码表里（兜底码除外）每一个码都要在上面有真实触发场景。
        covered.sort_unstable();
        covered.dedup();
        let mut expected: Vec<&str> = ErrorCode::TABLE
            .iter()
            .map(|(_, wire, _, _)| *wire)
            .filter(|w| *w != ErrorCode::Invalid.as_str())
            .collect();
        expected.sort_unstable();
        assert_eq!(
            covered,
            expected,
            "稳定契约的覆盖集与码表不一致：缺场景的码 = {:?}，多出来的 = {:?}",
            expected
                .iter()
                .filter(|w| !covered.contains(w))
                .collect::<Vec<_>>(),
            covered
                .iter()
                .filter(|w| !expected.contains(w))
                .collect::<Vec<_>>()
        );
    }

    /// **码表里每个码都必须在源码里有真实发出点**（不是只在文档里躺着的死码）。
    ///
    /// 判据用源码对账（`include_str!` 读自己）：场景空间是无穷的，穷举不出所有码；
    /// 但"某个码在源码里一次都没被用过"是可以机械判定的，而它正是"加了码忘了接线"
    /// 的形状。数的是 `mod tests` **之前**的主体，所以测试自己引用变体不算数。
    ///
    /// 与 `diagnostic_codes_are_stable_and_specific` 互补：后者证"每个码都有场景"，
    /// 前者证"每个码都有发出点"（含豁免的兜底码 `invalid`，它由两参数构造器发出）。
    #[test]
    fn every_code_in_the_table_is_emitted_by_the_source() {
        let src = include_str!("scene_doc.rs");
        let body = src
            .split("mod tests {")
            .next()
            .expect("源码里应当有 mod tests");
        for (code, wire, _, _) in ErrorCode::TABLE {
            let needle = format!("ErrorCode::{code:?}");
            assert!(
                body.contains(&needle),
                "码 `{wire}`（{needle}）在源码主体里没有任何发出点 —— 是死码：\
                 要么接上真实的校验分支，要么从码表里删掉"
            );
        }
    }

    /// 老 JSON（诊断里没有 `code`）必须还能反序列化，并且落到兜底码。
    #[test]
    fn diagnostics_without_a_code_deserialize_to_invalid() {
        let d: Diagnostic =
            serde_json::from_str(r#"{"severity":"error","path":"objects[0]","message":"旧诊断"}"#)
                .expect("没有 code 的老诊断要能解析");
        assert_eq!(d.code, ErrorCode::Invalid);
        // 序列化时 code 一定在（契约：每条诊断都带码）
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["code"], "invalid");
        assert_eq!(v["path"], "objects[0]");
    }

    /// serde 错误分类：结构检查的唯一出口，分类错了所有字段级诊断都会错。
    #[test]
    fn serde_errors_classify_into_stable_codes() {
        assert_eq!(
            classify_serde_code("unknown field `foo`, expected one of `a`, `b`"),
            ErrorCode::UnknownField
        );
        assert_eq!(
            classify_serde_code("missing field `type`"),
            ErrorCode::MissingField
        );
        assert_eq!(
            classify_serde_code("unknown variant `pyramid`, expected one of `box`"),
            ErrorCode::InvalidValue
        );
        assert_eq!(
            classify_serde_code("invalid type: string \"up\", expected an array of 3 numbers"),
            ErrorCode::InvalidType
        );
        assert_eq!(
            classify_serde_code("invalid length 2, expected an array of 3 elements"),
            ErrorCode::InvalidType
        );
        assert_eq!(
            classify_serde_code("backwards incompatible something"),
            ErrorCode::Invalid
        );
    }

    /// 高度场：结构（矩形、至少 2×2）、上限、格子边长、刚体类型逐条给稳定的码与路径。
    ///
    /// 上限那条在 e2e 里跑太贵（要写几十万个数的 JSON），所以在这里用**内存里构造**的文档测：
    /// 校验是纯函数，没有理由为了测一个上限去落盘。
    #[test]
    fn speculative_margin_diagnostics_are_specific() {
        let doc = |physics: &str| {
            SceneDoc::from_json(&format!(
                r#"{{ "objects": [ {{ "name": "球", "body": "dynamic", "shape": {{ "type": "sphere" }} }} ],
                     "physics": {physics}, "simulate": {{ "steps": 1 }} }}"#
            ))
            .expect("文档本身要能解析")
        };
        let codes_at = |physics: &str| -> Vec<crate::scene_doc::ErrorCode> {
            doc(physics)
                .validate()
                .iter()
                .filter(|d| {
                    d.path == "physics.speculative_margin"
                        && d.severity == crate::scene_doc::Severity::Error
                })
                .map(|d| d.code)
                .collect()
        };
        // 合法：缺省 / 0 / 厘米量级
        assert!(codes_at("{}").is_empty());
        assert!(codes_at(r#"{ "speculative_margin": 0.0 }"#).is_empty());
        assert!(codes_at(r#"{ "speculative_margin": 0.01 }"#).is_empty());
        // 负数 / NaN / 过大 → out_of_range
        for bad in ["-0.01", "-1"] {
            assert_eq!(
                codes_at(&format!(r#"{{ "speculative_margin": {bad} }}"#)),
                vec![crate::scene_doc::ErrorCode::OutOfRange],
                "余量 {bad}"
            );
        }
        assert_eq!(
            codes_at(r#"{ "speculative_margin": 2.0 }"#),
            vec![crate::scene_doc::ErrorCode::OutOfRange],
            "2 米余量"
        );
        // 偏大但可用 → 只是告警（不阻止运行）
        let warned = doc(r#"{ "speculative_margin": 0.4 }"#).validate();
        assert!(
            warned.iter().any(|d| {
                d.path == "physics.speculative_margin"
                    && d.severity == crate::scene_doc::Severity::Warning
            }),
            "0.4 米该告警：{warned:?}"
        );
        assert!(!SceneDoc::has_errors(&warned));
    }

    #[test]
    fn rounded_box_diagnostics_are_specific() {
        let doc = |json: &str| {
            SceneDoc::from_json(&format!(
                r#"{{ "objects": [ {{ "name": "盒", "body": "dynamic", "shape": {json} }} ],
                    "simulate": {{ "steps": 1 }} }}"#
            ))
            .expect("文档本身要能解析")
        };
        let diags_at = |json: &str, path: &str| -> Vec<crate::scene_doc::ErrorCode> {
            let d = doc(json).validate();
            d.iter()
                .filter(|x| x.path == path && x.severity == crate::scene_doc::Severity::Error)
                .map(|x| x.code)
                .collect()
        };
        // 合法
        assert!(diags_at(
            r#"{ "type": "rounded_box", "size": [1, 1, 1], "radius": 0.1 }"#,
            "objects[0].shape.radius"
        )
        .is_empty());
        // 缺 radius
        let codes = diags_at(
            r#"{ "type": "rounded_box", "size": [1, 1, 1] }"#,
            "objects[0].shape.radius",
        );
        assert_eq!(
            codes,
            vec![crate::scene_doc::ErrorCode::MissingField],
            "缺半径该报 missing_field"
        );
        // 半径非正
        for r in ["0", "-0.1"] {
            let codes = diags_at(
                &format!(r#"{{ "type": "rounded_box", "size": [1, 1, 1], "radius": {r} }}"#),
                "objects[0].shape.radius",
            );
            assert_eq!(
                codes,
                vec![crate::scene_doc::ErrorCode::OutOfRange],
                "半径 {r}"
            );
        }
        // 半径超过最短半边长
        let codes = diags_at(
            r#"{ "type": "rounded_box", "size": [1, 0.4, 1], "radius": 0.3 }"#,
            "objects[0].shape.radius",
        );
        assert_eq!(
            codes,
            vec![crate::scene_doc::ErrorCode::OutOfRange],
            "0.3 > 0.4/2"
        );
        // 边长非正
        let codes = diags_at(
            r#"{ "type": "rounded_box", "size": [1, 0, 1], "radius": 0.1 }"#,
            "objects[0].shape.size",
        );
        assert_eq!(codes, vec![crate::scene_doc::ErrorCode::OutOfRange]);
    }

    #[test]
    fn heightfield_diagnostics_are_specific() {
        // ① 正常：2×3 格点、格子 2 米、静态 —— 一条诊断都不该有
        let ok = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "地形", "body": "static",
                  "shape": { "type": "heightfield", "cell_size": 2,
                             "heights": [[0, 0, 0], [1, 1, 1]] } } ] }"#,
        )
        .unwrap();
        assert!(
            !SceneDoc::has_errors(&ok.validate()),
            "合法高度场不该报错：{:?}",
            ok.validate()
        );

        // ② 只有一行
        let one_row = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "t", "body": "static",
                  "shape": { "type": "heightfield", "heights": [[0, 1]] } } ] }"#,
        )
        .unwrap();
        let diags = one_row.validate();
        let d = diag_at(&diags, "objects[0].shape.heights").expect("一行必须报错");
        assert_eq!(d.code, ErrorCode::OutOfRange);
        assert!(d.message.contains("2×2"), "{}", d.message);

        // ③ 行长不一（不是矩形）
        let ragged = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "t", "body": "static",
                  "shape": { "type": "heightfield", "heights": [[0, 1, 2], [0, 1]] } } ] }"#,
        )
        .unwrap();
        let diags = ragged.validate();
        let d = diag_at(&diags, "objects[0].shape.heights").expect("行长不一要报错");
        assert_eq!(d.code, ErrorCode::InvalidValue);

        // ④ 缺 heights
        let missing = SceneDoc::from_json(
            r#"{ "objects": [ { "name": "t", "body": "static",
                  "shape": { "type": "heightfield" } } ] }"#,
        )
        .unwrap();
        let diags = missing.validate();
        let d = diag_at(&diags, "objects[0].shape.heights").expect("缺字段要报错");
        assert_eq!(d.code, ErrorCode::MissingField);

        // ⑤ 格子边长 0 / 负 / 非有限
        for cell in ["0", "-1"] {
            let bad = SceneDoc::from_json(&format!(
                r#"{{ "objects": [ {{ "name": "t", "body": "static",
                      "shape": {{ "type": "heightfield", "cell_size": {cell},
                                  "heights": [[0, 0], [0, 0]] }} }} ] }}"#
            ))
            .unwrap();
            let diags = bad.validate();
            let d = diag_at(&diags, "objects[0].shape.cell_size").expect("格子边长要报错");
            assert_eq!(d.code, ErrorCode::OutOfRange, "cell_size = {cell}");
        }

        // ⑥ 动态/运动学刚体：三角网没有质量属性，硬写会被引擎静默降级 → 必须报错
        for body in ["dynamic", "kinematic"] {
            let bad = SceneDoc::from_json(&format!(
                r#"{{ "objects": [ {{ "name": "t", "body": "{body}",
                      "shape": {{ "type": "heightfield", "heights": [[0, 0], [0, 0]] }} }} ] }}"#
            ))
            .unwrap();
            let diags = bad.validate();
            let d = diag_at(&diags, "objects[0].body")
                .unwrap_or_else(|| panic!("body = {body} 必须报错"));
            assert_eq!(d.code, ErrorCode::InvalidValue);
            assert!(d.hint.as_deref().unwrap_or("").contains("static"), "{d:?}");
        }

        // ⑦ 格点数超上限（内存里构造：2 行 × 32769 列 = 65538 > 65536）
        let mut doc = SceneDoc::default();
        doc.objects.push(
            serde_json::from_value(json!({
                "name": "巨地形",
                "body": "static",
                "shape": { "type": "heightfield" }
            }))
            .unwrap(),
        );
        doc.objects[0]
            .shape
            .as_mut()
            .expect("上面 json 里写了 shape")
            .heights = Some(vec![vec![0.0f32; MAX_HEIGHTFIELD_POINTS / 2 + 1]; 2]);
        let diags = doc.validate();
        let d = diag_at(&diags, "objects[0].shape.heights").expect("超上限要报错");
        assert_eq!(d.code, ErrorCode::LimitExceeded);

        // ⑧ 高度里有 NaN：直接构造结构体（JSON 里写不出 NaN）
        let mut bad = SceneDoc::default();
        bad.objects.push(
            serde_json::from_value(json!({
                "name": "坏地形",
                "body": "static",
                "shape": { "type": "heightfield", "heights": [[0, 0], [0, 0]] }
            }))
            .unwrap(),
        );
        bad.objects[0]
            .shape
            .as_mut()
            .expect("上面 json 里写了 shape")
            .heights = Some(vec![vec![0.0, f32::NAN], vec![0.0, 0.0]]);
        let diags = bad.validate();
        let d = diag_at(&diags, "objects[0].shape.heights").expect("NaN 高度要报错");
        assert_eq!(d.code, ErrorCode::OutOfRange);
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

#[cfg(test)]
mod include_tests {
    use super::*;

    // -----------------------------------------------------------------------
    // include：跨文件合并（格式层）
    // -----------------------------------------------------------------------

    /// 写一个临时场景目录：`files` 是 `相对路径 → 文件内容`（父目录自动建）。
    fn temp_scene_dir(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("quench3d_inc_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (rel, text) in files {
            let p = dir.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, text).unwrap();
        }
        dir
    }

    fn resolved_of(path: &Path) -> ResolvedDoc {
        SceneDoc::from_file(path)
            .expect("根场景要能解析")
            .resolved()
    }

    /// 合并顺序、具名表合并、根独有段保留、**片段资源路径重定基**、幂等、校验通过。
    #[test]
    fn include_merges_fragments_in_order_and_rebases_fragment_asset_paths() {
        let dir = temp_scene_dir(
            "merge",
            &[
                (
                    "main.json",
                    r#"{
                      "format": "quench3d.scene", "version": 2,
                      "include": ["parts/wall.json"],
                      "ground": { "size": 20 },
                      "objects": [ { "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                                     "position": [0, 0.5, 0], "body": "dynamic" } ],
                      "lighting": { "preset": "day" }
                    }"#,
                ),
                (
                    "parts/wall.json",
                    r#"{
                      "format": "quench3d.scene", "version": 2,
                      "materials": { "砖": { "color": [0.6, 0.3, 0.25],
                                             "base_color_texture": "tex/brick.png" } },
                      "templates": { "砖块": { "shape": { "type": "box", "size": [1, 1, 1] },
                                               "body": "static", "material": "砖" } },
                      "objects": [ { "name": "墙", "template": "砖块", "position": [2, 0.5, 0] } ]
                    }"#,
                ),
                ("parts/tex/brick.png", "这不是真的 PNG，只要求文件存在"),
            ],
        );
        let doc = SceneDoc::from_file(dir.join("main.json")).unwrap();
        let resolved = doc.resolved();
        assert!(!resolved.has_errors(), "{:?}", resolved.diagnostics);
        let merged = resolved.doc.expect("合并必须成功");
        // 顺序：本文件自己的内容在前，被 include 的在后
        let names: Vec<&str> = merged
            .objects
            .iter()
            .map(|o| o.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(names, vec!["球", "墙"]);
        // 具名表合并
        assert!(merged.materials.contains_key("砖"));
        assert!(merged.templates.contains_key("砖块"));
        // 根独有的段按根的值保留
        assert_eq!(merged.lighting.preset.as_deref(), Some("day"));
        assert_eq!(merged.ground.as_ref().map(|g| g.size), Some(20.0));
        // 片段里的相对贴图路径按**片段自己的目录**重定基
        assert_eq!(
            merged.materials["砖"].base_color_texture.as_deref(),
            Some("parts/tex/brick.png"),
            "片段里的 tex/brick.png 应当被重定基成相对根场景目录的路径"
        );
        // 合并结果里 include 必须是空的，否则会再并一遍（静默的数据繁殖）
        assert!(merged.include.is_empty());
        // 幂等：对合并结果再解析一次不该有变化
        let again = merged.resolved();
        assert!(!again.has_errors());
        assert_eq!(again.doc.expect("幂等").objects.len(), 2);
        // 合并后的文档必须真的能过校验（贴图按重定基后的路径找得到）
        let diags = doc.validate();
        assert!(!SceneDoc::has_errors(&diags), "{diags:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 嵌套 include 的相对路径以**片段自己的目录**为基准（不是根目录）。
    #[test]
    fn nested_include_paths_are_relative_to_the_fragment_that_writes_them() {
        let dir = temp_scene_dir(
            "nested",
            &[
                ("main.json", r#"{ "include": ["parts/outer.json"] }"#),
                ("parts/outer.json", r#"{ "include": ["inner.json"] }"#),
                (
                    "parts/inner.json",
                    r#"{ "objects": [ { "name": "内层", "shape": { "type": "box", "size": [1,1,1] } } ] }"#,
                ),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(!resolved.has_errors(), "{:?}", resolved.diagnostics);
        let merged = resolved.doc.expect("嵌套包含必须成功");
        let names: Vec<&str> = merged
            .objects
            .iter()
            .map(|o| o.name.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(names, vec!["内层"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 片段写 `assets` 是错误（根独有的段），并且**不改动**根自己的搜索路径。
    #[test]
    fn fragment_assets_section_is_rejected() {
        let dir = temp_scene_dir(
            "assets",
            &[
                (
                    "main.json",
                    r#"{ "assets": { "search_paths": ["maps"] }, "include": ["parts/tex.json"] }"#,
                ),
                (
                    "parts/tex.json",
                    r#"{ "assets": { "search_paths": ["../shared"] } }"#,
                ),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(resolved.doc.is_none());
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| { d.code == ErrorCode::InvalidValue && d.path == "include[0].assets" }),
            "{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 五类失败路径各自的码与路径：缺失、根独有段、重名、成环、深度上限；外加菱形包含的警告。
    #[test]
    fn include_reports_missing_root_only_conflicts_cycles_and_depth() {
        // ① 文件缺失
        let dir = temp_scene_dir("miss", &[("main.json", r#"{ "include": ["nope.json"] }"#)]);
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(resolved.doc.is_none(), "缺失文件必须失败");
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| d.code == ErrorCode::AssetNotFound && d.path == "include[0]"),
            "{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);

        // ② 片段写了根独有的段
        let dir = temp_scene_dir(
            "rootonly",
            &[
                ("main.json", r#"{ "include": ["frag.json"] }"#),
                ("frag.json", r#"{ "lighting": { "preset": "day" } }"#),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| { d.code == ErrorCode::InvalidValue && d.path == "include[0].lighting" }),
            "{:?}",
            resolved.diagnostics
        );
        assert!(resolved.doc.is_none());
        let _ = std::fs::remove_dir_all(&dir);

        // ②b `water` 也是根独有：片段里写必须报错，不能静默按"根赢"。
        let dir = temp_scene_dir(
            "rootonly-water",
            &[
                ("main.json", r#"{ "include": ["frag.json"] }"#),
                ("frag.json", r#"{ "water": { "clarity": 0.5 } }"#),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| { d.code == ErrorCode::InvalidValue && d.path == "include[0].water" }),
            "{:?}",
            resolved.diagnostics
        );
        assert!(resolved.doc.is_none());
        let _ = std::fs::remove_dir_all(&dir);

        // ③ 材质重名（静默覆盖是本仓库最在意的那类坑）
        let dir = temp_scene_dir(
            "dup",
            &[
                (
                    "main.json",
                    r#"{ "materials": { "木": { "color": [1,0,0] } },
                        "include": ["frag.json"] }"#,
                ),
                (
                    "frag.json",
                    r#"{ "materials": { "木": { "color": [0,1,0] } } }"#,
                ),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(
            resolved.diagnostics.iter().any(|d| {
                d.code == ErrorCode::DuplicateName && d.path == "include[0].materials.木"
            }),
            "{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);

        // ④ 成环（A → B → A）
        let dir = temp_scene_dir(
            "cycle",
            &[
                ("a.json", r#"{ "include": ["b.json"] }"#),
                ("b.json", r#"{ "include": ["a.json"] }"#),
            ],
        );
        let resolved = resolved_of(&dir.join("a.json"));
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| d.code == ErrorCode::IncludeCycle),
            "{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);

        // ⑤ 深度上限（19 层 > MAX_INCLUDE_DEPTH = 16）
        let mut files: Vec<(String, String)> = Vec::new();
        for i in 0..19 {
            let body = if i == 18 {
                r#"{ "objects": [ { "name": "底", "shape": { "type": "box", "size": [1,1,1] } } ] }"#
                    .to_string()
            } else {
                format!(r#"{{ "include": ["lvl{}.json"] }}"#, i + 1)
            };
            files.push((format!("lvl{i}.json"), body));
        }
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let dir = temp_scene_dir("deep", &refs);
        let resolved = resolved_of(&dir.join("lvl0.json"));
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| d.code == ErrorCode::LimitExceeded),
            "超过 {MAX_INCLUDE_DEPTH} 层应当报 limit_exceeded：{:?}",
            resolved.diagnostics
        );
        let _ = std::fs::remove_dir_all(&dir);

        // ⑥ 菱形包含：同一个文件被包含两次 → 跳过第二次并给一条 `ineffective` 警告
        let dir = temp_scene_dir(
            "diamond",
            &[
                ("main.json", r#"{ "include": ["a.json", "b.json"] }"#),
                ("a.json", r#"{ "include": ["common.json"] }"#),
                ("b.json", r#"{ "include": ["common.json"] }"#),
                (
                    "common.json",
                    r#"{ "objects": [ { "name": "公共", "shape": { "type": "box", "size": [1,1,1] } } ] }"#,
                ),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        assert!(!resolved.has_errors(), "{:?}", resolved.diagnostics);
        assert!(
            resolved
                .diagnostics
                .iter()
                .any(|d| d.code == ErrorCode::Ineffective),
            "菱形包含应当给一条 ineffective 警告：{:?}",
            resolved.diagnostics
        );
        assert_eq!(
            resolved.doc.expect("合并成功").objects.len(),
            1,
            "公共片段只该被并入一次"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 有 `include` 时报错的诊断要挂到**引用点**上（`include[0]:...`），
    /// 而不是让人去猜是哪一份片段里的 `objects[3]`。
    #[test]
    fn fragment_content_errors_are_labelled_with_the_referencing_include() {
        let dir = temp_scene_dir(
            "frag_err",
            &[
                ("main.json", r#"{ "include": ["frag.json"] }"#),
                (
                    "frag.json",
                    r#"{ "objects": [ { "name": "坏的", "shape": { "type": "pyramid" } } ] }"#,
                ),
            ],
        );
        let resolved = resolved_of(&dir.join("main.json"));
        let diag = resolved
            .diagnostics
            .iter()
            .find(|d| d.code == ErrorCode::InvalidValue)
            .unwrap_or_else(|| panic!("应当有 invalid_value：{:?}", resolved.diagnostics));
        assert_eq!(diag.path, "include[0]:objects[0].shape.type");
        assert!(
            diag.hint.as_deref().unwrap_or("").contains("frag.json"),
            "提示里要点名是哪份文件：{diag:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **不写 `include` = 老行为逐字节不变**：没有 include 时 `resolved()` 是纯克隆，
    /// 序列化结果一个字都不差（新字段有 `skip_serializing_if`）。
    #[test]
    fn scenes_without_include_are_byte_identical() {
        let json = r#"{ "name": "老场景", "ground": { "size": 10 },
                        "objects": [ { "name": "箱", "shape": { "type": "box", "size": [1,1,1] },
                                       "position": [0, 0.5, 0], "body": "dynamic" } ],
                        "simulate": { "steps": 30 } }"#;
        let doc = SceneDoc::from_json(json).unwrap();
        let resolved = doc.resolved();
        assert!(resolved.diagnostics.is_empty());
        let merged = resolved.doc.expect("没有 include 时必然成功");
        assert_eq!(merged.to_json_pretty(), doc.to_json_pretty());
        assert!(
            !doc.to_json_pretty().contains("include"),
            "不写就不该出现这个键"
        );
    }
}

#[cfg(test)]
mod water_style_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn water_style_roundtrips_and_rejects_bad_values() {
        let doc = SceneDoc::from_value(json!({
            "waters": [{ "size": [4.0, 4.0], "level": 0.0 }],
            "water": {
                "color": [0.1, 0.3, 0.5],
                "clarity": 0.75,
                "wave_strength": 1.5,
                "foam": 1.2,
                "refraction": 0.9
            }
        }))
        .expect("结构合法");
        let style = doc.water.as_ref().expect("water 应当解析");
        assert_eq!(style.color, Some([0.1, 0.3, 0.5]));
        assert_eq!(style.clarity, Some(0.75));
        assert_eq!(style.wave_strength, Some(1.5));
        assert_eq!(style.foam, Some(1.2));
        assert_eq!(style.refraction, Some(0.9));
        let again = SceneDoc::from_json(&doc.to_json_pretty()).expect("往返");
        assert_eq!(again.water.unwrap().wave_strength, Some(1.5));

        for (water, path) in [
            (json!({"color": [1.2, 0.0, 0.0]}), "water.color"),
            (json!({"clarity": -0.1}), "water.clarity"),
            (json!({"wave_strength": 5.0}), "water.wave_strength"),
            (json!({"foam": 3.0}), "water.foam"),
            (json!({"refraction": -1.0}), "water.refraction"),
        ] {
            let doc = SceneDoc::from_value(json!({ "water": water })).expect("结构合法");
            let diags = doc.validate();
            assert!(
                diags.iter().any(|d| d.path == path),
                "期望 {path} 被挡下，实际: {:?}",
                diags
                    .iter()
                    .map(|d| (&d.path, &d.message))
                    .collect::<Vec<_>>()
            );
        }
    }
}
