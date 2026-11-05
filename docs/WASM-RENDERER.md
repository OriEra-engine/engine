# WASM-RENDERER — 一套物理，四个入口（PLAN-embodied-general v2.2 §3.2 / M1）

M1 把浏览器 demo 从「Rust 引擎的 JS 镜像」退役为「Rust 引擎的渲染器」：
物理、奖励、任务判定、随机化、观测、指纹全部来自编译到 wasm32 的同一份
`TableTopEnv`（与 Rust CLI、Python wheel 同源）。本文记录架构、glue 选型、
退役清单、逐位一致验收结论（含未达标项的根因定位）与已知边界。

## 1. 架构：一套物理，四个入口

```
                    ┌──────────────────────────────────────────────┐
                    │  oriera-agent::embodied_env::TableTopEnv     │
                    │  (SceneSpec::load → to_env；pick_place@v1)   │
                    └───────┬───────────┬───────────┬──────────────┘
                            │           │           │
              crates/oriera-agent   crates/oriera-agent   crates/oriera-wasm
              （examples/           （src/python.rs        （wasm-bindgen；
               embodied_sim_collect  → PyO3 oriera_sim；    cargo build
               .rs CLI 采集/验收）    maturin → wheel）      --target wasm32）
                            │           │           │
        ┌───────────────────┘           │           └──────────────────┐
        ▼                               ▼                              ▼
   Rust CLI 跑分/金标            Python/Gymnasium/SB3          vendor/oriera/*.wasm
   （fingerprint 口径源）        （训练/评测，venv 可装）       examples/15-embodied-sim
                                                                engine/kernel.js（渲染器）
                                                                node tools/wasm_golden_check
```

- **kernel.js 现在只做四件事**：three.js 渲染（每帧读 `sim.poses()` 搬
  mesh）、交互（键盘/滑条/相机/录制）、postMessage 桥 + CDP 钩子
  （`__orieraSim.state/stats`，键名语义不变，数据源改 wasm）、一段
  「渲染层策略」（PHASES+ik 只喂目标角，不参与判定）。
- **场景也是 spec 驱动的**：fixtures（地板/桌子）、机器人 link 形状
  （盒/胶囊映射）、夹爪指板、prop 形状（立方/圆柱/球）、目标圈半径
  （`task.params.goal_xz_tol`）、滑条关节名与限位，全部从
  `sim.spec_json()` 生成——改 JSON 换方块形状，浏览器页直接可见（M0a
  验收④在浏览器侧成立）。
- **入口同一性**：`OrieraSim.from_spec_json(preset_spec_json())` 的
  `spec_hash` 与 `crates/oriera-embodied/specs/tabletop-4dof-pick.json` 一致
  （kernel.test.mjs 钉死）；demo 支持 `?spec=<url>` 与 postMessage
  `{cmd:"spec", json}` 换装（Studio 三库 M2 钩子）。

## 2. glue 选型（wasm-bindgen 产物怎么来的）

- 依赖纪律：oriera-wasm 只依赖 `oriera-agent(+rigid3d,spec)`、
  `wasm-bindgen 0.2`、`serde_json`（经 workspace）。robot/scene 类型全部
  经 `TableTopEnv` 公开字段访问，不新增 oriera-subsys 直接依赖；link/prop
  **形状**以 `spec_json()` 文本交给渲染器，wasm 侧从不点名 subsys 枚举。
- **官方 `wasm-bindgen-cli` 在本机装不上**（两试皆败，非网络问题）：
  本机 rust 工具链是 `stable-x86_64-pc-windows-gnu`，CLI 链接期在
  windows-sys/getrandom 的 `raw-dylib` 导入库上死（GNU ld 报
  `undefined reference to _head_*_dll_imports_lib`；MSVC 侧无 link.exe）。
- **采取的路径（产物仍是官方 glue）**：写了一个最小生成器
  （`wasm-bindgen-cli-support =0.2.129` 直接做库用，它没有 raw-dylib 依赖
  树），等价于 `wasm-bindgen --target web / --target no-modules`
  （`remove_name_section`+`remove_producers_section`，与官方 CLI 同参）。
  两个 target 的 `_bg.wasm` 字节一致，vendor 里只放一份 wasm：
  - `vendor/oriera/oriera_wasm.js` + `oriera_wasm_bg.wasm` + `.d.ts`
    （`--target web`，ESM；kernel.js 与 node 测试共用）。
  - `--target no-modules`（经典脚本全局 `wasm_bindgen`，Studio 若要
    非 ESM 静态加载用它；`wasm_bindgen({ module_or_path: ... })` 指定
    wasm 路径）。
- **复现命令**（M0b 落地后重打 vendor 时用）：

  ```bash
  cargo build --release --target wasm32-unknown-unknown -p oriera-wasm \
    # 体积版：CARGO_PROFILE_RELEASE_OPT_LEVEL=z CARGO_PROFILE_RELEASE_LTO=fat \
    #         CARGO_PROFILE_RELEASE_PANIC=abort CARGO_PROFILE_RELEASE_STRIP=symbols
  wbg-driver target/wasm32-unknown-unknown/release/oriera_wasm.wasm \
             examples/15-embodied-sim/vendor/oriera web --typescript
  ```

- 报告项：本次交付的 wasm 由**干净 HEAD 35e3dad 快照 + 本 crate** 构建
  （与并行在改 oriera-agent 的 M0b 解耦）；`preset_spec_json`/`layout` 等
  全部 API 都在 HEAD 公开 API 上。

## 3. 退役清单

### 3.1 kernel.js 删掉的物理（HEAD 866 行 → 674 行；渲染器新增了 spec 驱动布景）

| 退役项 | 替代 |
|---|---|
| `makeWorld`（rapier world/碰撞组/链体/关节马达/方块/指板，~90 行） | wasm 内 Rust 构建（含全部历史修复：碰撞组、TCP 转录、接触弹射……本来就修在 Rust 侧） |
| 马达代偿 `MOTOR_KP=960/MOTOR_KD=30`、`SUBSTEPS=2`、`numSolverIterations=100` | 不再需要——multibody 马达是 rapier 原生能力，wasm 里就是 Rust 的 kp=60/kd=4 |
| `stepOnce/readAngles/eePose/observe/updateHolding/envStep`（奖励/终止/截断手写镜像） | `sim.step()`（StepResult 五元组）；判定/奖励在 Rust |
| `lcgNext` + 种子画位姿（obj/goal 抽签镜像） | wasm `reset(seed)`（同一 LCG，抽签顺序=引擎顺序） |
| 原地 reset 的 9 个 `setTranslation` 瞬移 + 指板瞬移（wasm 内存泄漏修复的 JS 版） | wasm `reset` 每集重建（Rust 分配器，无泄漏面） |
| 收敛门控相位机（dwell/err 逃逸阀——旧 impulse 链软化误差的 JS 代偿） | 开环固定节拍 `policyTick(PHASES, step)`（=Rust collector round-4 同形） |
| 常量表 `DESC/FINGER_*/TABLE/OBJ_RANGE/GOAL_RANGE/ZONE_RADIUS/SETTLE_TICKS/MAX_STEPS/OBS_NAMES/ACTION_NAMES`（14 个 parity 守卫的锚） | spec 本身（`spec_json()`）；parity 由「同一份代码」构造性保证 |

### 3.2 为什么 14 个 parity 守卫可以退役

旧守卫逐个断言「JS 常量 == Rust 常量」（FINGER_HALF/FINGER_X/GAP/GRASP_
RADIUS/PROP_HALF/肘限位/DESC.links/kp/kd/SETTLE_TICKS/MAX_STEPS/DT/SUBSTEPS）。
物理退役后这些常量在浏览器侧**不存在了**——不存在第二个实现，就没有可漂移
的东西。新测试集（kernel.test.mjs，16 项）守的是接口契约：preset==spec 文件、
Gymnasium 五元组、双跑逐位一致、FK TCP≈物理遥测、布局名、坏 spec 可读报错。

### 3.3 保留（M0a 硬闸）

HUD 文案（含 G-5「作业证据一律以 CLI eval_report 为准」）、导出按钮独立
打开置灰 + CLI 指引（G-8）、标题与整体 UI、postMessage 桥、CDP 钩子、
键盘/滑条遥操作、退化重抽 displaced-seed 语义、录制卫生（回合末先切
录音带再重置）。

## 4. 逐位一致验收：结论与根因（如实入册）

`tools/wasm_golden_check.mjs`（node 侧代理）：跑 collect 协议（seed0=1000、
退化重抽 displaced band、12 集、collector 同款脚本基线），两个独立 sim
实例双跑 + 与两个参照逐集比对。

**PASS：wasm 内部确定性（同目标）。**
- 双跑逐位一致（指纹/steps/success/reward 全等）；
- 不同构建目录的两份 wasm（工作树构建 vs 干净 HEAD 快照构建）同 seed
  逐位一致；浏览器与 node 共用同一份 `_bg.wasm`。

**未逐位：native(x86_64) ↔ wasm32。** 三步定位：
1. 基线策略循环下 step 0 即分歧：动作本身就差 1 ulp——`ik()` 的
   `atan2f/acosf`：native（UCRT）与 wasm32（compiler-builtins 的 Rust
   libm 移植）不同实现，末位不同；
2. **换成固定动作序列**（两侧完全相同的 5 维动作 ×700 步）仍分歧：
   step 0 出现 1 ulp 的肘速度差——rapier/nalgebra 的求解器数学在
   x86_64（SSE/gemm 求和顺序）与 wasm32（标量）上不同；
3. 打开 rapier `enhanced-determinism`（simba/libm_force + parry 同开关）
   重建：native 侧指纹改变（开关生效）但仍与 wasm 相差 1 ulp
   （残留怀疑 matrixmultiply gemm 内核，M1 不再深挖）。

**统计口径对照（同 spec 同 seed0，12 集逐集）：**
- seed band 逐集一致（含 1001 退化重抽）；
- 成功率 wasm 10/12（83.3%）vs native 11/12（91.7%）——seed 66546 一集
  被末位噪声翻转（其余 11 集胜负一致、步数差 ≤±5）；Wilson 95% CI
  重叠。**M1 验收「浏览器 12 集 = Rust 91.7%」未达，实际 83.3%，如实
  记录**；真浏览器 12 集由主控前台复验。
- 结论（PLAN §五 风险落地）：「逐位一致」契约收敛为
  **「同目标逐位 + 跨目标统计一致」**，正是 M3 已规划的跨平台指纹矩阵
  议题。若未来要跨目标逐位，需：两侧统一 IK/物理数学（例如 wasm 与
  native 都走 Rust `libm`、禁 native gemm 内核），金标按 M0b 的
  `physics-behavior-change` 流程重锚——属独立工作包，M1 未动。

**金标口径说明**：pre-m0a 金标的 hash = `env.fingerprint()`
（=`sys.fingerprint(world)`）逐集；wasm 侧同名方法 `sim.fingerprint()`
返回十进制 u64 字符串（JS Number 装不下 64 位，工具按字符串逐字符比对）。
本次 HEAD 35e3dad 的原生收集器逐集复现了 pre-m0a 金标 12/12
（`/tmp ref-native` 运行证据），证明 HEAD 物理未漂移；wasm 侧差异全部
来自上述跨目标数学，而非引擎改动。

## 5. bundle 尺寸（如实报）

| 项 | 字节 |
|---|---|
| `vendor/oriera/oriera_wasm_bg.wasm` | **1,592,418 B（1.52 MiB）** |
| `vendor/oriera/oriera_wasm.js`（glue） | 25,678 B |
| `vendor/oriera/*.d.ts` | 9.6 KB |
| 退役（仍在盘上待删，加载路径已退役）：`vendor/rapier.mjs` | 4,340,292 B |
| 退役：`vendor/rapier_wasm3d_bg.wasm` | 3,082,103 B |

- 目标「增量 <1.5MB」：绝对值 1.52 MiB，**超 1.2%**（opt-level=z + fat
  LTO + panic=abort + strip 后已是最小可用档；wasm-opt 不可得——github
  不通、binaryen 源码编译成本高）。如实记录。
- **demo 静态资源净变化 −5.54 MiB**（退役 7.08 MiB rapier 资产 vs 新增
  1.55 MiB）——bundle 总量是净减的。

## 6. 吞吐（node / python，如实报）

| 路径 | 步速 |
|---|---|
| node + wasm（预热后，接触丰富段） | ~1,000 steps/s（冷启动首集 ~220；零动作 2,367） |
| node + wasm（12 集验收全流程均值，含 JIT 预热） | 335–386 steps/s |
| python wheel（干净 venv，单环境，零动作 700 步循环） | 5,500–10,100 steps/s |
| python 单进程（OrieraEnv + gym 包装，release pyd） | 3,416–5,494 steps/s（LAB 入册参考 13,458——测量口径不同，如实并列） |
| `recipe_subproc_vecenv.py` SubprocVecEnv×4（Windows spawn） | 聚合 1,636 steps/s（0.30×单进程——**负结果**：0.2ms 级的物理步吃不起每步一次的 pickle IPC） |
| DummyVecEnv×4（同进程向量化对照） | 聚合 1,851 steps/s |

结论：4 进程并行的收益要在**重负载**（RGB 渲染 obs、大场景）下才成立；
纯物理小步长环境在 Windows 上优先单进程多实例（或按 seed 分进程跑整集，
让 IPC 只发生在集边界——collector 的 12 集就是这么并的）。

## 7. 已知边界与 API 缺口（报告项，未改 oriera-agent）

1. **prop 旋转不可见**：`Pose3.rot` 在 `TableTopEnv` 私有字段里，公开 API
   只有 `cube_pos()`。渲染器方块的**位置精确、姿态恒等**（`poses()` 里
   标了 `"rot_known": false`）。建议补 `pub fn prop_pose(&self, i) -> Pose3`。
2. **夹爪指板为镜像**：`MultibodySystem::gap_now`/`fingers` 私有；
   oriera-wasm 按同一 ramp 公式镜像（`gap_open/closed/rate` 来自 spec，
   每步推进）。建议补 `pub fn debug_contact_state` 转发或 `pub fn gap()`。
3. **obs_noise>0 时 FK 用带噪关节角**（观测通道是唯一公开角度源）；
   建议补 `pub fn angles()`（真值）。默认 spec obs_noise=0，无影响。
4. 杂物 props（props[1..]）无公开读数——渲染器只画 specs 里能读到的
   props[0]（默认场景只有它）。
5. `pyproject.toml` 两处 maturin 配置 bug（未改，验证用临时副本构建）：
   `features = ["pyo3/python"]` → pyo3 无 `python` feature，应为
   `["python"]`（oriera-agent 的 feature）；`python-source = "scripts"`
   → maturin 要求 `scripts/oriera_sim/` 包目录存在，当前打包需去掉该项
   （oriera_gym.py 仍可经 LAB-GUIDE 的 target/release 路径使用）。
6. wasm 模块里 `fingerprint()` 返回字符串（u64 精度）；LeRobot 导出、
   eval_report 等仍在 CLI/wheel 侧。
7. 浏览器 60Hz 主循环每帧 `JSON.parse(poses())`（~2KB）+ 一次 wasm 调用
   集合——在 1,000 steps/s 的物理余量下不是瓶颈；若未来上多机器人场景
   再换 packed Float32Array 通道。

## 8. 验收命令速查

```bash
cargo check --target wasm32-unknown-unknown -p oriera-wasm
node --test examples/15-embodied-sim/engine/kernel.test.mjs          # 16 项
node examples/15-embodied-sim/tools/wasm_golden_check.mjs \
     --rust-ref <dir>      # <dir> = embodied_sim_collect --episodes 12 --out <dir> 的产物
node examples/15-embodied-sim/tools/wasm_golden_check.mjs --policy js   # JS 渲染层策略口径
python scripts/recipe_subproc_vecenv.py --n 4 --steps 2000
# wheel（pyproject 修复后）: python -m maturin build --release -o dist/
python -m venv .venv-wheel && .venv-wheel/Scripts/pip install dist/*.whl
# 浏览器: examples/15-embodied-sim/index.html（?spec=<url> 可换实验；?policy=wasm 切同代码逐位口径）
```
