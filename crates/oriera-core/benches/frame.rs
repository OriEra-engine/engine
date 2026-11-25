//! L2 帧循环微基准 — `Scheduler::tick` 的固定开销。
//!
//! 场景：
//! - `frame/empty`：零系统的 tick——调度器自身地板价（计时 + allocator
//!   allocate/observe + 帧记录）；
//! - `frame/systems_n{n}`：n 个空系统（n = 1/4/16/64）——每系统的
//!   动态分发 + 计时 + grant 读取的摊销成本；
//! - `frame/systems_n16_squeezed`：16 个系统连续超订，allocator 已把
//!   throttle 压到地板——调速态下 tick 是否比未调速更贵。
//!
//! 系统体是空操作，所以量到的全是调度器自身的税，不含任何业务逻辑。
//! 这个税是每个真实帧都要付的下限：它决定了"空场景能跑多少 FPS"。
//!
//! 确定性说明：`tick` 内部用 `Instant` 计预算，但 `with_fixed_dt` 把
//! 模拟步长钉死，数字只反映调度开销，不受墙钟漂移影响。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use oriera_core::{FrameCtx, Scheduler, System};

/// 空系统：只报名字，`run` 什么都不做。
struct Nop(&'static str);

impl System<()> for Nop {
    fn name(&self) -> &str {
        self.0
    }
    fn run(&mut self, _ctx: &FrameCtx, _world: &mut ()) {}
}

fn scheduler_with(n: usize) -> Scheduler<()> {
    let mut sched = Scheduler::new().with_fixed_dt(1.0 / 60.0);
    for i in 0..n {
        // 泄漏成 &'static str：bench 进程存活期间不回收，数量有界。
        let name: &'static str = Box::leak(format!("nop{i}").into_boxed_str());
        sched.add_system(Box::new(Nop(name)));
    }
    sched
}

fn bench_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame");

    // ── 零系统：调度器地板价 ──
    group.bench_function("empty", |b| {
        let mut sched = scheduler_with(0);
        let mut world = ();
        b.iter(|| {
            black_box(sched.tick(&mut world));
        })
    });

    // ── 系统数梯度：每系统的分发 + 计时摊销 ──
    for n in [1usize, 4, 16, 64] {
        group.bench_with_input(BenchmarkId::new("systems_n", n), &n, |b, &n| {
            let mut sched = scheduler_with(n);
            let mut world = ();
            b.iter(|| {
                black_box(sched.tick(&mut world));
            })
        });
    }

    // ── 调速态：16 个系统先连跑 200 帧超订，让 allocator 压到地板 ──
    //
    // 空系统的实测耗时远低于预算，allocator 不会触发 squeeze；所以
    // 这里用 `observe_extra_cost` 注入超额虚拟成本把 throttle 压下去，
    // 再量调速态的 tick。grants 表从空变成 16 行有值——分发路径变长。
    group.bench_function("systems_n16_squeezed", |b| {
        let mut sched = scheduler_with(16);
        let mut world = ();
        for _ in 0..200 {
            for i in 0..16 {
                sched.observe_extra_cost(&format!("nop{i}"), 50.0);
            }
            sched.tick(&mut world);
        }
        let throttled: Vec<f32> = {
            let grants = sched.grants();
            (0..16)
                .map(|i| grants.throttle_of(&format!("nop{i}")))
                .collect()
        };
        assert!(
            throttled.iter().any(|t| *t < 1.0),
            "allocator must have squeezed at least one system"
        );
        b.iter(|| {
            black_box(sched.tick(&mut world));
        })
    });
    group.finish();
}

criterion_group!(benches, bench_frame);
criterion_main!(benches);
