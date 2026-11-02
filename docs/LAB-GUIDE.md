# LAB-GUIDE — OriEra 具身仿真台 · 学生实验指导书

> 面向「机器人学导论」类课程的一周实验。目标：跑通官方基线 → 写你自己的
> 策略 → 交一份带置信区间的评测报告。全程本地零 API 消耗。

**命令怎么照抄**：所有命令都在引擎仓库根目录运行（`engine\` 这一层，能看到
`README.md`、`Cargo.toml` 和 `examples` 文件夹）。每个命令块前标了适用环境：

- **任意终端**：cmd / PowerShell / Git Bash / macOS / Linux 通用（`cargo`、
  `python` 的参数里写 `/`，Windows 也认）；
- **cmd / PowerShell**：Windows 自带终端，路径用 `\`；
- **Git Bash**（以及 macOS / Linux）：路径用 `/`。

长命令一律写成一行（cmd 不认 `\` 续行），整行复制即可。

## 0. 三条路线图（先看到臂动，再装环境）

| 步 | 你会得到什么 | 要装什么 | 大约多久 |
|---|---|---|---|
| **第 0 步** 浏览器看臂动 | 一台能遥控、也能自己抓放的机械臂 | 只要 Python 3.10+ | 10 秒 |
| **第 1 步** CLI 跑基线、交作业 | `eval_report.json` 等全部作业证据 | Rust + Python 依赖（§1） | Windows 见 §1 预估 |
| **第 2 步** Studio 图形界面 | 不敲命令录制、一键导出训练数据 | OriEra Studio 桌面端 | 下载安装 |

还没拿到代码就先 clone（任意终端；国内网络慢可用 README「鸣谢」节的 AtomGit 镜像；
没装 Git 也可以在 GitHub 页面点 Code → Download ZIP 解压，再 `cd` 进解压出的文件夹）：
```
git clone https://github.com/OriEra-engine/engine.git
cd engine
```

**第 0 步：10 秒看臂动**。任意终端：
```
python -m http.server 8124 -d examples
```
浏览器打开 **http://localhost:8124/15-embodied-sim/** → 点「▶ 脚本策略演示」看它
自己把方块放进绿圈；或按 Q/A、W/S、E/D、R/F 遥控四个关节，空格开合夹爪。
跑服务器的终端窗口别关（关了网页就断），用完在该窗口按 `Ctrl+C` 停止。

> 浏览器页是「看手感」的演示版：它的物理内核与引擎基线口径不同，右上角
> 「成功 N/M」通常比 91.7% 低——**作业的数字一律以第 1 步 CLI 的
> `eval_report.json` 为准**（页面右上角也有这行小字）。独立打开时「导出」按钮
> 是灰的，只有在 Studio 里才能用。

**第 1 步：CLI 跑基线、交作业**。按 §1 装环境、跑基线；交作业的全部证据都从这里
来（§6 有「报告里贴什么」模板）。

**第 2 步：Studio 图形界面**。[OriEra Studio](https://github.com/OriEra-engine/studio/releases/latest)
侧栏「具身仿真台」（/sim 页）里可以录制、一键导出 LeRobot 训练数据。这个入口
2026-10-30 才加入，你的 Studio 侧栏没有这一项说明版本较旧——先用第 0、1 步，
作业不受影响。

## 1. 环境准备（Windows：已装 VS Build Tools 约 20 分钟内跑通基线；没装另加 30–60 分钟）

**时间花在哪**（4 核以上笔记本、网络通畅时的估计）：装 Rust 约 5 分钟；第一次编译
引擎几分钟（之后改代码再编译只要几秒）；`pip` 首次要下载 PyTorch（约 200 MB，
stable-baselines3 依赖它）。下面第 1 到 4 条大约 20 分钟走完；第 5 条（Python 训练桥，
release 构建）再加 5–10 分钟。

**Windows 编译器：MSVC 还是 MinGW？** rustup 在 Windows 默认装 MSVC 工具链，链接要用
VS Build Tools 里的 `link.exe`：

- **装过 Visual Studio 或 VS Build Tools**：什么都不用改，直接往下做；
- **没装**：首选装 [VS Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)，
  勾选「使用 C++ 的桌面开发」（下载 2–3 GB、占盘 6–8 GB、需要管理员权限，网速
  决定 30–60 分钟）；
- **只有这几种情况才值得改用 MinGW**：没有管理员权限、C 盘剩余不到 10 GB、或
  2–3 GB 实在下不动。MinGW-w64 只有几百 MB、解压即用，但要自己把它的 `bin` 目录
  加进 PATH，然后切到 GNU 工具链（任意终端）：
  ```
  rustup default stable-x86_64-pc-windows-gnu
  ```
  GNU 工具链用于跑 CLI 基线（第 1 步）；第 5 条的 Python 训练桥没在 GNU 工具链下
  验证过，遇到链接报错请回到 VS Build Tools。

**Windows 三个常见坑**：

- **pip 装到了别的 Python**：电脑上有多个 Python 时，`pip` 可能不指向你运行的那个
  → 一律用 `python -m pip install ...`（本指南的命令都是这种写法）。
- **PATH**：装完 Rust/Python 要**新开一个终端**（旧终端看不到新 PATH）；输入
  `python` 却弹出微软商店 → 用 python.org 安装包装 Python 并勾选「Add python.exe
  to PATH」，再到「设置 → 应用 → 高级应用设置 → 应用执行别名」关掉 python.exe 和
  python3.exe 两项。
- **长路径**：仓库放在短路径（如 `D:\oriera\engine`），别放桌面深处、别带中文或空格；
  `git clone` 报 `Filename too long` 时先运行 `git config --global core.longpaths true`
  再 clone。

**装好后逐条执行**：

1. 确认工具都在（任意终端；两条各输出一个版本号就对了）：
   ```
   cargo --version
   python --version
   ```
2. 装 Python 依赖（任意终端，一次到位，避免后面卡住）：
   ```
   python -m pip install gymnasium numpy stable-baselines3 pyarrow imageio imageio-ffmpeg
   ```
3. 自检物理引擎（任意终端；输出里每组都是 `test result: ok`、没有 `FAILED` 即环境 OK）：
   ```
   cargo test -p oriera-subsys --features rigid3d,cam
   ```
4. 跑通官方基线（任意终端；这同时也是交作业用的命令）：
   ```
   cargo run -q -p oriera-agent --example embodied_sim_collect --features rigid3d,cam -- --task pick_place --episodes 12 --out out/my-run
   ```
   看到 `✓ 评测：成功率 91.7%（Wilson 95% CI [64.6%, 98.5%]）` 这样的输出即基线
   正常，产物在 `out\my-run\`。引擎内置 ≥90% 硬闸，低于 90% 会直接报错（人话解释
   见文末排查表「90% 闸」一行）——你不需要也不应该改这个闸。
5. 构建 Python 训练桥（§3、§5 要用；任意终端，注意带 `--release`，产物才会在
   `target\release\`）：
   ```
   cargo build --release -p oriera-agent --features python
   ```
   再把产物复制成 Python 能导入的 `oriera_sim.pyd`。cmd / PowerShell：
   ```
   copy target\release\oriera_agent.dll target\release\oriera_sim.pyd
   ```
   （cmd 里第二次复制会问「覆盖 ... 吗? (Yes/No/All)」，输入 `Y` 回车即可。）
   Git Bash：
   ```
   cp target/release/oriera_agent.dll target/release/oriera_sim.pyd
   ```
   macOS / Linux 产物名不同（Linux `liboriera_agent.so`，macOS `liboriera_agent.dylib`），
   都复制成 `target/release/oriera_sim.so`。每次重新 `cargo build` 后都要重新复制一次。

## 2. 看懂评测报告（eval_report.json）

| 字段 | 含义 |
|---|---|
| `success_rate` | 12 集成功比例（脚本基线应 ≈0.92） |
| `wilson95` | 成功率的 Wilson 95% 置信区间——报告里**必须**引用区间而非裸比例 |
| `runs[].seed` | 每集种子：同种子必复现同结局（作业可核验性） |
| `runs[].rand` | 每集域随机化抽签（密度/摩擦/增益/质量——做消融实验用） |
| `randomization` | 本批 DR 档位概要 |

## 3. Python 接入（写你自己的策略）

先做完 §1 第 5 条。两层入口，任选：
- **gymnasium 标准**（推荐，接 SB3/lerobot）。把你的 `.py` 放在仓库根目录并在根目录
  运行；`oriera_gym` 会自己去 `target\release` 找 `oriera_sim.pyd`：
  ```python
  import sys; sys.path.insert(0, "scripts")   # oriera_gym.py 在 scripts/ 下
  from oriera_gym import OrieraPickPlaceEnv
  env = OrieraPickPlaceEnv(task="pick_place", kp_jitter=0.2, mass_jitter=0.15)
  obs, info = env.reset(seed=1000)          # float32[21]
  obs, r, term, trunc, info = env.step(env.action_space.sample())
  ```
- **原生 PyO3**（更细控制）：`import sys; sys.path.insert(0, "target/release")` 后
  `import oriera_sim`，用 `oriera_sim.PickPlaceEnv(...)`，见
  `crates/oriera-agent/src/python.rs` 头注释。

常见导入错误：
| 症状 | 原因 | 处置 |
|---|---|---|
| `ModuleNotFoundError: No module named 'oriera_gym'` | `sys.path` 没加 `scripts`，或不在仓库根目录运行 | 照上面第一行写法，在仓库根目录运行 |
| `ModuleNotFoundError: No module named 'oriera_sim'` | pyd 没复制，或构建时没带 `--release` | 重做 §1 第 5 条，确认 `target\release\oriera_sim.pyd` 存在 |
| `ModuleNotFoundError: No module named 'gymnasium'`（或 numpy 等） | 依赖装到了别的 Python | `python -m pip install ...`，见 §1 第 2 条 |
| `DLL load failed while importing oriera_sim` | 用的是 32 位或低于 3.10 的 Python（训练桥按 64 位 Python 3.10+ 构建） | 换 64 位 Python 3.10+ 后重做 §1 第 5 条 |
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

- **模仿学习**：先用 §1 第 4 条采集，再转成训练框架认的格式（任意终端）：
  ```
  python scripts/jsonl_to_lerobot_v21.py out/my-run/lerobot-v21
  ```
  然后 `lerobot-train --dataset.repo_id=local/my-run --dataset.root=out/my-run/lerobot-v21 --policy.type=act`
  （消费级 GPU 即可，如 RTX 3060）。lerobot 各版本参数有出入，报参数错误时以
  `lerobot-train --help` 为准；报数据集版本不兼容见排查表「数据集版本」一行；
- **强化学习**：SB3 PPO 挂 `OrieraPickPlaceEnv`（经 gymnasium 包装层，先做完 §1
  第 5 条）。一键训练器已备好（固定评测 seed 1000-1011 + 曲线落盘；任意终端）：
  ```
  python scripts/train_ppo.py --task reach --steps 100000 --dr
  ```
  产物在 `out/rl-training/`（eval_curve.json + ppo_model.zip）。
  域随机化三旋钮（kp/mass/obs_noise）做对照实验。无 GPU 的学生做 RL
  路线（MlpPolicy 纯 CPU 即可），有 GPU 的做 IL 路线。

  > **0% 是引擎已知结论，不是你的 bug**：pick_place 用 vanilla PPO 跑出成功率 0%
  > 是预期结果（实测见下）。无 GPU 优先做 reach（RL，纯 CPU），想做 pick_place 走
  > IL 路线。

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

> **报告里贴什么（证据三件套，照着贴就行）**
>
> ① **成功率 + 可信区间**：从 `out\my-run\eval_report.json` 摘 `wilson95` 与
> `runs[].seed` 各一段（下面是官方基线实测值，以你自己跑出的为准）：
> ```
> "success_rate":0.9167,"wilson95":[0.6461,0.9851]
> {"seed":1002,"success":false,"steps":700,"reward_sum":-193.074}
> ```
> 正文这样写：「成功率 91.7%（11/12，Wilson 95% CI [64.6%, 98.5%]）；失败集
> seed=1002，700 步超时」。
>
> ② **可复现回执**：`out\my-run\lerobot-v21\meta\oriera_receipt.json` 贴一行（哈希
> 太长可只留前两个）：
> ```
> {"codebase_version":"oriera-embodied-v2","engine":"oriera-engine/0.10.0","backend":"rigid3d+cam","dt":0.01666667,"successes":11,"episode_state_hashes":[11494885462336563344,13407570765900629234,...]}
> ```
> `episode_state_hashes` 是每集的「身份证号」：助教用同一命令重跑，数字逐位相同才算数。
>
> ③ **一段视频的关键帧**：先跑 §5 的转换命令，视频在
> `out\my-run\lerobot-v21\videos\chunk-000\observation.images.top\episode_000000.mp4`
> （每集一个文件）。用系统自带播放器打开，在「接近方块 / 夹住 / 放进绿圈」三处
> 暂停截图（Windows 按 `Win+Shift+S`），每张图注上 seed 和大致时间点。

## 7. 提交清单与评分细则

**提交**：`out/my-run/` 全目录（含 eval_report.json + receipt + 数据集）
+ 策略代码 + ≤2 页报告（证据三件套见 §6）。

**建议分值**：基线复现 20 / 策略提升 40 / 规范引用（CI/Wilson）20 / 思考题 20。

## 附：问题排查

**看到红字先别慌**：Rust 程序出错时固定打印 `thread 'main' panicked at <文件:行>:`，
**下一行才是原因**，后面的 `note: run with RUST_BACKTRACE=1` 可以忽略。拿原因那句对照
下表；表里找不到，就把整段输出连同你执行的命令一起发给助教。

| 症状 | 处置 |
|---|---|
| **改坏了**（编译报 `error[E…]`、或不记得改过什么）/ 想回到初始状态 | 在仓库根目录运行 `git checkout -- .`：仓库自带文件全部回到 clone 时的样子；你新建的文件（如 `my_policy.py`、`out\`）不受影响，不放心先用 `git status` 看看改了哪些。实在不行删掉文件夹重新 clone（下载 ZIP 的就重新解压一份）——**一切都可恢复**。 |
| **90% 闸**：跑基线报 `scripted baseline must solve ≥90% of seeds (got 10/12)` | 这是**引擎自检在工作**，不是你把电脑弄坏了：`got 10/12` 就是你这次的实际成功集数/总集数，低于 90% 引擎拒绝出数据，免得你拿坏数据交作业。处置：① 确认命令与 §1 第 4 条逐字一致（集数改少会放大偶然失败，`--episodes 5` 错一集就只剩 80%）② `git status` 看有没有改过引擎文件，有就 `git checkout -- .` ③ 确认没改参后重跑。引擎是逐位确定的，同代码同参数结果必然相同，所以要先排除改动再重跑；三步后仍失败，带着整段输出找助教。 |
| 报 `#66+#67: episode fingerprints must match` 或 `... must be bit-identical` | 引擎每次把采集跑两遍逐位比对；不一致说明引擎代码被改出了随机性（随机数、计时、遍历顺序等）→ `git checkout -- .` 恢复后重跑。 |
| `error: linker 'link.exe' not found` | 没装 VS Build Tools → 按 §1「MSVC 还是 MinGW」二选一。 |
| `'cargo'`（或 `'python'`）`不是内部或外部命令` / `无法将“cargo”项识别为 cmdlet` / `command not found` | 新开一个终端再试（装完旧终端看不到新 PATH）；`python` 弹出微软商店或仍找不到，见 §1「Windows 三个常见坑」PATH 一条；`cargo` 仍找不到就重新运行 rustup 安装程序。 |
| `cargo test` 结果里出现 `FAILED` | 先 `git status` 确认没改过文件（改过就 `git checkout -- .` 后重跑）；没改过仍失败就不是你的环境问题，把输出发给助教。 |
| pip 报 `No matching distribution found`（常见于 torch） | 你的 Python 太新，依赖还没出对应版本 → 装 Python 3.12–3.14 中的一个再来（本指南依赖在 3.14 上实测可装可用）。 |
| 复制 pyd 报「另一个程序正在使用此文件」/ `being used by another process` | 旧的 `oriera_sim.pyd` 正被某个 Python 占用 → 先关掉正在运行的 Python（训练脚本、Jupyter 内核）再复制。 |
| `未知参数 --xxx（可用 ...）` | 参数拼错了，括号里列了全部合法参数；照抄 §1 第 4 条。 |
| import 找不到 `oriera_gym` / `oriera_sim` 等 | 见 §3「常见导入错误」表。 |
| `lerobot-train` 报数据集版本不兼容（提到 v2.1 / v3.0 / codebase_version） | **数据集版本**：lerobot 0.4（2025-10）起只读 v3.0 格式，本仓转换脚本当前写 v2.1（引擎直接写 v3.0 正在做）。过渡期二选一：装 0.4 之前的 lerobot（`python -m pip install "lerobot<0.4"`，引号要带上），或用 lerobot 自带的 v2.1→v3.0 转换工具。 |
| 相机视频打不开 / 转换脚本报「缺 imageio[ffmpeg]」 | `python -m pip install imageio imageio-ffmpeg`，然后在资源管理器里删掉 `out\my-run` 文件夹，重跑 §1 第 4 条采集和 §5 转换命令（采集是确定性的，重跑得到同样的数据；转换只能对刚采集的目录做一次）。 |
| 转换脚本报 `no JSONL episodes under ...` | 这个目录已经转换过了（原始 JSONL 已挪进 `raw_jsonl\`），不用再转；想从头来就删掉 `out\my-run` 后重跑 §1 第 4 条和 §5 转换。 |
| 网页显示 `Error code: 404`（File not found） | 起服务器时不在仓库根目录 → 先 `cd` 到有 `examples` 文件夹的那一层再运行 §0 的命令。 |
| 网页一直停在「正在加载」或黑屏 | ① 地址必须是 `http://localhost:8124/15-embodied-sim/`（双击 html 用 file:// 打开不行）② 跑服务器的终端别关 ③ 按 F12 看 Console 里第一条红字，截图找助教。 |
| 起服务器报 `[WinError 10048]` / `[WinError 10013]` / `Address already in use` | 端口被占用或被系统保留 → 换一个：`python -m http.server 8125 -d examples`，网址里的 8124 同步改成 8125。 |
| 浏览器页成功率比 91.7% 低 | 正常：演示内核与引擎基线口径不同（页面右上角小字已声明），作业数字以 CLI 的 `eval_report.json` 为准。 |
| 浏览器页「导出」按钮是灰的 | 正常：独立打开的网页不能导出（鼠标悬停有提示）；采集用 §1 第 4 条的 CLI，或在 Studio 里打开。 |
| 想用 Studio 工作台 | Studio 侧栏「具身仿真台」→ /sim 页；侧栏没有这一项见 §0 第 2 步。 |

## 附：术语人话对照

| 术语 | 人话 |
|---|---|
| episode（集） | 从重置到「放进绿圈」或「超时」的一次完整尝试；`--episodes 12` 就是试 12 次 |
| seed（种子） | 决定方块和目标初始位置的编号；同一个种子必然复现同一个结局 |
| 指纹 / 回执（`episode_state_hashes`） | 这次实验的身份证号：同代码同参数必得同一串数字，改动一点就变 |
| Wilson 95% CI（`wilson95`） | 成功率的可信区间：12 集成功 11 集，真实成功率大概率落在 64.6%–98.5% 之间；集数越少，区间越宽 |
| 90% 硬闸 | 引擎自检：官方脚本基线低于 90% 就拒绝出数据，防止拿坏数据交作业 |
| LeRobot（v2.1 / v3.0） | 训练框架认的数据格式：转换后 `lerobot-train` 能直接读；斜杠后是格式版本号 |
| 域随机化（DR，`--dr`） | 每集随机改一点摩擦/质量/电机增益，逼策略别只会一种情况 |
| IL / RL / PPO | 模仿学习＝照着示范学；强化学习＝自己试错学；PPO＝最常用的一种强化学习算法 |

## 附：学生视角验收

本指南的验收标准（M0a 学生可见验收锚）：从第 0 步照抄到 §7，**全程走查应无未解释的
红字**——每处报错都能在本指南找到人话解释和下一步；遇到表里没有的红字，请把整段
输出连同执行的命令反馈给助教或提 issue，我们补进排查表。
