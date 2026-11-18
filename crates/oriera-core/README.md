# oriera-core — L2 帧调度与确定性

> 谁按什么顺序跑、这一帧还剩多少毫秒、同一串命令能不能跑出同一个世界。这三件事都在这里，且只在这里。

书：T5 · 清单：[`manifests/L2-core.yaml`](../../manifests/L2-core.yaml)

## 层内职责

- `Scheduler<W>` 对世界类型泛型：L2 不认识 `World`。应用壳实例化调度器、持有世界，每帧把 `&mut W` 显式交给每个 system。
- `FrameCtx` 带这一帧的帧号、`dt`、时间戳和算力配额。配额由 `BudgetAllocator` 按观测到的耗时切分，吃预算的系统自己按 grant 缩工作量。
- `with_fixed_dt` 把 `dt` 钉成常数：回放和评测从此与墙钟无关，同一串命令在不同负载下跑出同一个世界（issue #66）。

## 核心 API

| 名称 | 作用 |
|---|---|
| `Scheduler::new` / `with_budget_ns` | 默认按 60fps 评级；90fps 的端用后者传自己的预算 |
| `add_system` | 注册 `Box<dyn System<W>>`，system 名字在此时捕获 |
| `tick` | 跑一帧，返回本帧实际耗时（ms） |
| `system_timings` / `avg_frame_ms` | 「这一帧被谁吃了」与滚动均值 |
| `observe_extra_cost` | 把 GPU、网络这类外挂成本并入下一次分摊 |

## 10 秒上手

```rust
use oriera_core::{FrameCtx, Scheduler, System};

struct Ticks { n: u32 }
impl System<()> for Ticks {
    fn name(&self) -> &str { "ticks" }
    fn run(&mut self, _ctx: &FrameCtx, _world: &mut ()) { self.n += 1; }
}

let mut scheduler: Scheduler<()> = Scheduler::new().with_fixed_dt(1.0 / 60.0);
scheduler.add_system(Box::new(Ticks { n: 0 }));
scheduler.tick(&mut ());
assert!(scheduler.fixed_dt().is_some());
```

## 延伸

- [ARCHITECTURE · L2](../../docs/ARCHITECTURE.md#l2--oriera-core--调度器与确定性)
- [COMPUTE-BUDGET.md](../../docs/COMPUTE-BUDGET.md)：预算切分与降级的完整推导
