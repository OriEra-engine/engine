# oriera-subsys — L6 子系统服务

> 物理、动画、音频、UI、网络同步都住这一层，v0.1 先做了物理：定步积分 + 边界回弹，刚体与关节臂在 feature 后面。

书：T9 · 清单：[`manifests/L6-subsys.yaml`](../../manifests/L6-subsys.yaml)

## 层内职责

- `PhysicsSystem`：按 `ctx.dt * TimeScale` 对 `Position`/`Velocity` 定步积分，越界回弹。重力读 `Gravity` 资源（没有就按 0 处理），时间缩放读 `TimeScale`。
- 世界由 L2 调度器每帧传进来，子系统不碰全局（书 T3 §3.8.1）。
- `rigid` feature：二维刚体；`rigid3d` feature：多体、关节臂、接触抓取与 `sim_fingerprint`（具身仿真面的基础，见 ADR 0004）。
- 无可动的东西时整趟跳过（零重力 + 零速度 + 全在界内），空跑的帧不付费。

## 核心 API

| 名称 | 作用 |
|---|---|
| `PhysicsSystem::new(w, h)` | 世界边界宽高，粒子物理的积分器 |
| `Gravity` / `TimeScale` | 资源：重力加速度、时间缩放（0 就是冻结，可见地冻） |
| `RigidBodySystem`（`rigid`） | 二维刚体 |
| `MultibodySystem`（`rigid3d`） | 多体关节、夹爪与接触承载 |
| `sim_fingerprint`（`rigid3d`） | 仿真状态的指纹，实验身份的一半 |

## 10 秒上手

```rust
use oriera_core::Scheduler;
use oriera_ecs::{Position, Velocity, World};
use oriera_subsys::PhysicsSystem;

let mut world = World::new();
let e = world.spawn();
world.insert(e, Position { x: 10.0, y: 20.0 });
world.insert(e, Velocity { vx: 1.0, vy: 0.0 });

let mut sched: Scheduler<World> = Scheduler::new().with_fixed_dt(1.0 / 60.0);
sched.add_system(Box::new(PhysicsSystem::new(800.0, 600.0)));
sched.tick(&mut world);

assert!(world.get::<Position>(e).unwrap().x > 10.0);
```

## 延伸

- [PHYSICS-FIDELITY.md](../../docs/PHYSICS-FIDELITY.md)：摩擦、惯性、自碰撞这些旋钮的取舍
- [EMBODIED-PIPELINES.md](../../docs/EMBODIED-PIPELINES.md)：仿真数据工厂的全流程
