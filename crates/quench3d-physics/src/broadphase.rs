//! 宽相：均匀空间哈希网格。
//!
//! * 单元边长取所有 AABB 最大边长的中位数（乘 2，让典型物体大致占一个单元），
//!   因此网格规模与场景尺度自适应；
//! * 每个单元只与 13 个「前向」邻居配对，保证每对物体只检测一次；
//! * 半空间没有有限 AABB，单独作为「全局代理」与所有其他物体配对；
//! * 输出按 `(a, b)` 排序去重，完全不依赖哈希遍历顺序 → 可复现；
//! * 网格用**复用的扁平数组**（排序后的单元键 + 区间 + 展平的下标）实现，
//!   稳态下每步零堆分配——早期版本用 `BTreeMap`，每步要为每个单元分配/释放节点，
//!   一个「全部睡着」的 500 体场景因此要 480 次分配/步；
//! * 邻居单元用**直接映射的开放寻址哈希表**（线性探测 + 时间戳，不需要每步清零）
//!   定位，而不是对排序后的键做二分：2000 体深堆一步有十几万次邻居查询，
//!   二分查找是这一阶段最大的开销（实测占整步 32~37%）。
//!   查找方式变了，但**候选对集合与顺序完全不变**（单元集合一样、输出照样排序去重），
//!   所以物理结果是逐位可复现的。

use crate::body::InteractionGroups;
use quench3d_math::*;

/// 参与宽相的代理。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Proxy {
    /// 刚体槽位下标。
    pub index: u32,
    /// 世界 AABB（半空间为无穷）。
    pub aabb: Aabb,
    /// 是否**可能移动**（动态体，或运动学体）。
    ///
    /// 名字不叫 `dynamic`：宽相这里问的是"这一对有没有必要算接触"，
    /// 而运动学体虽然质量无穷大、也不会睡觉，却必须参与（否则它撞上睡眠刚体时
    /// 连候选对都不生成，唤醒逻辑永远走不到——曾经的 bug）。
    pub movable: bool,
    /// 是否处于睡眠。
    pub sleeping: bool,
    /// 是否全局代理（半空间）。
    pub global: bool,
    /// 碰撞过滤分组。
    ///
    /// 放在宽相而不是窄相：不碰的一对物体连候选对都不该产生，
    /// 否则"过滤掉 90% 的物体"在宽相之后仍然是 O(对数) 的窄相开销。
    pub groups: InteractionGroups,
}

/// BVH 节点：叶子用 `start/count` 指向 `bvh_order`，内部节点用 `left/right`。
#[derive(Clone, Copy, Debug)]
struct BvhNode {
    aabb: Aabb,
    left: u32,
    right: u32,
    start: u32,
    count: u32,
}

/// 超过这个代理数就改用 BVH：深堆里网格的"相邻单元 × 单元内物体"会退化成
/// 十几万次 AABB 测试，而 BVH 只测真正重叠的分支（候选对集合不变）。
const BVH_MIN_PROXIES: usize = 512;

/// 空间哈希宽相（所有缓冲跨步复用，稳态零分配）。
#[derive(Clone, Debug)]
pub(crate) struct Broadphase {
    /// 当前单元边长。
    cell_size: f32,
    /// 排序去重后的单元键。
    keys: Vec<(i32, i32, i32)>,
    /// 每个单元键在 [`Broadphase::items`] 中的区间 `[start, end)`。
    ranges: Vec<(u32, u32)>,
    /// 展平的 `(单元, 代理)` 展开结果，按单元键分组。
    items: Vec<u32>,
    /// 排序前的 `(单元键, 代理下标)`。
    scratch: Vec<((i32, i32, i32), u32)>,
    /// AABB 尺寸缓冲（算单元边长用）。
    sizes: Vec<f32>,
    /// 全局代理下标（半空间 / 超大物体）。
    globals: Vec<usize>,
    /// 开放寻址哈希表：高 32 位是时间戳，低 32 位是「单元下标 + 1」。
    ///
    /// 合成一个数组是为了把每次探测的随机访存次数从 2 降到 1——
    /// 深堆场景一步十几万次探测，访存次数直接决定这一阶段的耗时。
    buckets: Vec<u64>,
    /// 当前轮次的时间戳（每步 +1，避免每步把整张表清零）。
    stamp: u32,
    /// `stamps.len() - 1`，用于代替取模。
    mask: usize,
    /// BVH 节点与叶子顺序（大场景复用）。
    bvh_nodes: Vec<BvhNode>,
    bvh_order: Vec<u32>,
}

impl Default for Broadphase {
    fn default() -> Self {
        Self::new()
    }
}

/// 13 个前向邻居偏移（加上自身单元即为完整邻域）。
const NEIGHBORS: [(i32, i32, i32); 13] = [
    (1, 0, 0),
    (1, 1, 0),
    (1, -1, 0),
    (1, 0, 1),
    (1, 0, -1),
    (1, 1, 1),
    (1, 1, -1),
    (1, -1, 1),
    (1, -1, -1),
    (0, 1, 0),
    (0, 1, 1),
    (0, 1, -1),
    (0, 0, 1),
];

/// 单个物体最多占用的单元数（防止超大物体拖垮网格）。
const MAX_CELLS_PER_PROXY: i64 = 64;

impl Broadphase {
    /// 新建。
    pub fn new() -> Self {
        Self {
            cell_size: 1.0,
            keys: Vec::new(),
            ranges: Vec::new(),
            items: Vec::new(),
            scratch: Vec::new(),
            sizes: Vec::new(),
            globals: Vec::new(),
            buckets: Vec::new(),
            stamp: 0,
            mask: 0,
            bvh_nodes: Vec::new(),
            bvh_order: Vec::new(),
        }
    }

    /// 单元键 → 单元下标（开放寻址，线性探测）。
    ///
    /// 表里只放「本轮」写入的槽位：用时间戳代替清零，场景从小到大变化时也不用重扫。
    #[inline]
    fn cell_index(&self, key: (i32, i32, i32)) -> Option<u32> {
        if self.buckets.is_empty() {
            return None;
        }
        let stamp = self.stamp as u64;
        let mut h = hash_key(key) & self.mask;
        loop {
            let slot = self.buckets[h];
            if (slot >> 32) != stamp {
                return None;
            }
            let idx = (slot & 0xFFFF_FFFF) as usize;
            if idx != 0 && self.keys[idx - 1] == key {
                return Some(idx as u32 - 1);
            }
            h = (h + 1) & self.mask;
        }
    }

    /// 把单元键写进哈希表（调用前必须已经填好 `keys`）。
    fn build_lookup(&mut self) {
        let need = (self.keys.len() * 2).next_power_of_two().max(16);
        if self.buckets.len() != need {
            self.buckets = vec![0; need];
            self.stamp = 1;
            self.mask = need - 1;
        }
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            // 时间戳回绕：清零一次，重新开始
            for slot in self.buckets.iter_mut() {
                *slot = 0;
            }
            self.stamp = 1;
        }
        let stamp = self.stamp as u64;
        for (idx, key) in self.keys.iter().enumerate() {
            let mut h = hash_key(*key) & self.mask;
            while (self.buckets[h] >> 32) == stamp {
                h = (h + 1) & self.mask;
            }
            self.buckets[h] = (stamp << 32) | (idx as u64 + 1);
        }
    }

    /// 当前单元边长（供测试与调试观察）。
    #[allow(dead_code)]
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// 计算所有可能相交的刚体对，写入 `out`（已排序去重）。
    pub fn compute_pairs(&mut self, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        if proxies.len() >= BVH_MIN_PROXIES {
            out.clear();
            if proxies.len() < 2 || !proxies.iter().any(|p| p.movable) {
                return;
            }
            self.compute_pairs_bvh(proxies, out);
            out.sort_unstable();
            out.dedup();
            return;
        }
        self.compute_pairs_grid(proxies, out);
    }

    /// 大场景的 BVH 宽相（与网格产生同一组候选对，只是少做无用测试）。
    fn compute_pairs_bvh(&mut self, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        self.bvh_order.clear();
        self.globals.clear();
        for (i, p) in proxies.iter().enumerate() {
            if p.global || !p.aabb.is_valid() {
                self.globals.push(i);
            } else {
                self.bvh_order.push(i as u32);
            }
        }
        self.bvh_nodes.clear();
        if !self.bvh_order.is_empty() {
            let root = self.build_bvh(proxies, 0, self.bvh_order.len(), 0);
            self.bvh_self(root, proxies, out);
        }
        // 全局代理（半空间/超大物体）仍逐个测；数量通常是个位数。
        for gi in 0..self.globals.len() {
            let i = self.globals[gi];
            for j in 0..proxies.len() {
                if i == j || self.globals[gi + 1..].contains(&j) {
                    continue;
                }
                try_pair(proxies, i as u32, j as u32, out);
            }
        }
    }

    fn build_bvh(&mut self, proxies: &[Proxy], start: usize, end: usize, depth: usize) -> u32 {
        let mut aabb = proxies[self.bvh_order[start] as usize].aabb;
        for i in self.bvh_order[start + 1..end].iter() {
            aabb = aabb.union(&proxies[*i as usize].aabb);
        }
        let idx = self.bvh_nodes.len() as u32;
        self.bvh_nodes.push(BvhNode {
            aabb,
            left: u32::MAX,
            right: u32::MAX,
            start: start as u32,
            count: (end - start) as u32,
        });
        if end - start <= 4 || depth >= 24 {
            return idx;
        }
        let mut cmin = Vec3::splat(f32::INFINITY);
        let mut cmax = Vec3::splat(f32::NEG_INFINITY);
        for i in &self.bvh_order[start..end] {
            let c = proxies[*i as usize].aabb.center();
            cmin = cmin.min(c);
            cmax = cmax.max(c);
        }
        let extent = cmax - cmin;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        let mid = start + (end - start) / 2;
        let order = &mut self.bvh_order[start..end];
        // 只把「中间元素」放到中位，不整段排序：建树总比较次数从
        // O(n log² n) 降到 O(n log n)。左右子树的遍历顺序会变，但候选对
        // 最终统一排序去重，集合与旧实现相同。
        order.select_nth_unstable_by(mid - start, |a, b| {
            let ca = proxies[*a as usize].aabb.center();
            let cb = proxies[*b as usize].aabb.center();
            let (a, b) = match axis {
                0 => (ca.x, cb.x),
                1 => (ca.y, cb.y),
                _ => (ca.z, cb.z),
            };
            a.total_cmp(&b)
        });
        let left = self.build_bvh(proxies, start, mid, depth + 1);
        let right = self.build_bvh(proxies, mid, end, depth + 1);
        self.bvh_nodes[idx as usize].left = left;
        self.bvh_nodes[idx as usize].right = right;
        idx
    }

    fn bvh_self(&self, node: u32, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        let n = self.bvh_nodes[node as usize];
        if n.left == u32::MAX {
            self.bvh_leaf_pairs(node, node, proxies, out);
            return;
        }
        self.bvh_self(n.left, proxies, out);
        self.bvh_self(n.right, proxies, out);
        self.bvh_cross(n.left, n.right, proxies, out);
    }

    fn bvh_cross(&self, a: u32, b: u32, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        let na = self.bvh_nodes[a as usize];
        let nb = self.bvh_nodes[b as usize];
        if !na.aabb.intersects(&nb.aabb) {
            return;
        }
        let a_leaf = na.left == u32::MAX;
        let b_leaf = nb.left == u32::MAX;
        if a_leaf && b_leaf {
            self.bvh_leaf_pairs(a, b, proxies, out);
        } else if a_leaf {
            self.bvh_cross(a, nb.left, proxies, out);
            self.bvh_cross(a, nb.right, proxies, out);
        } else if b_leaf {
            self.bvh_cross(na.left, b, proxies, out);
            self.bvh_cross(na.right, b, proxies, out);
        } else {
            self.bvh_cross(na.left, nb.left, proxies, out);
            self.bvh_cross(na.left, nb.right, proxies, out);
            self.bvh_cross(na.right, nb.left, proxies, out);
            self.bvh_cross(na.right, nb.right, proxies, out);
        }
    }

    fn bvh_leaf_pairs(&self, a: u32, b: u32, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        let na = self.bvh_nodes[a as usize];
        let nb = self.bvh_nodes[b as usize];
        for ia in na.start..(na.start + na.count) {
            let i = self.bvh_order[ia as usize];
            for ib in nb.start..(nb.start + nb.count) {
                let j = self.bvh_order[ib as usize];
                if a == b && j <= i {
                    continue;
                }
                try_pair(proxies, i, j, out);
            }
        }
    }

    /// 小场景的网格宽相（历史路径，保逐位兼容）。
    fn compute_pairs_grid(&mut self, proxies: &[Proxy], out: &mut Vec<(u32, u32)>) {
        out.clear();
        if proxies.len() < 2 {
            return;
        }
        // 没有任何「清醒的可动体」时，`try_pair` 的可动性检查会拒绝所有组合，
        // 结果必然为空——直接返回，让整片睡着的世界几乎不花时间。
        if !proxies.iter().any(|p| p.movable) {
            return;
        }

        // --- 单元边长：AABB 最长边中位数（乘 2） ---
        self.sizes.clear();
        for p in proxies {
            if p.global {
                continue;
            }
            let s = p.aabb.size();
            let m = s.x.max(s.y).max(s.z);
            if m.is_finite() && m > 1e-6 {
                self.sizes.push(m);
            }
        }
        self.cell_size = pick_cell_size(&mut self.sizes);

        // --- 展开成 (单元键, 代理下标) ---
        self.scratch.clear();
        self.globals.clear();
        for (i, p) in proxies.iter().enumerate() {
            if p.global || !p.aabb.is_valid() {
                self.globals.push(i);
                continue;
            }
            if !spread_into_cells(&p.aabb, self.cell_size, i as u32, &mut self.scratch) {
                // 超大物体：升级为全局代理（与其他所有代理做 AABB 测试），
                // 保证不漏检；代价是与场景规模成线性。
                self.globals.push(i);
            }
        }

        // --- 按单元键压缩成「键 + 区间 + 展平下标」 ---
        self.scratch.sort_unstable();
        self.keys.clear();
        self.ranges.clear();
        self.items.clear();
        let mut k = 0usize;
        while k < self.scratch.len() {
            let key = self.scratch[k].0;
            let start = self.items.len() as u32;
            while k < self.scratch.len() && self.scratch[k].0 == key {
                self.items.push(self.scratch[k].1);
                k += 1;
            }
            self.keys.push(key);
            self.ranges.push((start, self.items.len() as u32));
        }

        // --- 建立单元键的哈希查找表 ---
        self.build_lookup();

        // --- 单元内 + 13 个前向邻居配对 ---
        for c in 0..self.keys.len() {
            let (start, end) = self.ranges[c];
            for a in start..end {
                for b in (a + 1)..end {
                    try_pair(proxies, self.items[a as usize], self.items[b as usize], out);
                }
            }
            let key = self.keys[c];
            for off in NEIGHBORS {
                let neighbor = (key.0 + off.0, key.1 + off.1, key.2 + off.2);
                let pos = match self.cell_index(neighbor) {
                    Some(pos) => pos as usize,
                    None => continue,
                };
                let (ns, ne) = self.ranges[pos];
                for a in start..end {
                    for b in ns..ne {
                        try_pair(proxies, self.items[a as usize], self.items[b as usize], out);
                    }
                }
            }
        }

        // --- 全局代理（半空间 / 超大物体）与其他所有代理配对 ---
        for gi in 0..self.globals.len() {
            let i = self.globals[gi];
            for j in 0..proxies.len() {
                if i == j {
                    continue;
                }
                // 两个全局代理只配对一次。
                if self.globals[gi + 1..].contains(&j) {
                    continue;
                }
                try_pair(proxies, i as u32, j as u32, out);
            }
        }

        out.sort_unstable();
        out.dedup();
    }
}

/// 单元键的整数混洗哈希（确定性：不用随机种子，保证跨运行可复现）。
#[inline]
fn hash_key(key: (i32, i32, i32)) -> usize {
    let mut h = (key.0 as u32).wrapping_mul(0x9E37_79B1)
        ^ (key.1 as u32).wrapping_mul(0x85EB_CA77)
        ^ (key.2 as u32).wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h as usize
}

/// 由 AABB 最长边样本选单元边长（25 分位 × 2，钳到 `[0.05, 1e4]`）。
///
/// 宽相与 [`crate::query_index::QueryIndex`] 共用同一套策略：两边的网格形状一致，
/// "宽相里配得上对的两个物体，查询时也该在彼此的候选里"这件事才不需要额外推理。
///
/// 取 25 分位而不是中位数：少量超大物体（地板网格）不该把整个网格撑大。
pub(crate) fn pick_cell_size(sizes: &mut [f32]) -> f32 {
    if sizes.is_empty() {
        return 1.0;
    }
    // 不稳定排序即可：相等的边长不可区分，选中值不变。
    sizes.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let idx = sizes.len() / 4;
    // 倍率取 2：典型物体大致占一个单元。这个倍率还没有做过严格的
    // 配对 A/B（见 README 8.4 的说明），是一个已知可调的旋钮。
    clamp(sizes[idx] * 2.0, 0.05, 1.0e4)
}

/// 把一个 AABB 铺进它覆盖的所有单元。
///
/// 返回 `false` 表示覆盖的单元数超过 [`MAX_CELLS_PER_PROXY`]（超大物体）：
/// 调用方应当把它当"全局代理"，每个查询都测一遍，保证不漏检。
///
/// AABB 非有限时返回 `true` 但一个单元都不铺——与宽相原有行为一致
/// （坐标是 NaN 的刚体在宽相里是"看不见"的）。
pub(crate) fn spread_into_cells<T: Copy>(
    aabb: &Aabb,
    cell_size: f32,
    value: T,
    scratch: &mut Vec<((i32, i32, i32), T)>,
) -> bool {
    let inv_cell = 1.0 / cell_size;
    let min = aabb.min * inv_cell;
    let max = aabb.max * inv_cell;
    let mut lo = [0i32; 3];
    let mut hi = [0i32; 3];
    let mins = [min.x, min.y, min.z];
    let maxs = [max.x, max.y, max.z];
    let mut span = 1i64;
    for k in 0..3 {
        let a = (mins[k] as f64).floor() as f32;
        let b = (maxs[k] as f64).floor() as f32;
        if !a.is_finite() || !b.is_finite() {
            lo[k] = i32::MAX;
            hi[k] = i32::MIN;
            continue;
        }
        lo[k] = clamp_i32(a);
        hi[k] = clamp_i32(b);
        if k == 0 {
            span = hi[k] as i64 - lo[k] as i64 + 1;
        } else {
            span *= hi[k] as i64 - lo[k] as i64 + 1;
        }
    }
    if span > MAX_CELLS_PER_PROXY {
        return false;
    }
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                scratch.push(((x, y, z), value));
            }
        }
    }
    true
}

#[inline]
fn clamp_i32(v: f32) -> i32 {
    if v > 1.0e9 {
        1_000_000_000
    } else if v < -1.0e9 {
        -1_000_000_000
    } else {
        v as i32
    }
}

/// 过滤并记录一对候选。
fn try_pair(proxies: &[Proxy], i: u32, j: u32, out: &mut Vec<(u32, u32)>) {
    if i == j {
        return;
    }
    let (pa, pb) = (proxies[i as usize], proxies[j as usize]);
    // 两个都不可动（静态×静态、静态×睡眠）→ 无意义
    if !pa.movable && !pb.movable {
        return;
    }
    // 两个都在睡眠 → 跳过
    if pa.sleeping && pb.sleeping {
        return;
    }
    // 分组不互相匹配 → 跳过（对称：两边都要愿意）
    if !pa.groups.interacts(pb.groups) {
        return;
    }
    // AABB 相交测试：半空间的 AABB 为无穷，`intersects` 恒为真。
    if !pa.aabb.intersects(&pb.aabb) {
        return;
    }
    let key = if pa.index < pb.index {
        (pa.index, pb.index)
    } else {
        (pb.index, pa.index)
    };
    if key.0 == key.1 {
        return;
    }
    out.push(key);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(index: u32, c: Vec3, half: f32) -> Proxy {
        proxy_groups(index, c, half, InteractionGroups::default())
    }

    fn proxy_groups(index: u32, c: Vec3, half: f32, groups: InteractionGroups) -> Proxy {
        Proxy {
            index,
            aabb: Aabb::from_center_half_extents(c, Vec3::splat(half)),
            movable: true,
            groups,
            sleeping: false,
            global: false,
        }
    }

    #[test]
    fn overlapping_pair_is_reported_once() {
        let proxies = vec![
            proxy(0, Vec3::ZERO, 0.5),
            proxy(1, vec3(0.5, 0.0, 0.0), 0.5),
        ];
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&proxies, &mut out);
        assert_eq!(out, vec![(0, 1)]);
        assert_eq!(out.len(), 1, "不能重复报告同一对");
    }

    #[test]
    fn bvh_and_grid_report_the_same_candidate_pairs() {
        let mut proxies = Vec::new();
        // 一个半空间（全局代理）+ 600 个随机代理，覆盖 movable/sleeping/groups。
        proxies.push(Proxy {
            index: 0,
            aabb: Aabb::new(Vec3::splat(f32::NEG_INFINITY), Vec3::splat(f32::INFINITY)),
            movable: false,
            sleeping: false,
            global: true,
            groups: InteractionGroups::default(),
        });
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / 16_777_216.0
        };
        for i in 1..601 {
            let c = Vec3::new(rnd() * 8.0 - 4.0, rnd() * 8.0 - 4.0, rnd() * 8.0 - 4.0);
            let h = 0.05 + rnd() * 0.45;
            let groups = InteractionGroups {
                memberships: 1 << (i % 4),
                filter: InteractionGroups::ALL,
            };
            proxies.push(Proxy {
                index: i as u32,
                aabb: Aabb::from_center_half_extents(c, Vec3::splat(h)),
                movable: i % 7 != 0,
                sleeping: i % 11 == 0,
                global: false,
                groups,
            });
        }
        let mut grid = Broadphase::new();
        let mut want = Vec::new();
        grid.compute_pairs_grid(&proxies, &mut want);
        want.sort_unstable();
        want.dedup();
        let mut bvh = Broadphase::new();
        let mut got = Vec::new();
        bvh.compute_pairs(&proxies, &mut got);
        assert_eq!(got, want, "BVH 必须与网格产生完全相同的候选对");
    }

    #[test]
    fn distant_pair_is_culled() {
        let proxies = vec![
            proxy(0, Vec3::ZERO, 0.5),
            proxy(1, vec3(50.0, 0.0, 0.0), 0.5),
        ];
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&proxies, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn static_and_sleeping_pairs_are_culled() {
        let mut a = proxy(0, Vec3::ZERO, 0.5);
        let mut b = proxy(1, vec3(0.5, 0.0, 0.0), 0.5);
        a.movable = false;
        b.movable = false;
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&[a, b], &mut out);
        assert!(out.is_empty(), "静态-静态应被剔除");

        a.movable = true;
        b.movable = true;
        a.sleeping = true;
        b.sleeping = true;
        bp.compute_pairs(&[a, b], &mut out);
        assert!(out.is_empty(), "睡眠-睡眠应被剔除");
    }

    #[test]
    fn grid_of_bodies_is_linear_ish() {
        // 10x10x2 网格，间距 2（互不接触）→ 期望 0 对。
        let mut proxies = Vec::new();
        let mut idx = 0u32;
        for x in 0..10 {
            for y in 0..10 {
                for z in 0..2 {
                    proxies.push(proxy(
                        idx,
                        vec3(x as f32 * 2.0, y as f32 * 2.0, z as f32 * 2.0),
                        0.5,
                    ));
                    idx += 1;
                }
            }
        }
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&proxies, &mut out);
        assert!(
            out.is_empty(),
            "间距 2、半径 0.5 不应有重叠，实际 {}",
            out.len()
        );
        assert!(bp.cell_size() >= 0.05);
    }

    #[test]
    fn oversized_proxy_is_not_dropped() {
        // 20x20 的「地板」+ 两个小物体，25 分位会把单元设得很小；
        // 地板必须升级为全局代理，否则会漏掉与小物体的配对。
        let mut floor = proxy(0, Vec3::ZERO, 10.0);
        floor.movable = false;
        let proxies = vec![
            floor,
            proxy(1, vec3(3.0, 2.0, 3.0), 0.5),
            proxy(2, vec3(-3.0, 2.0, -3.0), 0.5),
        ];
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&proxies, &mut out);
        assert_eq!(out, vec![(0, 1), (0, 2)], "超大地板不应漏检");
    }

    /// 宽相的输出必须恰好等于「所有 AABB 相交、且至少一个可动、且不都睡着」的对。
    ///
    /// 用暴力 O(n²) 做参照：这样无论网格、单元边长、邻居枚举、哈希查找怎么改，
    /// 只要漏检或多检就会被抓住。（哈希查找替代二分查找之后加的这一条。）
    #[test]
    fn pairs_match_brute_force_on_a_dense_scene() {
        // 确定性伪随机布局：不加依赖、每次运行都一样
        let mut proxies = Vec::new();
        for i in 0..400u32 {
            let f = |k: u32| {
                (i.wrapping_mul(2_654_435_761)
                    .wrapping_add(k.wrapping_mul(40_503))
                    % 1000) as f32
                    / 100.0
                    - 5.0
            };
            let mut p = proxy(i, vec3(f(1) * 0.4, f(2) * 0.4, f(3) * 0.4), 0.35);
            // 造出静态、睡眠、全局三种特殊代理
            if i % 17 == 0 {
                p.movable = false;
            }
            if i % 23 == 0 {
                p.sleeping = true;
            }
            if i % 97 == 0 {
                p.global = true;
                p.aabb = Aabb::new(Vec3::NEG_INFINITY, Vec3::INFINITY);
                p.movable = false;
            }
            proxies.push(p);
        }

        let mut bp = Broadphase::new();
        let mut got = Vec::new();
        bp.compute_pairs(&proxies, &mut got);

        let mut want = Vec::new();
        for (i, pa) in proxies.iter().enumerate() {
            for pb in proxies.iter().skip(i + 1) {
                if !pa.movable && !pb.movable {
                    continue;
                }
                if pa.sleeping && pb.sleeping {
                    continue;
                }
                if !pa.aabb.intersects(&pb.aabb) {
                    continue;
                }
                want.push((pa.index.min(pb.index), pa.index.max(pb.index)));
            }
        }
        want.sort_unstable();
        want.dedup();
        assert!(!want.is_empty(), "这个场景本来应该有配对");
        assert_eq!(got, want, "宽相结果与暴力枚举不一致");
    }

    #[test]
    fn groups_filter_pairs_before_narrowphase() {
        // 两个重叠的代理，但分组互不匹配 → 连候选对都不产生
        // （A 只在第 0 层且只碰第 0 层；B 只在第 1 层且只碰第 1 层）
        let a = proxy_groups(0, Vec3::ZERO, 1.0, InteractionGroups::new(1 << 0, 1 << 0));
        let b = proxy_groups(1, Vec3::ZERO, 1.0, InteractionGroups::new(1 << 1, 1 << 1));
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&[a, b], &mut out);
        assert!(out.is_empty(), "互不匹配的分组不该产生候选对：{out:?}");

        // 单边愿意也不行（对称匹配）：A（第 0 层）愿意碰第 0 层的 B，
        // 但 B（第 1 层）只愿意碰第 0 层 —— 于是 B 的 filter 不含 A 的成员层 → 不碰
        let a = proxy_groups(0, Vec3::ZERO, 1.0, InteractionGroups::new(1 << 0, 1 << 0));
        let b = proxy_groups(1, Vec3::ZERO, 1.0, InteractionGroups::new(1 << 1, 1 << 0));
        bp.compute_pairs(&[a, b], &mut out);
        assert!(out.is_empty(), "A 想碰 B 但 B 不想碰 A → 不碰");

        // 双方都匹配 → 正常产生（回归：过滤不能把正常的对也滤掉）
        let a = proxy_groups(0, Vec3::ZERO, 1.0, InteractionGroups::layer(3));
        let b = proxy_groups(1, Vec3::ZERO, 1.0, InteractionGroups::layer(3));
        bp.compute_pairs(&[a, b], &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn global_proxy_pairs_with_everything() {
        let mut plane = proxy(0, Vec3::ZERO, 0.5);
        plane.global = true;
        plane.aabb = Aabb::new(Vec3::NEG_INFINITY, Vec3::INFINITY);
        plane.movable = false;
        let bodies = vec![
            plane,
            proxy(1, vec3(0.0, 1.0, 0.0), 0.5),
            proxy(2, vec3(20.0, 1.0, 0.0), 0.5),
        ];
        let mut bp = Broadphase::new();
        let mut out = Vec::new();
        bp.compute_pairs(&bodies, &mut out);
        assert_eq!(out, vec![(0, 1), (0, 2)]);
    }
}
