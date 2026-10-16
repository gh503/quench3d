//! Quench3D 物理引擎 —— 完全自研的确定性冲量法刚体动力学。
//!
//! 模块划分：
//! * [`shape`]：形状、支持函数与质量属性；
//! * [`body`]：刚体、世代句柄与构建器；
//! * [`contact`]：接触点与接触流形；
//! * [`broadphase`]：空间哈希宽相；
//! * [`narrowphase`]：SAT / 解析 / GJK+EPA / 三角网格窄相；
//! * [`solver`]：顺序冲量求解器（热启动 + 分裂冲量 + 睡眠）；
//! * [`joints`]：固定 / 球铰 / 铰链 / 距离 / 滑块约束；
//! * [`world`]：物理世界与固定步长主循环；
//! * [`SmallVec`]：内联小向量（热路径零分配的关键工具）；
//! * [`query`]：射线 / 任意形状重叠 / 形状投射 + [`QueryFilter`]；
//! * [`query_index`]：查询加速结构（快照式空间哈希网格）；
//! * [`character`]：角色控制器（移动并滑动）。

#![forbid(unsafe_code)]

#[cfg(test)]
mod api_tests;
mod body;
mod broadphase;
mod character;
mod contact;
#[cfg(test)]
mod digest_tests;
mod gjk;
mod island;
mod joints;
mod narrowphase;
#[cfg(not(target_arch = "wasm32"))]
mod parallel;
mod query;
mod query_index;
mod shape;
#[cfg(test)]
mod sim_tests;
mod small_vec;
mod solver;
mod world;

pub use crate::island::IslandStats;
pub use body::*;
/// `broadphase` 内部类型不公开，这里保留扁平重导出以统一模块风格。
#[allow(unused_imports)]
pub use broadphase::*;
pub use character::*;
pub use contact::*;
pub use joints::*;
/// `narrowphase` 仅通过 [`PhysicsWorld`] 对外提供能力，无公开类型。
#[allow(unused_imports)]
pub use narrowphase::*;
pub use query::*;
pub use query_index::*;
pub use shape::*;
pub use small_vec::SmallVec;
pub use solver::*;
pub use world::*;

use quench3d_math::{vec3, Vec3};

/// 引擎默认重力（米/秒²）。
pub const GRAVITY: Vec3 = vec3(0.0, -9.81, 0.0);
