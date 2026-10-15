//! island 并行的接触求解（仅 native；wasm 走单线程路径）。
//!
//! # 为什么并行不改变结果
//!
//! 顺序冲量求解器（Gauss-Seidel）的结果取决于**每个刚体经历的接触处理顺序**。
//! island 划分保证：
//!
//! * 岛与岛之间**不共享清醒刚体**：静态体、运动学体和睡着的体不会出现在任何岛里，
//!   而它们在求解中只会被读（`apply_impulse` 对 `inv_m == 0` 的视图直接跳过），
//!   所以一个冻结体可以同时出现在多个岛的接触里而互不影响；
//! * 岛内接触保持**全局顺序**（分组用的是稳定分桶），所以每个刚体看到的
//!   接触顺序与串行完全一致；
//! * 关节不参与并行（接触与关节在每个迭代里本来就是两趟：先全部接触、再全部关节），
//!   关节串行求解不会改变任何刚体看到的顺序。
//!
//! 于是并行版本与串行版本**逐位相同**——这一点由黄金摘要测试守着：
//! 如果摘要变了，就是这三条里有哪条被破坏了。
//!
//! # 布局
//!
//! 每个岛一份本地 [`BodyView`] 数组（岛内清醒体 + 它接触到的冻结体副本），
//! 接触的 `a`/`b` 重映射到本地下标。岛的本地数组首尾相接放在一起，
//! 按「工作线程」分组连续排列，这样每个线程拿到的是**一段连续的可变切片**，
//! 不需要任何 `unsafe`。
//!
//! 岛的分配用 LPT（长的先分）贪心：按接触数从大到小，每次放到当前负载最小的线程，
//! 平局取编号小的线程 → 分配本身也是确定性的。

use crate::island::Islands;
use crate::solver::{
    relax_contacts, solve_position_contacts, solve_velocity_contacts, BodyView, SolverConfig,
    SolverContact,
};

/// 至少这么多接触才值得开线程（太小的场景线程开销大于收益）。
const MIN_CONTACTS: usize = 96;
/// 线程数上限（再多也是打满内存带宽，收益递减）。
const MAX_THREADS: usize = 16;

/// 一个岛在本地布局里的两段长度。
#[derive(Clone, Copy, Debug, Default)]
struct Chunk {
    /// 本地视图数量（清醒体 + 冻结伙伴副本）。
    view_len: u32,
    /// 接触数量。
    contact_len: u32,
}

/// 并行求解的布局与缓冲（跨步复用，稳态零分配）。
#[derive(Debug, Default)]
pub(crate) struct ParallelLayout {
    /// 本步是否启用并行（不满足条件时走串行路径）。
    enabled: bool,
    /// 本步使用的线程数。
    threads: usize,
    /// 本地视图数组（按线程分组、岛内连续）。
    views: Vec<BodyView>,
    /// 本地接触数组（同上）。
    contacts: Vec<SolverContact>,
    /// 本地视图下标 → 全局刚体槽位。
    view_global: Vec<u32>,
    /// 本地接触下标 → 全局接触下标。
    contact_global: Vec<u32>,
    /// 每个岛的布局信息，按（线程, 原岛序）排列。
    chunks: Vec<Chunk>,
    /// 每个线程占用的 `chunks` 区间 `[start, end)`。
    workers: Vec<(u32, u32)>,
    /// 全局刚体槽位 → 本地视图下标（用 `stamp` 做时间戳，避免每岛清零）。
    map: Vec<u32>,
    /// `map` 的时间戳。
    stamp: Vec<u32>,
    /// 当前岛的 epoch。
    epoch: u32,
    /// 接触按岛分桶后的全局下标（每岛一段，段内保持全局顺序）。
    order: Vec<u32>,
    /// 每个岛在 `order` 中的起点。
    start: Vec<u32>,
    /// 每个岛的接触数。
    count: Vec<u32>,
    /// 分桶写指针。
    write: Vec<u32>,
    /// 岛 → 线程编号。
    assign: Vec<u32>,
    /// 岛按（接触数降序, 下标升序）排好的顺序。
    seq: Vec<u32>,
    /// 每个线程的当前负载（接触数）。
    load: Vec<u32>,
}

impl ParallelLayout {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 本步使用的线程数。
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// 根据本步的 island 划分构建布局。返回是否启用并行。
    pub fn build(
        &mut self,
        islands: &Islands,
        views: &[BodyView],
        contacts: &[SolverContact],
        max_threads: usize,
    ) -> bool {
        self.enabled = false;
        let stats = islands.stats();
        if !stats.parallelizable() || contacts.len() < MIN_CONTACTS {
            return false;
        }
        let hw = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let cap = if max_threads == 0 {
            MAX_THREADS
        } else {
            max_threads.min(MAX_THREADS)
        };
        let threads = hw.min(cap).min(stats.multi_body_islands as usize).max(1);
        if threads < 2 {
            return false;
        }
        self.threads = threads;
        // 每步重新铺布局：所有展平缓冲都必须清空（否则会一步步累积，
        // 变成 O(步数²) 的工作量——这个 bug 让一个 240 步的测试跑了 36 秒）
        self.views.clear();
        self.contacts.clear();
        self.view_global.clear();
        self.contact_global.clear();
        self.chunks.clear();
        self.workers.clear();

        // --- 1. 每个岛有多少接触（接触至少有一端是清醒体，否则 prepare 阶段就被拒了） ---
        let n_islands = islands.len();
        self.count.clear();
        self.count.resize(n_islands, 0);
        for c in contacts.iter() {
            if let Some(k) = islands.island_of(c.a).or_else(|| islands.island_of(c.b)) {
                self.count[k as usize] += 1;
            }
        }
        // 前缀和 → 每岛在 order 中的起点
        self.start.clear();
        let mut acc = 0u32;
        for k in 0..n_islands {
            self.start.push(acc);
            acc += self.count[k];
        }
        // 稳定分桶：按全局顺序扫一遍，岛内顺序即全局顺序
        self.order.clear();
        self.order.resize(acc as usize, 0);
        self.write.clear();
        self.write.extend_from_slice(&self.start);
        for (i, c) in contacts.iter().enumerate() {
            if let Some(k) = islands.island_of(c.a).or_else(|| islands.island_of(c.b)) {
                let w = &mut self.write[k as usize];
                self.order[*w as usize] = i as u32;
                *w += 1;
            }
        }

        // --- 2. LPT 分配：接触多的岛先分给当前最闲的线程 ---
        self.assign.clear();
        self.assign.resize(n_islands, 0);
        {
            let Self {
                seq,
                assign,
                load,
                count,
                ..
            } = self;
            seq.clear();
            seq.extend(0..n_islands as u32);
            seq.sort_unstable_by(|&a, &b| {
                count[b as usize].cmp(&count[a as usize]).then(a.cmp(&b))
            });
            load.clear();
            load.resize(threads, 0);
            for &k in seq.iter() {
                let mut best = 0usize;
                for w in 1..threads {
                    if load[w] < load[best] {
                        best = w;
                    }
                }
                assign[k as usize] = best as u32;
                load[best] += count[k as usize];
            }
        }

        // --- 3. 按（线程, 岛序）铺开本地布局 ---
        self.seq.clear();
        self.seq.extend(0..n_islands as u32);
        self.seq
            .sort_unstable_by_key(|&k| (self.assign[k as usize], k));
        const UNASSIGNED: u32 = u32::MAX;
        let mut worker = UNASSIGNED;
        let mut worker_chunk_start = 0u32;
        for idx in 0..self.seq.len() {
            let k = self.seq[idx];
            let w = self.assign[k as usize];
            if w != worker {
                if worker != UNASSIGNED {
                    self.workers
                        .push((worker_chunk_start, self.chunks.len() as u32));
                }
                worker = w;
                worker_chunk_start = self.chunks.len() as u32;
            }
            self.push_island(islands, views, contacts, k);
        }
        if worker != UNASSIGNED {
            self.workers
                .push((worker_chunk_start, self.chunks.len() as u32));
        }

        self.enabled = !self.workers.is_empty();
        self.enabled
    }

    /// 把一个岛的视图与接触追加到本地布局。
    fn push_island(
        &mut self,
        islands: &Islands,
        views: &[BodyView],
        contacts: &[SolverContact],
        k: u32,
    ) {
        let view_start = self.views.len();
        let contact_start = self.contacts.len();
        // 新 epoch：本地映射表用时间戳判断有效，不必每岛清零
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            for s in self.stamp.iter_mut() {
                *s = 0;
            }
            self.epoch = 1;
        }
        // 岛内清醒体
        for &b in islands.bodies_of(k) {
            self.push_view(b as usize, views);
        }
        // 该岛的接触：先补上冻结伙伴的副本，再重映射下标
        let (s, e) = (
            self.start[k as usize] as usize,
            (self.start[k as usize] + self.count[k as usize]) as usize,
        );
        for j in s..e {
            let gi = self.order[j] as usize;
            let c = contacts[gi];
            // 接触里的下标要相对**本岛的切片**（线程内部就是按岛切片的），
            // 所以要减掉这个岛的视图起点。
            let a = self.local_index(c.a, views) - view_start;
            let b = self.local_index(c.b, views) - view_start;
            self.contacts.push(SolverContact { a, b, ..c });
            self.contact_global.push(gi as u32);
        }
        // 岛内清醒体的局部下标：并发写回时要按全局槽位同步
        let view_len = (self.views.len() - view_start) as u32;
        let contact_len = (self.contacts.len() - contact_start) as u32;
        self.chunks.push(Chunk {
            view_len,
            contact_len,
        });
    }

    /// 把全局刚体槽位加进本地视图（已加过就返回既有下标）。
    fn push_view(&mut self, slot: usize, views: &[BodyView]) -> usize {
        if self.map.len() <= slot {
            self.map.resize(slot + 1, u32::MAX);
            self.stamp.resize(slot + 1, 0);
        }
        if self.stamp[slot] == self.epoch {
            return self.map[slot] as usize;
        }
        let local = self.views.len();
        self.views.push(views[slot]);
        self.view_global.push(slot as u32);
        self.stamp[slot] = self.epoch;
        self.map[slot] = local as u32;
        local
    }

    /// 取全局刚体槽位在本地布局里的下标（不存在则加入）。
    fn local_index(&mut self, slot: usize, views: &[BodyView]) -> usize {
        self.push_view(slot, views)
    }

    /// 全局视图 → 本地视图（只同步清醒体；冻结体在求解中不变）。
    pub fn sync_from(&mut self, views: &[BodyView]) {
        if !self.enabled {
            return;
        }
        for (i, g) in self.view_global.iter().enumerate() {
            let g = *g as usize;
            if views[g].inv_m > 0.0 {
                self.views[i] = views[g];
            }
        }
    }

    /// 本地视图 → 全局视图（只写回清醒体，岛间因此互不重叠）。
    pub fn sync_to(&mut self, views: &mut [BodyView]) {
        if !self.enabled {
            return;
        }
        for (i, g) in self.view_global.iter().enumerate() {
            let g = *g as usize;
            if views[g].inv_m > 0.0 {
                views[g] = self.views[i];
            }
        }
    }

    /// 本地的接触冲量写回全局接触表（供热启动缓存与统计使用）。
    pub fn flush_impulses(&mut self, contacts: &mut [SolverContact]) {
        if !self.enabled {
            return;
        }
        for (i, g) in self.contact_global.iter().enumerate() {
            let src = &self.contacts[i];
            if let Some(dst) = contacts.get_mut(*g as usize) {
                dst.normal_impulse = src.normal_impulse;
                dst.tangent_impulse = src.tangent_impulse;
                dst.pseudo_impulse = src.pseudo_impulse;
            }
        }
    }

    /// 把每个岛的接触切片交给工作线程执行 `f`。
    ///
    /// 每个线程拿到一段**连续**的视图/接触切片（布局就是按线程分组铺的），
    /// 线程内部再按岛切分——全程安全 Rust，没有 `unsafe`。
    fn run_workers<F>(&mut self, f: F)
    where
        F: Fn(&mut [SolverContact], &mut [BodyView]) + Send + Sync,
    {
        if !self.enabled {
            return;
        }
        let Self {
            views,
            contacts,
            chunks,
            workers,
            ..
        } = self;
        std::thread::scope(|scope| {
            let mut vs: &mut [BodyView] = views;
            let mut cs: &mut [SolverContact] = contacts;
            for &(cstart, cend) in workers.iter() {
                let slice = &chunks[cstart as usize..cend as usize];
                let vlen: usize = slice.iter().map(|c| c.view_len as usize).sum();
                let clen: usize = slice.iter().map(|c| c.contact_len as usize).sum();
                let (v, vr) = vs.split_at_mut(vlen);
                vs = vr;
                let (c, cr) = cs.split_at_mut(clen);
                cs = cr;
                let f = &f;
                scope.spawn(move || {
                    let mut vv = v;
                    let mut cc = c;
                    for ch in slice {
                        let (iv, ivr) = vv.split_at_mut(ch.view_len as usize);
                        vv = ivr;
                        let (ic, icr) = cc.split_at_mut(ch.contact_len as usize);
                        cc = icr;
                        f(ic, iv);
                    }
                });
            }
        });
    }

    /// 场景里没有关节时：每个工作线程一口气跑完自己那些岛的
    /// 「全部速度迭代 → 松弛 → 位置修正」。
    ///
    /// 这样每步只需**一次**线程分发（而不是每个迭代一次），同步开销从
    /// 「12 波 × 2 次全量同步」降到「1 波 + 2 次」。结果仍然逐位相同：
    /// 岛与岛之间没有任何共享的清醒刚体，每个刚体看到的运算序列与串行一致。
    pub fn solve_all(
        &mut self,
        iters: usize,
        relax_iters: usize,
        gain: f32,
        dt: f32,
        cfg: &SolverConfig,
        pos_iters: usize,
    ) {
        self.run_workers(move |cs, vs| {
            for _ in 0..iters {
                solve_velocity_contacts(cs, vs);
            }
            relax_contacts(cs, vs, relax_iters, gain);
            solve_position_contacts(cs, vs, dt, cfg, pos_iters);
        });
    }

    /// 并行跑一轮速度迭代。
    pub fn solve_velocity(&mut self) {
        self.run_workers(solve_velocity_contacts);
    }

    /// 并行松弛。
    pub fn relax(&mut self, iterations: usize, gain: f32) {
        self.run_workers(|cs, vs| relax_contacts(cs, vs, iterations, gain));
    }

    /// 并行位置修正。
    pub fn solve_position(&mut self, dt: f32, cfg: &SolverConfig, iterations: usize) {
        self.run_workers(|cs, vs| solve_position_contacts(cs, vs, dt, cfg, iterations));
    }
}
