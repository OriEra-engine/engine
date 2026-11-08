# oriera-asset — L4 资产句柄注册表

> 逻辑名字 → 句柄 → 载荷。生成式管线重发资产时换的是名字背后的字节，实体手里的句柄不用动。

书：T7 · 清单：[`manifests/L4-asset.yaml`](../../manifests/L4-asset.yaml)

## 层内职责

- 句柄 `AssetHandle(u64)` 稳定；`insert_named` 原地换载荷（hot swap），所有通过名字解析的调用方立刻看到新版本——不重建实体、不换句柄。
- 索引按 `(TypeId, 名字)` 建，所以 `hero` 网格与 `hero` 贴图可以同名共存，互不顶掉。
- `unload` 只摘这个句柄的载荷，名字注册保留：L8 重新产出后，同一个名字照样能解析。
- v0.1 是自增 id；内容寻址（哈希即身份）在 v0.2，`glb` 模块已先有了网格加载。

## 核心 API

| 名称 | 作用 |
|---|---|
| `load` | 装一份载荷，返回新句柄 |
| `get` | 按句柄取载荷，类型不符返回 `None` |
| `insert_named` / `get_named` | 逻辑名字的热替换与解析（AI 重新生成素材就走这条） |
| `unload` | 摘掉一个句柄的载荷 |
| `len` / `is_empty` | 当前驻留条数 |

## 10 秒上手

```rust
use oriera_asset::AssetStore;

let mut store = AssetStore::new();
let h = store.load(String::from("rain_v1.mp3"));

// 生成式管线重发了一版，名字不变、句柄不变
store.insert_named("bgm_rain", String::from("rain_v2.mp3"));

assert_eq!(store.get_named::<String>("bgm_rain").map(|s| s.as_str()), Some("rain_v2.mp3"));
assert_eq!(store.get::<String>(h).map(|s| s.as_str()), Some("rain_v1.mp3"));
```

## 延伸

- [`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md)：L4 在层表里的位置与依赖方向
- 生成侧（L8）怎么产出资产：[`docs/AI-NATIVE.md`](../../docs/AI-NATIVE.md)
