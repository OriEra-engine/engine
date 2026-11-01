# LAB-GUIDE — OriEra 具身仿真台 · 学生实验指导书

> 面向「机器人学导论」类课程的一周实验。目标：跑通官方基线 → 写你自己的
> 策略 → 交一份带置信区间的评测报告。全程本地零 API 消耗。

## 1. 环境准备（≈20 分钟）

**先装 Python 依赖**（一次到位，避免后面卡住）：
```bash
pip install gymnasium numpy stable-baselines3 pyarrow imageio imageio-ffmpeg
```

- Rust（rustup 默认工具链即可）+ Python 3.10+；
- `cargo test -p oriera-subsys --features rigid3d,cam` 全绿即环境 OK；
- 构建训练桥：`cargo build -p oriera-agent --features python`
  → 产物 `target/release/oriera_agent.dll`，复制为 `target/release/oriera_sim.pyd`
  （Linux/macOS 为 `liboriera_sim.so` → `oriera_sim.so`）；
- 跑通官方基线（这同时也是交作业用的命令）：
  ```bash
  cargo run -q -p oriera-agent --example embodied_sim_collect \
      --features rigid3d,cam -- --task pick_place --episodes 12 --out out/my-run
  ```
  看到 `≥90%` 硬闸通过即基线正常（引擎内置，低于 90% 会直接报错——
  你不需要也不应该改这个闸）。

> **Windows 工具链提示**：rustup 默认安装 MSVC 工具链（需要 VS Build
> Tools 的 link.exe）。如果你没有装 Visual Studio，改用 GNU 工具链：
> `rustup default stable-x86_64-pc-windows-gnu`（需安装 MinGW-w64）。

## 2. 看懂评测报告（eval_report.json）

| 字段 | 含义 |
|---|---|
| `success_rate` | 12 集成功比例（脚本基线应 ≈0.92） |
| `wilson95` | 成功率的 Wilson 95% 置信区间——报告里**必须**引用区间而非裸比例 |
| `runs[].seed` | 每集种子：同种子必复现同结局（作业可核验性） |
| `runs[].rand` | 每集域随机化抽签（密度/摩擦/增益/质量——做消融实验用） |
| `randomization` | 本批 DR 档位概要 |

## 3. Python 接入（写你自己的策略）

两层入口，任选：
- **gymnasium 标准**（推荐，接 SB3/lerobot）：
  ```python
  import sys; sys.path.insert(0, "target/release")
  from oriera_gym import OrieraPickPlaceEnv
  env = OrieraPickPlaceEnv(task="pick_place", kp_jitter=0.2, mass_jitter=0.15)
  obs, info = env.reset(seed=1000)          # float32[21]
  obs, r, term, trunc, info = env.step(env.action_space.sample())
  ```
- **原生 PyO3**（更细控制）：`oriera_sim.PickPlaceEnv(...)`，见
  `crates/oriera-agent/src/python.rs` 头注释。

常见导入错误：
| 症状 | 原因 | 处置 |
|---|---|---|
| `ModuleNotFoundError: oriera_sim` | pyd 没复制或路径没插 | 确认 dll→pyd 已复制且 `sys.path` 指向 target/release |
| `DLL load failed` | pyd 构建时 Python 版本 ≠ 运行时 | 用同一 Python 版本重跑 `cargo build --features python` |
| `unknown task` | 合法名 `reach/pick_place/push`，也接受自然短语 | 见 `python.rs` 文档 |

## 4. 观测与动作（21 维 / 5 维）

- obs 前 20 维是浮点通道（关节角×4、速度×4、夹爪、末端位姿×3、
  方块相对×3、目标相对×3、高度与距离），第 21 维语义见
  `crates/oriera-agent/src/embodied_env.rs` 的 `OBS_NAMES`；
- action = 4 个关节目标角（rad）+ 夹爪（0 开 / 1 合）；引擎侧 PD 马达
  （ForceBased）负责跟踪，你只管给目标；
- reward：reach/pick_place/push 三任务各有密集塑形 + 成功大额奖励；
  `info["success"]` 是评测判据。

## 5. 训练作业（二选一）

- **模仿学习**：遥操作/脚本采集 → `python scripts/jsonl_to_lerobot_v21.py
  <目录>` → `lerobot-train --dataset.repo_id=<user>/<ds> --policy.type=act`
  （消费级 GPU 即可，如 RTX 3060）；
- **强化学习**：SB3 PPO 挂 `OrieraPickPlaceEnv`（经 gymnasium 包装层）。
  一键训练器已备好（固定评测 seed 1000-1011 + 曲线落盘）：
  ```bash
  python scripts/train_ppo.py --task reach --steps 100000 --dr
  ```
  产物在 `out/rl-training/`（eval_curve.json + ppo_model.zip）。
  域随机化三旋钮（kp/mass/obs_noise）做对照实验。无 GPU 的学生做 RL
  路线（MlpPolicy 纯 CPU 即可），有 GPU 的做 IL 路线。

  **实测预期（2026-11-01，别被 0% 吓住）**：reach 100k 即可逼近脚本
  基线回报（-213→-46）；**pick_place 用 vanilla PPO 学不会**——
  100k 回报冲到最优（≈-20，悬停行为）后 500k 一直在平台期震荡、
  成功率 0%：抓取的奖励鸿沟靠密集塑形填不平，正道是 IL 路线（脚本
  采集 91.7% 数据 → ACT/π0 微调）或 RL 加课程表/HER（进阶作业）。

## 6. 评测规范与思考题

- **规范**：≥12 集 + Wilson 95% CI；引用引擎指纹回执证明可复现；
- 思考题：①为何某些种子「生成即入区」被引擎剔除？②push 任务为何不设
  90% 硬闸？③`meta/oriera_receipt.json` 的逐集指纹在打分中如何防作弊？
  ④域随机化的 `profile=off` 与 `default` 跑出的策略有何预期差异？

## 7. 提交清单与评分细则

**提交**：`out/my-run/` 全目录（含 eval_report.json + receipt + 数据集）
+ 策略代码 + ≤2 页报告。

**建议分值**：基线复现 20 / 策略提升 40 / 规范引用（CI/Wilson）20 / 思考题 20。

## 附：问题排查

| 症状 | 处置 |
|---|---|
| import 找不到 oriera_sim | 确认 dll→pyd 已复制且 `sys.path` 指向 target/release |
| 基线 90% 闸报错 | 先 `cargo build` 确认无改动；不要改 limits/增益参数 |
| 相机视频打不开 | `pip install imageio imageio-ffmpeg` 后重跑转换脚本 |
| 想看浏览器 demo | 需静态服务器：`python -m http.server -d examples`（file:// 不行）|
| 想用 Studio 工作台 | Studio 侧栏「具身仿真台」→ /sim 路由 |
