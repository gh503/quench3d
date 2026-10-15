//! 确定性 island（连通分量）划分。
//!
//! 把刚体按「接触 + 关节」的连通性切成互不相交的连通分量。用途：
//!
//! * **诊断**：告诉使用者和 agent「这个场景能不能靠并行求解变快」——
//!   2000 个互不相干的箱子是 2000 个岛（并行友好），
//!   一个 2000 体的深堆是 1 个岛（并行完全没用）。
//! * **并行求解的前提**：岛与岛之间不共享刚体，因此在岛内保持原有接触顺序、
//!   岛间并行，结果与串行**逐位相同**。详见 `docs/island_parallel_plan.md`。
//! * 以后整岛休眠/唤醒也建立在这上面。
//!
//! 只有**清醒的动态体**参与划分：静态体和睡着的体不会被求解器写状态
//! （`inv_m == 0`，逆惯量为零矩阵），它们可以同时出现在多个岛的接触里而互不影响，
//! 所以「所有箱子都压在同一块静态地板上」不会把整个世界并成一个岛。
//!
//! 全部缓冲跨步复用，稳态下零堆分配；岛的编号按「岛内最小刚体下标」升序，
//! 与哈希遍历顺序无关 → 可复现。

use crate::solver::BodyView;

/// 岛划分的统计结果（供基准、CLI 与测试观察）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IslandStats {
    /// 岛的总数。
    pub islands: u32,
    /// 参与划分的清醒动态体数量。
    pub awake_bodies: u32,
    /// 最大岛包含的刚体数。
    pub largest: u32,
    /// 含两个及以上刚体的岛（只有这些岛并行才有意义）。
    pub multi_body_islands: u32,
}

impl IslandStats {
    /// 是否值得走并行求解路径（至少两个多体岛）。
    pub fn parallelizable(&self) -> bool {
        self.multi_body_islands >= 2
    }
}

/// 岛划分器（缓冲跨步复用）。
#[derive(Clone, Debug, Default)]
pub(crate) struct Islands {
    /// 并查集父指针。
    parent: Vec<u32>,
    /// 并查集秩（按秩合并，避免退化成链表）。
    rank: Vec<u8>,
    /// 该槽位是否清醒动态体。
    awake: Vec<bool>,
    /// 刚体槽位 → 岛编号（`u32::MAX` 表示不参与划分）。
    island_of: Vec<u32>,
    /// 根节点 → 岛编号（划分过程中的临时标记）。
    root_id: Vec<u32>,
    /// 展平的「岛 → 刚体槽位」，每个岛内按槽位升序。
    bodies: Vec<u32>,
    /// 岛 → `bodies` 中的区间 `[start, end)`。
    ranges: Vec<(u32, u32)>,
    /// 计数缓冲（前缀和用）。
    counts: Vec<u32>,
    /// CSR 填充时的写指针（复用，避免每步分配）。
    write: Vec<u32>,
    /// 本次划分的统计。
    stats: IslandStats,
}

impl Islands {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 开始一轮划分：按 `views` 判定哪些刚体是清醒动态体。
    pub fn begin(&mut self, views: &[BodyView]) {
        let n = views.len();
        self.parent.clear();
        self.parent.extend(0..n as u32);
        self.rank.clear();
        self.rank.resize(n, 0);
        self.awake.clear();
        self.awake
            .extend(views.iter().map(|v| v.inv_m > 0.0 && v.inv_m.is_finite()));
        self.island_of.clear();
        self.island_of.resize(n, u32::MAX);
        self.stats = IslandStats::default();
    }

    /// 合并一条约束（接触或关节）连接的两个刚体。
    ///
    /// 只有两端都是清醒动态体时才真正合并——静态体与睡着的体是只读的，
    /// 它们不该把不同的岛粘在一起。
    pub fn union_pair(&mut self, a: usize, b: usize) {
        if a == b || a >= self.awake.len() || b >= self.awake.len() {
            return;
        }
        if !self.awake[a] || !self.awake[b] {
            return;
        }
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        // 按秩合并；秩相同时挂到下标较小的一侧，保证结构只依赖输入顺序
        match self.rank[ra].cmp(&self.rank[rb]) {
            core::cmp::Ordering::Less => self.parent[ra] = rb as u32,
            core::cmp::Ordering::Greater => self.parent[rb] = ra as u32,
            core::cmp::Ordering::Equal => {
                self.parent[rb] = ra as u32;
                self.rank[ra] = self.rank[ra].saturating_add(1);
            }
        }
    }

    /// 结束一轮划分：编号、建 CSR、算统计。返回统计结果。
    pub fn finish(&mut self) -> IslandStats {
        let n = self.parent.len();
        self.root_id.clear();
        self.root_id.resize(n, u32::MAX);
        // 岛编号按「岛内最小刚体下标」升序分配 → 与遍历顺序无关，可复现
        let mut next: u32 = 0;
        for i in 0..n {
            if !self.awake[i] {
                continue;
            }
            let root = self.find(i);
            if self.root_id[root] == u32::MAX {
                self.root_id[root] = next;
                next += 1;
            }
            self.island_of[i] = self.root_id[root];
        }
        // CSR：岛 → 刚体槽位（岛内升序，因为外层按下标升序扫描）
        self.counts.clear();
        self.counts.resize(next as usize, 0);
        let mut awake_bodies = 0u32;
        for i in 0..n {
            let id = self.island_of[i];
            if id != u32::MAX {
                self.counts[id as usize] += 1;
                awake_bodies += 1;
            }
        }
        self.ranges.clear();
        self.bodies.clear();
        let mut start = 0u32;
        for c in self.counts.iter() {
            self.ranges.push((start, start + *c));
            start += *c;
        }
        self.bodies.resize(start as usize, 0);
        // 写指针从各区间的起点开始（复用缓冲，稳态零分配）
        self.write.clear();
        self.write.extend(self.ranges.iter().map(|r| r.0));
        for i in 0..n {
            let id = self.island_of[i];
            if id != u32::MAX {
                let w = &mut self.write[id as usize];
                self.bodies[*w as usize] = i as u32;
                *w += 1;
            }
        }
        // 统计
        let mut largest = 0u32;
        let mut multi = 0u32;
        for r in self.ranges.iter() {
            let len = r.1 - r.0;
            largest = largest.max(len);
            if len >= 2 {
                multi += 1;
            }
        }
        self.stats = IslandStats {
            islands: next,
            awake_bodies,
            largest,
            multi_body_islands: multi,
        };
        self.stats
    }

    /// 上一次划分的统计。
    pub fn stats(&self) -> IslandStats {
        self.stats
    }

    /// 刚体槽位所属的岛编号；静态体与睡着的体返回 `None`。
    pub fn island_of(&self, body: usize) -> Option<u32> {
        match self.island_of.get(body) {
            Some(&id) if id != u32::MAX => Some(id),
            _ => None,
        }
    }

    /// 岛的数量。
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// 某个岛包含的刚体槽位（升序）。
    pub fn bodies_of(&self, island: u32) -> &[u32] {
        match self.ranges.get(island as usize) {
            Some(&(s, e)) => &self.bodies[s as usize..e as usize],
            None => &[],
        }
    }

    /// 路径压缩查找（迭代写法，避免深堆场景下递归爆栈）。
    fn find(&mut self, mut i: usize) -> usize {
        // 先找到根
        let mut root = i;
        while self.parent[root] as usize != root {
            root = self.parent[root] as usize;
        }
        // 再压平路径
        while self.parent[i] as usize != root {
            let next = self.parent[i] as usize;
            self.parent[i] = root as u32;
            i = next;
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个「清醒动态体」视图（`inv_m > 0`）。
    fn awake() -> BodyView {
        let mut v = BodyView::frozen();
        v.inv_m = 1.0;
        v
    }

    #[test]
    fn separate_clusters_get_separate_islands() {
        let views = [awake(), awake(), awake(), awake(), awake(), awake()];
        let mut isl = Islands::new();
        isl.begin(&views);
        // 两个互不相干的三角形
        isl.union_pair(0, 1);
        isl.union_pair(1, 2);
        isl.union_pair(3, 4);
        isl.union_pair(4, 5);
        let stats = isl.finish();
        assert_eq!(stats.islands, 2);
        assert_eq!(stats.awake_bodies, 6);
        assert_eq!(stats.largest, 3);
        assert_eq!(isl.bodies_of(0), &[0, 1, 2]);
        assert_eq!(isl.bodies_of(1), &[3, 4, 5]);
        assert_eq!(isl.island_of(4), Some(1));
        assert!(stats.parallelizable(), "两个多体岛应该判定为可并行");
    }

    #[test]
    fn island_numbering_follows_smallest_body_index() {
        let views = [awake(), awake(), awake(), awake()];
        let mut isl = Islands::new();
        isl.begin(&views);
        // 先声明靠后的那一对：编号仍应按最小下标（0 所在岛优先）
        isl.union_pair(2, 3);
        isl.union_pair(0, 1);
        isl.finish();
        assert_eq!(isl.island_of(0), Some(0));
        assert_eq!(isl.island_of(2), Some(1));
    }

    #[test]
    fn static_and_sleeping_bodies_do_not_merge_islands() {
        // 0、1 清醒；2 是静态（inv_m == 0）；3 是清醒
        let mut views = vec![awake(), awake(), BodyView::frozen(), awake()];
        views[2].inv_m = 0.0;
        let mut isl = Islands::new();
        isl.begin(&views);
        // 两个清醒体都压在同一个静态地板上：不该并成一个岛
        isl.union_pair(0, 2);
        isl.union_pair(1, 2);
        isl.union_pair(1, 3);
        let stats = isl.finish();
        assert_eq!(stats.islands, 2, "静态体不该把岛粘起来");
        assert_eq!(isl.bodies_of(0), &[0]);
        assert_eq!(isl.bodies_of(1), &[1, 3]);
        assert_eq!(isl.island_of(2), None, "静态体不属于任何岛");
    }

    #[test]
    fn joints_merge_bodies_across_contacts() {
        let views = [awake(), awake(), awake(), awake()];
        let mut isl = Islands::new();
        isl.begin(&views);
        isl.union_pair(0, 1); // 接触把 0-1 连起来
        isl.union_pair(1, 2); // 关节把 1-2 连起来 → 0,1,2 同一个岛
        let stats = isl.finish();
        assert_eq!(stats.islands, 2); // {0,1,2} 与 {3}
        assert_eq!(stats.largest, 3);
        assert_eq!(stats.multi_body_islands, 1);
        assert!(!stats.parallelizable());
    }

    #[test]
    fn rebuild_is_reproducible_and_reuses_buffers() {
        let views = [awake(), awake(), awake(), awake()];
        let mut isl = Islands::new();
        let mut snapshot = Vec::new();
        for _ in 0..3 {
            isl.begin(&views);
            isl.union_pair(0, 1);
            isl.finish();
            let now: Vec<u32> = (0..views.len())
                .map(|i| isl.island_of(i).unwrap_or(u32::MAX))
                .collect();
            if snapshot.is_empty() {
                snapshot = now;
            } else {
                assert_eq!(now, snapshot, "同样输入必须得到同样的划分");
            }
        }
        assert_eq!(isl.len(), 3);
    }

    #[test]
    fn degenerate_inputs_are_ignored() {
        let views = [awake(), BodyView::frozen()];
        let mut isl = Islands::new();
        isl.begin(&views);
        isl.union_pair(0, 0); // 自环
        isl.union_pair(0, 9); // 越界
        isl.union_pair(0, 1); // 一端静态
        let stats = isl.finish();
        assert_eq!(stats.islands, 1);
        assert_eq!(stats.largest, 1);
        assert!(!stats.parallelizable());
    }
}
