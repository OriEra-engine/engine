# baselines/ — 学习类 / 脚本类基线注册表（M2，PLAN-embodied-general §9.1 P-11）

研究院主诉「基线无登记处」的答案：**每条基线 = 一份 YAML**（`baselines/<task>/<method>.yaml`），
钉死入口命令、预设、协议、评测种子带、训练种子、超参；**本次实测的参考数**落在
`results/reference/<spec_hash8>/<protocol>.json`（由 `embodied_sim_baseline --reference-out results/reference`
写出，键 = 实验身份 spec_hash8 + 协议 id）。改物理/判据的 PR 应重跑全基线×全预设并附差值表
（PLAN M2 验收 ⑤；自动化脚本 `scripts/oriera_results.py` 已就位，PR 钩子待 CI 接）。

| task | method | 文件 | 入口 | 状态 |
|---|---|---|---|---|
| pick_place | scripted-phases | `pick_place/scripted-phases.yaml` | `embodied_sim_collect`（4 轴解析 IK，金标路） | 实测（91.7% 锚） |
| pick_place | dls-ik | `pick_place/dls-ik.yaml` | `embodied_sim_baseline`（任意 N-DoF；tabletop + panda + panda-contact-criterion 三预设） | 实测 |
| pick_place | dls-ik（contact 判据变体） | `pick_place/dls-ik.yaml`（presets[2]） | `embodied_sim_baseline --spec official/panda-contact-criterion.json` | 实测（P2-Wave2 升格官方，`95f75626`） |
| pick_place | ppo-sb3 | `pick_place/ppo-sb3.yaml` | `scripts/train_ppo.py` | 实测（已知 0%，LAB-GUIDE §5） |
| pick_place | act-lerobot | `pick_place/act-lerobot.yaml` | `lerobot-train --policy.type=act` | **占位**（M2 验收 ③ `lerobot-train` 1 step 冒烟待 CI 钉版本） |
| reach | scripted-phases | `reach/scripted-phases.yaml` | `embodied_sim_collect --task reach` | 实测 |
| reach | ppo-sb3 | `reach/ppo-sb3.yaml` | `scripts/train_ppo.py --task reach` | 实测（100k 逼近脚本回报） |
| push | scripted-phases | `push/scripted-phases.yaml` | `embodied_sim_collect --task push` | 实测（不设闸，如实记录） |
| stack | dls-ik | `stack/dls-ik.yaml` | `embodied_sim_baseline --spec stack-4dof` | 实测 |
| bin_pick | dls-ik | `bin_pick/dls-ik.yaml` | `embodied_sim_baseline --spec bin-pick-4dof` | 实测 |
| drawer | dls-ik | `drawer/dls-ik.yaml` | `embodied_sim_baseline --spec drawer-4dof` | 实测 |

## 本次实测参考数（2026-11-03 首批；2026-11-06 增 panda-contact-criterion 双协议，`embodied_sim_baseline --protocol course@v1|paper@v1 --reference-out results/reference`）

| 预设（spec_hash8） | method | once / at_end | Wilson 95% | 闸 | 吞吐（release） |
|---|---|---|---|---|---|
| tabletop-4dof-pick（c90869dc） | dls-ik | 11/12 · 11/12（91.7%） | [64.6%, 98.5%] | ≥90% **pass** | 781 steps/s |
| panda-7dof-pick（b5535a74） | dls-ik | 12/12 · 12/12（100%） | [75.8%, 100%] | ≥75% **pass** | 1380 / 1340 steps/s（双跑，2026-11-07 P2-Wave-3B 治理后） |
| panda-contact-criterion（95f75626，P2-Wave2 升格） | dls-ik | 12/12 · 12/12（100%） | [75.8%, 100%] | ≥75% **pass** | 1651 / 1619 steps/s（双跑，2026-11-07） |
| stack-4dof（ffc96325） | dls-ik | 11/12 · 11/12（91.7%） | [64.6%, 98.5%] | ≥75% **pass** | 1146 steps/s |
| bin-pick-4dof（14fc9bf8） | dls-ik | 12/12 · 12/12（100%） | [75.8%, 100%] | ≥60% **pass** | 564 steps/s |
| drawer-4dof（ef18ac07） | dls-ik | 10/12 · 10/12（83.3%） | [55.2%, 95.3%] | ≥60% **pass** | 627 steps/s |
| tabletop-4dof-pick（c90869dc） | scripted-phases（collect，金标路） | 11/12（91.7%） | [64.6%, 98.5%] | ≥90% pass | 2575 steps/s（12 集 state hash = pre-m0a 锚逐个相等） |
| panda-7dof-pick（b5535a74） | dls-ik · **paper@v1** | 95/100 · 95/100（95.0%） | [88.8%, 97.8%] | ≥75% 对应口径 **pass** | 2095 / 2082 steps/s（双跑，2026-11-07 P2-Wave-3B 治理后） |
| panda-contact-criterion（95f75626） | dls-ik · **paper@v1** | 95/100 · 95/100（95.0%） | [88.8%, 97.8%] | ≥75% 对应口径 **pass** | 2628 / 2384 steps/s（双跑，2026-11-07） |

Panda paper 口径（P1-B2，2026-11-04）：相位机 ≥6 DoF 专用机制调参后（悬停沉降标定 +
下探末重标定 + 闭爪目标钉住 + 三级抬升 + 限幅 0.028 rad/tick），paper@v1 100 集
60% → **80%**（种子带 1000..，`--verify-determinism sample` 首集逐位一致）；
金标桌面与其余 4 轴预设的 course@v1 参考数逐位复现不变（heavy 路只对 ≥6 DoF 生效，
spec_hash 未变 = 无新预设文件）。失败集中拼凑：方块落在近基区（x < 0.19）的折叠臂位，
夹爪 1.7 cm 指块摩擦挤压余量不足——下一个调参假设是 preset 的抓取带/指块几何（新文件）。

**Contact 判据升官方（P2-Wave2，2026-11-06）**：lab 变体 `panda-contact-criterion`
按治理规则升格官方（同名同 hash `95f75626` 只换登记 tier，lab 原件删除防同名异
hash；升格脉络见 `specs/official/README.md`）。**任务专属闸语义**：判据换装不换闸
——`pick_place@v1` 的 success = 在区（XZ < 0.09 ∧ 高度带内），不读 `holding`，
闸值沿用 Panda pick 的 ≥75%（`baseline_gate` 按 task×robot 查表，与 holding 判据
无关）；变的只是 `holding` 遥测标签（TipRadius 启发式 → Contact 接触力判据：双板
法向力 ≥1.0 N ∧ 板↔物滑速 ≤0.05 m/s），进报告的只有成功率与奖励里读 holding 的
两项（grasp_event +2.0 / 持握段塑形项）。**升格证据 = 同 seeds 两口径逐集对照零
翻转**：course@v1 12 集、paper@v1 100 集 success / once / at_end / steps 逐集全同
（10/12、80/100，transition 总数 6574 / 57913 也一致），仅 reward_sum 每集有差
（holding 标签分歧的痕迹；失败 700 步集差 +9…+22——TipRadius 在未接触窗口发假
「夹住」标签、Contact 按物理修正）。同构建 TipRadius 复跑（`panda-7dof-pick` 本体）
course/paper 与 `results/reference/b5535a74/` 存档逐集位同——升格未动任何现有预设
的物理与参考数；新预设两遍双跑 runs 逐集位同（吞吐为计时元数据自然不同），
reference 落盘 `results/reference/95f75626/{course_v1,paper_v1}.json`。

**失败相位复核（P2 深水区 Wave-1，`failure_phase@v1` 遥测，2026-11-06）**：同命令加
`--failure-phases` 复跑 paper@v1——100 集与 `results/reference/b5535a74/paper_v1.json`
逐集 (seed, success, steps) 100/100 相同（80/100），**20 个失败集全部归 `lift` 相位**：
每集握持 126–127 tick、`max_lift_held = 0`（夹持下方块从未离桌）、`min_ee_obj ≤ 5 mm`
（够得着、夹得到）——是抬升起步即滑脱，不是 IK 够不着、也不是没夹上。空间分布：20/20 都在
近基带 x < 0.19（近基 20/69 = 29% vs 远端 0/31），且集中在 **x < 0.16 ∧ z ≥ 0 一侧
（15/18 = 83%；同 x 带 z < 0 为 0/12）**。结论：原假设「近基折叠臂位 + 指块摩擦挤压余量
不足」在相位上**成立**、在空间上**收窄为 +z 侧近基口袋**。单集复盘
（`ORIERA_SIM_TRACE=20 ORIERA_SIM_TRACE_FK=1 … --seed0 132105 --episodes 1`）：闭爪期
TCP 在方块中心上方 5 mm、开合轴与世界 Z 偏差 ≤ 0.3°、接近轴向 −X 倾 4.6°（近基折叠位的
工具倾角，z < 0 的成功集同样 4.6°），t≈280 臂起而方块不动。下一个调参假设收窄为 +z 侧
近基口袋的抓取带/指块几何（或该侧 wrist 解支），仍按「改字段 = 新文件」走。
course@v1 12 集同遥测（`--failure-phases --runs-root`）：tabletop 1 lift · panda 2 lift ·
stack 1 place · drawer-4dof 2 place · bin-pick / drawer v2 / mobile-arm 0（判定规则见
[LAB-GUIDE §2.2](../docs/LAB-GUIDE.md)）。（上段「下一个调参假设」已由下节 P2-Wave-3B
解决——真因不是几何余量而是接触流形冻结，无需新预设文件。）

**Panda lift 相位治理（P2-Wave-3B，2026-11-07）**：沿 Wave-1 遥测继续逐 tick 复盘
+z 侧近基失败集，真因落到 rapier 接触层而非几何：**幽灵夹持（ghost grip）**——板↔方块
的碰撞对在接近姿态下创建时，流形冻结在创建时刻的推测接触（speculative contacts）上，
这些点算「活跃接触」使碰撞对的回收预算放大到整段 5 cm，而 21 mm 的闭爪扫掠永远跨不过
它，求解器于是输出恰好 0 N——抬升时板子从方块两侧「穿」上去而方块纹丝不动（Wave-1 测到的
`max_lift_held = 0` 正是此现象）。扫掠、抬升、绕轴旋转都无法让一个活的碰撞对刷新流形；
**唯一有效的是杀掉它再重生**：远离方块重生的碰撞对尚无流形点，回收预算被 ~2 cm 预测距离封顶，
21 mm 的再闭爪即可跨过预算、接触在形成那一刻被精确重算。落地机制（`embodied_sim_baseline.rs`
heavy 路，`SWAP_*` 常量）：首段抬升开始后第 `SCRUB_DETECT_AFTER=8` tick 检测方块抬升量
< `SCRUB_DETECT_RISE=4 mm` 即判幽灵夹持 → 开爪上撤 `SWAP_RISE_HEIGHT=8 cm`（30 tick，碰撞对
在 broad-phase 消亡）→ 驻留 6 tick → 开爪降回同一组 ±z 面（25 tick）→ 全力重闭 →
`SWAP_SETTLE_TICKS=40` 保持挤压再进入竖直抬升（直接放进 carry 段的水平猛拉会把刚夹住的
方块扯脱——这一项是从 83% 到 95% 的关键一步）。撤离全程开爪，撞不到东西；误触发（慢但真的
夹持）只是把方块放回原位一厘米再重夹一次。

数字（paper@v1 100 集，seeds 1000..，与存档同协议同 seed 带）：**80/100 → 95/100**
（Wilson [88.8%, 97.8%]）；course@v1 **10/12 → 12/12**。失败相位前后对照：lift 20 → **5**
（reach/grasp/place 全程 0），残余 5 集 = seeds 132091 / 197645 / 197647 / 197660 / 525382
（TipRadius 口径归 lift；Contact 口径同 5 集归 **grasp**——Contact 不把 0 N 的幽灵接触算
「夹住」，归因落到正确相位，再次印证 U-6 判据设计）。`panda-contact-criterion` 同机制同
升幅（12/12、95/100，transitions 55378 与母预设全等）。**门控证据**：机制只在 heavy
（≥6 DoF，`sag_comp`）路径启用——tabletop 4 轴金标与 HEAD 二进制 12 集逐字段全等、
`reanchor_golden` 三口径 no drift；mobile-arm（7 DoF，但从不触发）同样 12 集逐字段全等；
两个 panda 预设 reference 双跑逐集位同后重写
`results/reference/{b5535a74,95f75626}/{course_v1,paper_v1}.json`。**试过无效、已撤的**：
重闭 12-tick 挤压斜坡（残余 5 集零恢复）、`SCRUB_DETECT_AFTER` 8→16（退步）、水平偏航撤离
（无增益）。残余 5 集未再追（seed 66549 的 19 mm 误触发阈值也未重标定）——按「负结果也是
结果」收手入档。

once 与 at_end 相同是因为六预设都走 `terminate_on_success = true`（首次满足即终止；tabletop / panda / stack / bin-pick / drawer v2 / mobile-arm 同款）；`--set task.params.terminate_on_success=false` 跑满 700 步才会分开。

约定：
- `eval.protocol` 一律写 id（`course@v1` / `paper@v1`），集数/种子带由协议决定（`oriera_agent::eval`）；
- `eval.seed0` 是评测带起点（CLI 验收带 1000；RL 的 held-out 带 10_000_000，见 `train_ppo.py`）；
- `train.seeds` 是训练种子列表（`paper@v1` 要 ≥5 个）；脚本类基线没有训练，写 `null`；
- `gate` 引用 `oriera_agent::eval::baseline_gate` 的数字（金标桌面 0.90 / Panda·stack 0.75 / bin·drawer 0.60 / push 无）；
- `reference` 指向 `results/reference/<spec_hash8>/<protocol>.json`——没有这份文件 = 还没实测，别写数字。
