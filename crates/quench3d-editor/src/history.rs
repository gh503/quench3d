//! 编辑历史（撤销 / 重做）。
//!
//! **为什么用"整场景快照"而不是"命令对象"**：这个编辑器的场景状态本来就有一个
//! 无损的序列化形式（`quench3d.scene`，与 CLI 共用），而"重建场景"的代码路径
//! 已经存在且被往返测试钉着（[`crate::scene_io::load_into_engine`]）。
//! 命令对象要针对每一种编辑各写一份 do/undo，才是真正容易出错的地方
//! （删物体撤销要恢复刚体句柄、复制撤销要恢复名字计数……）。
//! 快照法的代价是每步都存一份 JSON（默认场景约几 KB，上限 128 步），
//! 换来的是"撤销一定能回到当时的样子"。
//!
//! **明确的边界**（说清楚比假装完整好）：
//! * 撤销恢复的是**场景内容**：物体、名字、变换、可见性、材质参数、刚体类型与参数、
//!   灯光、相机文档字段。它**不**恢复"物理这一步跑到哪儿"（速度、睡眠、接触缓存）——
//!   那些不在场景格式里。暂停状态下编辑/撤销都是精确的；运行中撤销相当于
//!   "把场景重置成当时的样子，物理从这里继续"。
//! * 撤销**不动你的视角**：载入快照会把文档里的相机装回去，这里会立刻把你当前的
//!   轨道相机放回去（编辑器里最不意外的行为，和 Blender 一致）。
//! * 选区只在自己越界时收敛：撤销后原来选中的下标还有效就保持选中。
//!
//! 语义约定（写清楚省得以后自己搞混）：`past` 里存的是**曾经出现过的状态**，
//! 且**栈顶就是当前状态**。所以每次编辑之后 `record()` 一次；
//! 撤销 = 把栈顶挪进 `future`，再回到新的栈顶。

use quench3d::Engine;
use quench3d_cli::runner::ModelInstance;

use crate::scene_io;

/// 默认保留多少步历史。
pub const DEFAULT_CAPACITY: usize = 128;

/// 撤销 / 重做成功后的结果。
pub struct Restored {
    /// 装回去之后的物体数。
    pub objects: usize,
    /// 刚装回去的那份快照（调用方据此同步自己的底稿文档）。
    pub snapshot: String,
    /// 该快照重新展开出的模型实例映射。
    ///
    /// 必须跟着刷新：撤销一次"加物体"会让后面的物体下标整体前移，
    /// 老映射会把模型面板/整组拖动指到别的物体上。
    pub instances: Vec<ModelInstance>,
}

/// 一步历史：快照 + 给面板看的标签。
#[derive(Clone)]
struct Entry {
    json: String,
    label: String,
}

/// 编辑历史。
pub struct History {
    past: Vec<Entry>,
    future: Vec<Entry>,
    capacity: usize,
    /// 拖拽/连续编辑期间为 true：这段时间里的 `record` 不再入栈，
    /// 整段拖拽只算**一步**（不然拖一次鼠标就能塞满历史）。
    in_transaction: bool,
    /// 事务期间是否真的改过东西（没改就不留历史）。
    dirty_in_transaction: bool,
}

impl Default for History {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl History {
    /// 指定上限（至少 2：只有一步的历史没有意义）。
    pub fn new(capacity: usize) -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
            capacity: capacity.max(2),
            in_transaction: false,
            dirty_in_transaction: false,
        }
    }

    /// 以给定快照为起点重开历史（打开文件 / 装载新场景 / 建默认场景之后调用）。
    pub fn reset_json(&mut self, json: Option<String>) {
        self.past.clear();
        self.future.clear();
        self.in_transaction = false;
        self.dirty_in_transaction = false;
        if let Some(json) = json {
            self.past.push(Entry {
                json,
                label: "起始状态".to_string(),
            });
        }
    }

    /// 记一步（`json` = **编辑之后**的场景快照）。与栈顶相同（无变化）时不记录。
    pub fn record_json(&mut self, json: Option<String>) {
        self.record_json_labeled(json, None);
    }

    /// 同 [`History::record_json`]，但给这一步一个面板里能看懂的标签。
    pub fn record_json_labeled(&mut self, json: Option<String>, label: Option<String>) {
        if self.in_transaction {
            self.dirty_in_transaction = true;
            return;
        }
        let Some(json) = json else { return };
        if self.past.last().is_some_and(|entry| entry.json == json) {
            return;
        }
        self.past.push(Entry {
            json,
            label: label.unwrap_or_else(|| "编辑".to_string()),
        });
        if self.past.len() > self.capacity {
            let drop = self.past.len() - self.capacity;
            self.past.drain(0..drop);
        }
        self.future.clear();
    }

    /// 开始一段连续编辑（拖 gizmo、拖数值框）。期间的 `record` 折叠成一步。
    pub fn begin_transaction(&mut self) {
        if self.in_transaction {
            return; // 已经开着就别把"改过"的标志清掉
        }
        self.in_transaction = true;
        self.dirty_in_transaction = false;
    }

    /// 结束连续编辑：期间真的改过才入栈。
    pub fn end_transaction_json(&mut self, json: Option<String>) {
        if !self.in_transaction {
            return;
        }
        self.in_transaction = false;
        let dirty = self.dirty_in_transaction;
        self.dirty_in_transaction = false;
        if dirty {
            self.record_json(json);
        }
    }

    /// 放弃当前事务（拖拽中途按 Esc）：既不提交也不留历史。
    /// 调用方负责把物体还原到按下时的状态。
    pub fn abort_transaction(&mut self) {
        self.in_transaction = false;
        self.dirty_in_transaction = false;
    }

    /// 是否处在连续编辑中（`record` 当前会被折叠）。
    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// 能撤销吗。
    pub fn can_undo(&self) -> bool {
        self.past.len() >= 2
    }

    /// 能重做吗。
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// 历史步数（含当前状态）。
    pub fn len(&self) -> usize {
        self.past.len()
    }

    /// 第 `index` 步的标签（0 是最旧）。
    pub fn label(&self, index: usize) -> Option<&str> {
        self.past.get(index).map(|entry| entry.label.as_str())
    }

    /// 当前状态是不是最后一步。
    pub fn is_current(&self, index: usize) -> bool {
        self.past.len().saturating_sub(1) == index
    }

    /// 只有当前状态、且没有可重做的，就算"空历史"。
    pub fn is_empty(&self) -> bool {
        self.past.len() <= 1 && self.future.is_empty()
    }

    /// 撤销一步：`None` = 没有可撤销的；`Some(Ok(..))` = 恢复成功
    /// （调用方可以据此同步自己的底稿文档与模型实例映射）；
    /// `Some(Err(理由))` = 快照装载失败（历史状态保持不变）。
    pub fn undo(&mut self, engine: &mut Engine) -> Option<Result<Restored, String>> {
        if !self.can_undo() {
            return None;
        }
        let current = self.past.pop()?;
        let target = self.past.last().cloned()?;
        match restore(engine, &target.json) {
            Ok((objects, instances)) => {
                self.future.push(current);
                Some(Ok(Restored {
                    objects,
                    snapshot: target.json,
                    instances,
                }))
            }
            Err(e) => {
                self.past.push(current); // 装不回去就当作没撤销过
                Some(Err(e))
            }
        }
    }

    /// 重做一步。语义同 [`History::undo`]。
    pub fn redo(&mut self, engine: &mut Engine) -> Option<Result<Restored, String>> {
        let target = self.future.pop()?;
        match restore(engine, &target.json) {
            Ok((objects, instances)) => {
                self.past.push(target.clone());
                Some(Ok(Restored {
                    objects,
                    snapshot: target.json,
                    instances,
                }))
            }
            Err(e) => {
                self.future.push(target);
                Some(Err(e))
            }
        }
    }

    /// 直接跳回过去第 `index` 步（0 是最旧）；被跳过的步骤进入重做链。
    ///
    /// 返回语义同 [`History::undo`]：`None` = 已经是当前步/越界。
    pub fn jump_to(
        &mut self,
        index: usize,
        engine: &mut Engine,
    ) -> Option<Result<Restored, String>> {
        if index + 1 >= self.past.len() {
            return None;
        }
        let target = self.past[index].json.clone();
        let removed: Vec<Entry> = self.past.drain(index + 1..).collect();
        match restore(engine, &target) {
            Ok((objects, instances)) => {
                self.future.clear();
                self.future.extend(removed.into_iter().rev());
                Some(Ok(Restored {
                    objects,
                    snapshot: target,
                    instances,
                }))
            }
            Err(e) => {
                self.past.extend(removed);
                Some(Err(e))
            }
        }
    }
}

/// 把快照装回引擎。视角（轨道相机）与暂停状态保持不动。
///
/// 返回 `(物体数, 模型实例映射)`：映射必须一路传出去，见 [`Restored::instances`]。
fn restore(engine: &mut Engine, snapshot: &str) -> Result<(usize, Vec<ModelInstance>), String> {
    let orbit = engine.orbit;
    let paused = engine.paused;
    match scene_io::install_text(engine, snapshot) {
        Ok(installed) => {
            engine.orbit = orbit;
            engine.paused = paused;
            Ok((installed.objects, installed.instances))
        }
        Err(e) => Err(e),
    }
}
