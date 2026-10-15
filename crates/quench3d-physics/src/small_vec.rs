//! 内联小向量：常见规模下完全不碰堆，超出容量才自动退化成 [`Vec`]。
//!
//! 为什么需要它：物理热路径上**每个接触对**都要临时放几个接触点。用 `Vec` 就是
//! 每次碰撞一次 malloc/free——2000 个刚体的场景里每步几万次分配，既慢又费内存带宽。
//! 这个类型把前 `N` 个元素直接放在结构体里（`[T; N]`，要求 `T: Copy + Default`），
//! 只在**真的超过容量**时才分配。
//!
//! 关键性质：
//! * 行为与 `Vec` 完全一致（不会像定长数组那样丢数据），所以是一个**纯性能**替换；
//! * 全程安全代码，不需要 `unsafe`/`MaybeUninit`；
//! * 实现了 [`Deref`]/[`DerefMut`] 到 `[T]`，因此 `v[0]`、`for x in &v`、`v.len()`
//!   这些写法与 `Vec` 一样可用。

use core::cmp::Ordering;
use core::ops::{Deref, DerefMut};

/// 内联容量为 `N` 的小向量。
///
/// `T` 需要 `Copy + Default`，这样内联存储可以直接用数组初始化，不需要 `unsafe`。
#[derive(Clone, Debug)]
pub enum SmallVec<T: Copy + Default, const N: usize> {
    /// 还在内联数组里（`items[..len]` 有效）。
    Inline {
        /// 内联存储。
        items: [T; N],
        /// 有效长度。
        len: usize,
    },
    /// 超过内联容量后退化为堆存储。
    Heap(Vec<T>),
}

impl<T: Copy + Default, const N: usize> Default for SmallVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> SmallVec<T, N> {
    /// 新建空向量（不分配）。
    pub fn new() -> Self {
        Self::Inline {
            items: [T::default(); N],
            len: 0,
        }
    }

    /// 元素个数。
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            Self::Inline { len, .. } => *len,
            Self::Heap(v) => v.len(),
        }
    }

    /// 是否为空。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 清空但**保留容量**（内联部分不清零，堆部分保留分配）。
    #[inline]
    pub fn clear(&mut self) {
        match self {
            Self::Inline { len, .. } => *len = 0,
            Self::Heap(v) => v.clear(),
        }
    }

    /// 追加一个元素；内联满时转成堆存储（只发生一次）。
    #[inline]
    pub fn push(&mut self, value: T) {
        match self {
            Self::Inline { items, len } => {
                if *len < N {
                    items[*len] = value;
                    *len += 1;
                } else {
                    let mut v = Vec::with_capacity(N * 2);
                    v.extend_from_slice(&items[..*len]);
                    v.push(value);
                    *self = Self::Heap(v);
                }
            }
            Self::Heap(v) => v.push(value),
        }
    }

    /// 追加一段切片。
    pub fn extend_from_slice(&mut self, other: &[T]) {
        for &item in other {
            self.push(item);
        }
    }

    /// 截断到 `n` 个元素。
    pub fn truncate(&mut self, n: usize) {
        match self {
            Self::Inline { len, .. } => *len = (*len).min(n),
            Self::Heap(v) => v.truncate(n),
        }
    }

    /// 稳定排序（与 `Vec::sort_by` 一致，保证确定性）。
    pub fn sort_by<F: FnMut(&T, &T) -> Ordering>(&mut self, compare: F) {
        self.as_mut_slice().sort_by(compare);
    }

    /// 按 `f` 保留元素。
    pub fn retain<F: FnMut(&T) -> bool>(&mut self, mut keep: F) {
        match self {
            Self::Inline { items, len } => {
                let mut write = 0usize;
                for read in 0..*len {
                    if keep(&items[read]) {
                        items[write] = items[read];
                        write += 1;
                    }
                }
                *len = write;
            }
            Self::Heap(v) => v.retain(|x| keep(x)),
        }
    }

    /// 切片视图。
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::Inline { items, len } => &items[..*len],
            Self::Heap(v) => v.as_slice(),
        }
    }

    /// 可变切片视图。
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        match self {
            Self::Inline { items, len } => &mut items[..*len],
            Self::Heap(v) => v.as_mut_slice(),
        }
    }

    /// 第一个元素。
    #[inline]
    pub fn first(&self) -> Option<&T> {
        self.as_slice().first()
    }

    /// 最后一个元素。
    #[inline]
    pub fn last(&self) -> Option<&T> {
        self.as_slice().last()
    }

    /// 是否还在内联存储里（用于测试与基准观察）。
    #[inline]
    pub fn is_inline(&self) -> bool {
        matches!(self, Self::Inline { .. })
    }
}

impl<T: Copy + Default, const N: usize> Deref for SmallVec<T, N> {
    type Target = [T];
    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy + Default, const N: usize> DerefMut for SmallVec<T, N> {
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: Copy + Default, const N: usize> AsRef<[T]> for SmallVec<T, N> {
    #[inline]
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy + Default, const N: usize> AsMut<[T]> for SmallVec<T, N> {
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: Copy + Default + PartialEq, const N: usize> PartialEq for SmallVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<'a, T: Copy + Default, const N: usize> IntoIterator for &'a SmallVec<T, N> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl<'a, T: Copy + Default, const N: usize> IntoIterator for &'a mut SmallVec<T, N> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_mut_slice().iter_mut()
    }
}

impl<T: Copy + Default, const N: usize> FromIterator<T> for SmallVec<T, N> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut out = Self::new();
        for item in iter {
            out.push(item);
        }
        out
    }
}

impl<T: Copy + Default, const N: usize> From<Vec<T>> for SmallVec<T, N> {
    /// 从 `Vec` 转换：放得下就搬进内联存储（随后释放堆内存），放不下就沿用这个 `Vec`。
    fn from(v: Vec<T>) -> Self {
        if v.len() <= N {
            let mut out = Self::new();
            out.extend_from_slice(&v);
            out
        } else {
            Self::Heap(v)
        }
    }
}

impl<T: Copy + Default, const N: usize, const M: usize> From<[T; M]> for SmallVec<T, N> {
    /// 从数组转换（零分配，除非 `M > N`）。
    fn from(items: [T; M]) -> Self {
        let mut out = Self::new();
        out.extend_from_slice(&items);
        out
    }
}

impl<T: Copy + Default, const N: usize> From<&[T]> for SmallVec<T, N> {
    /// 从切片转换。
    fn from(items: &[T]) -> Self {
        let mut out = Self::new();
        out.extend_from_slice(items);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_inline_until_capacity_is_exceeded() {
        let mut v: SmallVec<u32, 4> = SmallVec::new();
        assert!(v.is_inline());
        assert!(v.is_empty());
        for i in 0..4 {
            v.push(i);
        }
        assert!(v.is_inline(), "4 个元素应当还在内联存储里");
        assert_eq!(&v[..], &[0, 1, 2, 3]);
        v.push(4);
        assert!(!v.is_inline(), "第 5 个元素应当退化成堆");
        assert_eq!(&v[..], &[0, 1, 2, 3, 4]);
    }

    #[test]
    fn clear_keeps_heap_capacity_and_reuse_does_not_reallocate() {
        let mut v: SmallVec<u64, 2> = SmallVec::new();
        for i in 0..10 {
            v.push(i);
        }
        let cap = match &v {
            SmallVec::Heap(h) => h.capacity(),
            _ => unreachable!(),
        };
        v.clear();
        assert_eq!(v.len(), 0);
        for i in 0..10 {
            v.push(i);
        }
        match &v {
            SmallVec::Heap(h) => assert_eq!(h.capacity(), cap, "复用不该重新分配"),
            _ => unreachable!(),
        }
        assert_eq!(&v[..], &(0..10).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn truncate_sort_retain_match_vec_semantics() {
        let mut v: SmallVec<i32, 4> = SmallVec::new();
        for x in [3, 1, 4, 1, 5, 9, 2, 6] {
            v.push(x);
        }
        v.truncate(6);
        assert_eq!(&v[..], &[3, 1, 4, 1, 5, 9]);
        v.sort_by(|a, b| a.cmp(b));
        assert_eq!(&v[..], &[1, 1, 3, 4, 5, 9]);
        v.retain(|x| x % 2 == 1);
        assert_eq!(&v[..], &[1, 1, 3, 5, 9]);

        // 与真实 Vec 的结果对照
        let mut reference = vec![3, 1, 4, 1, 5, 9, 2, 6];
        reference.truncate(6);
        reference.sort();
        reference.retain(|x| x % 2 == 1);
        assert_eq!(&v[..], &reference[..]);
    }

    #[test]
    fn conversions_from_vec_array_and_slice() {
        let small: SmallVec<u8, 4> = vec![1u8, 2, 3].into();
        assert!(small.is_inline());
        assert_eq!(&small[..], &[1, 2, 3]);

        let big: SmallVec<u8, 2> = vec![1u8, 2, 3, 4].into();
        assert!(!big.is_inline());
        assert_eq!(&big[..], &[1, 2, 3, 4]);

        let from_array: SmallVec<u8, 4> = [9u8, 8].into();
        assert_eq!(&from_array[..], &[9, 8]);

        let from_slice: SmallVec<u8, 4> = [5u8, 6, 7].as_slice().into();
        assert_eq!(&from_slice[..], &[5, 6, 7]);
    }

    #[test]
    fn borrowed_iteration_works_like_a_vec() {
        let mut v: SmallVec<u32, 4> = SmallVec::new();
        v.extend_from_slice(&[7, 8, 9, 10, 11]);
        let doubled: Vec<u32> = (&v).into_iter().map(|x| x * 2).collect();
        assert_eq!(doubled, vec![14, 16, 18, 20, 22]);
        for x in &mut v {
            *x += 1;
        }
        assert_eq!(&v[..], &[8, 9, 10, 11, 12]);
    }

    #[test]
    fn extend_and_iteration_work_like_a_slice() {
        let mut v: SmallVec<u8, 8> = SmallVec::new();
        v.extend_from_slice(&[1, 2, 3]);
        v.extend_from_slice(&[4, 5]);
        let sum: u32 = v.iter().map(|x| *x as u32).sum();
        assert_eq!(sum, 15);
        assert_eq!(v.first(), Some(&1));
        assert_eq!(v.last(), Some(&5));
        assert_eq!(v.len(), 5);
    }
}
