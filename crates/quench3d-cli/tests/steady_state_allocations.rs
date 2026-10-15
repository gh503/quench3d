//! 稳态分配回归测试：物理世界跑热之后，每一步**不允许有任何堆分配**。
//!
//! 这是「低资源占用」的硬约束——早期实现每个接触对都要分配几个 `Vec`，
//! 2000 体场景每步分配 3 万次；现在稳态是 0 次。这个测试用计数分配器把它钉死，
//! 任何人再往热路径里塞 `Vec::new()` / `collect()` / `to_string()` 都会在这里失败。
//!
//! 说明：为了统计分配，本文件（独立测试 crate）用 `unsafe impl GlobalAlloc`
//! 包了一层计数器；引擎本体仍然是 `#![forbid(unsafe_code)]`。
//! 这个文件里**只放一个测试**——测试二进制内部并行跑多个测试会让计数器互相干扰。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quench3d::math::Vec3;
use quench3d::physics::{BodyBuilder, Joint, PhysicsWorld, Shape};

/// 计数分配器。
struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(new_size, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// 一个有堆叠、有斜坡、有球体与关节的混合场景（覆盖宽相/窄相/求解器/关节各条路径）。
fn scene(bodies: usize) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    world.set_gravity(Vec3::new(0.0, -9.81, 0.0));
    // 关闭睡眠：让每一步都真的在算，否则「全睡着」会掩盖分配问题
    world.config.sleep_enabled = false;
    world.add_body(
        BodyBuilder::r#static(Shape::Plane {
            normal: Vec3::new(0.0, 1.0, 0.0),
        })
        .build(),
    );
    let side = 10usize;
    let mut made = 0usize;
    let mut layer = 0usize;
    while made < bodies {
        for i in 0..side {
            for j in 0..side {
                if made >= bodies {
                    break;
                }
                // 一半箱子一半球体，覆盖不同窄相路径
                let shape = if made.is_multiple_of(2) {
                    Shape::Box {
                        half_extents: Vec3::splat(0.4),
                    }
                } else {
                    Shape::Sphere { radius: 0.35 }
                };
                world.add_body(
                    BodyBuilder::dynamic(shape)
                        .position(Vec3::new(
                            (i as f32 - side as f32 * 0.5) * 0.95,
                            0.4 + layer as f32 * 0.85,
                            (j as f32 - side as f32 * 0.5) * 0.95,
                        ))
                        .build(),
                );
                made += 1;
            }
        }
        layer += 1;
    }
    // 一对关节，覆盖关节求解与条目缓冲
    let a = world.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::splat(0.1),
        })
        .position(Vec3::new(-6.0, 2.0, 0.0))
        .build(),
    );
    let b = world.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::new(0.3, 0.1, 0.1),
        })
        .position(Vec3::new(-5.4, 2.0, 0.0))
        .build(),
    );
    world.add_joint(Joint::Distance {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        rest_length: 0.6,
        stiffness: 0.0,
        damping: 0.0,
    });
    world
}

#[test]
fn steady_state_step_allocates_nothing() {
    for bodies in [50usize, 400] {
        let mut world = scene(bodies);
        // 这里守的是**求解器热路径**的零分配。并行求解会按步分发线程，
        // 而 `std::thread::scope` 的簿记本身就要分配——那是另一类开销，
        // 所以先关掉并行，把「求解器内部一次都不分配」钉死。
        world.config.parallel = false;
        // 预热：让所有复用缓冲扩容到位、场景进入稳态
        for _ in 0..120 {
            world.step(1.0 / 60.0);
        }

        // 关掉「抓取分配点」之类的干扰，纯粹数这一步的分配次数
        let before_allocs = ALLOCS.load(Ordering::Relaxed);
        let before_live = LIVE.load(Ordering::Relaxed);
        for _ in 0..60 {
            world.step(1.0 / 60.0);
        }
        let allocs = ALLOCS.load(Ordering::Relaxed) - before_allocs;
        let live_delta = LIVE.load(Ordering::Relaxed) as i64 - before_live as i64;

        assert_eq!(
            allocs, 0,
            "{bodies} 体场景预热后 60 步里发生了 {allocs} 次堆分配——热路径上不允许分配"
        );
        assert!(
            live_delta.abs() < 64 * 1024,
            "{bodies} 体场景活动内存变化 {live_delta} 字节，疑似泄漏或缓冲无限增长"
        );

        // 并行路径：分配**允许存在但必须有界、且不随步数增长**。
        // 这一条能抓住「布局缓冲跨步累积」那类错误（曾经让一个 240 步的测试
        // 从 0.5 秒变成 36 秒，因为每步都在往同一个 Vec 末尾追加）。
        world.config.parallel = true;
        for _ in 0..120 {
            world.step(1.0 / 60.0);
        }
        let p_allocs = ALLOCS.load(Ordering::Relaxed);
        let p_live = LIVE.load(Ordering::Relaxed);
        for _ in 0..60 {
            world.step(1.0 / 60.0);
        }
        let p_delta = ALLOCS.load(Ordering::Relaxed) - p_allocs;
        let p_live_delta = LIVE.load(Ordering::Relaxed) as i64 - p_live as i64;
        assert!(
            p_delta <= 60 * 1024,
            "{bodies} 体场景并行路径 60 步分配了 {p_delta} 次，明显超出线程分发的量级"
        );
        assert!(
            p_live_delta.abs() < 512 * 1024,
            "{bodies} 体场景并行路径活动内存变化 {p_live_delta} 字节，缓冲疑似跨步累积"
        );

        // 顺手确认世界仍然健康（优化不能把物理算坏）
        let mut nan = 0usize;
        let mut moving = 0usize;
        for (_, body) in world.bodies() {
            if !body.position().is_finite() || !body.linear_velocity.is_finite() {
                nan += 1;
            }
            if body.linear_velocity.length() > 1e-3 {
                moving += 1;
            }
        }
        assert_eq!(nan, 0, "{bodies} 体场景出现 NaN");
        assert!(moving > 0, "{bodies} 体场景全部静止，测试可能没在真的算");
    }
}
