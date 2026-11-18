# oriera-ecs — L3 对象模型

> 实体是 `(index, generation)` 而不是裸指针；组件是每类型一列，按下标直接访问，查询不打哈希。

书：T6 · 清单：[`manifests/L3-ecs.yaml`](../../manifests/L3-ecs.yaml)

## 层内职责

- **实体 = 索引 + 代际**：`despawn` 后代际 +1，索引可以被复用，但旧句柄永远取不到东西——use-after-free 变成可测的 `None`。
- **组件 = 每类型一列** `Vec<Option<T>>`：O(1) 随机访问，`Option` 同时兼作「有没有这个组件」。
- **资源 = 按类型单例**（如 `Gravity`）：跨 system 的共享状态走资源，不走全局变量（书 T3 §3.8.1）。
- **`state_hash`**：位级状态指纹，回放、多端一致校验、金标锚都用它。
- v0.1 是扁列 ECS，不是多 archetype：加删组件不搬迁表，公开 API 到 v0.2 不变。

## 核心 API

| 名称 | 作用 |
|---|---|
| `spawn` / `despawn` | 拿实体 / 立刻清空它的所有列并复用索引 |
| `insert` / `get` / `get_mut` | 组件读写；句柄失效返回 `None` |
| `for_each2_mut` | 两个组件的可变遍历，热点路径用它 |
| `insert_resource` / `resource` | 类型单例的写入与读取 |
| `state_hash` | 位级状态指纹，回放与多端一致性校验都靠它 |

## 10 秒上手

```rust
use oriera_ecs::{Position, Velocity, World};

let mut world = World::new();
let e = world.spawn();
world.insert(e, Position { x: 1.0, y: 2.0 });
world.insert(e, Velocity { vx: 0.5, vy: 0.0 });

world.for_each2_mut::<Position, Velocity>(|_e, p, v| p.x += v.vx);
assert_eq!(world.get::<Position>(e).unwrap().x, 1.5);

world.despawn(e);
assert!(world.get::<Position>(e).is_none()); // 旧句柄不再有效
```

## 延伸

- [ARCHITECTURE · L3](../../docs/ARCHITECTURE.md#l3--oriera-ecs--对象模型)
- 上层的用法：[`oriera-subsys`](../oriera-subsys/README.md)、[`oriera-render`](../oriera-render/README.md)
