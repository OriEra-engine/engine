# baselines/ — 学习类 / 脚本类基线注册表（M2，PLAN-embodied-general §9.1 P-11）

研究院主诉「基线无登记处」的答案：**每条基线 = 一份 YAML**（`baselines/<task>/<method>.yaml`），
钉死入口命令、预设、协议、评测种子带、训练种子、超参；**本次实测的参考数**落在
`results/reference/<spec_hash8>/<protocol>.json`（由 `embodied_sim_baseline --reference-out results/reference`
写出，键 = 实验身份 spec_hash8 + 协议 id）。改物理/判据的 PR 应重跑全基线×全预设并附差值表
（PLAN M2 验收 ⑤；自动化脚本 `scripts/oriera_results.py` 已就位，PR 钩子待 CI 接）。

| task | method | 文件 | 入口 | 状态 |
|---|---|---|---|---|
| pick_place | scripted-phases | `pick_place/scripted-phases.yaml` | `embodied_sim_collect`（4 轴解析 IK，金标路） | 实测（91.7% 锚） |
| pick_place | dls-ik | `pick_place/dls-ik.yaml` | `embodied_sim_baseline`（任意 N-DoF；tabletop + panda 两预设） | 实测 |
| pick_place | ppo-sb3 | `pick_place/ppo-sb3.yaml` | `scripts/train_ppo.py` | 实测（已知 0%，LAB-GUIDE §5） |
| pick_place | act-lerobot | `pick_place/act-lerobot.yaml` | `lerobot-train --policy.type=act` | **占位**（M2 验收 ③ `lerobot-train` 1 step 冒烟待 CI 钉版本） |
| reach | scripted-phases | `reach/scripted-phases.yaml` | `embodied_sim_collect --task reach` | 实测 |
| reach | ppo-sb3 | `reach/ppo-sb3.yaml` | `scripts/train_ppo.py --task reach` | 实测（100k 逼近脚本回报） |
| push | scripted-phases | `push/scripted-phases.yaml` | `embodied_sim_collect --task push` | 实测（不设闸，如实记录） |
| stack | dls-ik | `stack/dls-ik.yaml` | `embodied_sim_baseline --spec stack-4dof` | 实测 |
| bin_pick | dls-ik | `bin_pick/dls-ik.yaml` | `embodied_sim_baseline --spec bin-pick-4dof` | 实测 |
| drawer | dls-ik | `drawer/dls-ik.yaml` | `embodied_sim_baseline --spec drawer-4dof` | 实测 |

## 本次实测参考数（2026-11-03，`embodied_sim_baseline --protocol course@v1 --verify-determinism full --reference-out results/reference`）

| 预设（spec_hash8） | method | once / at_end | Wilson 95% | 闸 | 吞吐（release） |
|---|---|---|---|---|---|
| tabletop-4dof-pick（c90869dc） | dls-ik | 11/12 · 11/12（91.7%） | [64.6%, 98.5%] | ≥90% **pass** | 781 steps/s |
| panda-7dof-pick（b5535a74） | dls-ik | 10/12 · 10/12（83.3%） | [55.2%, 95.3%] | ≥75% **pass** | 1106 steps/s |
| stack-4dof（ffc96325） | dls-ik | 11/12 · 11/12（91.7%） | [64.6%, 98.5%] | ≥75% **pass** | 1146 steps/s |
| bin-pick-4dof（14fc9bf8） | dls-ik | 12/12 · 12/12（100%） | [75.8%, 100%] | ≥60% **pass** | 564 steps/s |
| drawer-4dof（ef18ac07） | dls-ik | 10/12 · 10/12（83.3%） | [55.2%, 95.3%] | ≥60% **pass** | 627 steps/s |
| tabletop-4dof-pick（c90869dc） | scripted-phases（collect，金标路） | 11/12（91.7%） | [64.6%, 98.5%] | ≥90% pass | 2575 steps/s（12 集 state hash = pre-m0a 锚逐个相等） |
| panda-7dof-pick（b5535a74） | dls-ik · **paper@v1** | 80/100 · 80/100（80.0%） | [71.1%, 86.7%] | ≥75% 对应口径 **pass** | 1932 steps/s |

Panda paper 口径（P1-B2，2026-11-04）：相位机 ≥6 DoF 专用机制调参后（悬停沉降标定 +
下探末重标定 + 闭爪目标钉住 + 三级抬升 + 限幅 0.028 rad/tick），paper@v1 100 集
60% → **80%**（种子带 1000..，`--verify-determinism sample` 首集逐位一致）；
金标桌面与其余 4 轴预设的 course@v1 参考数逐位复现不变（heavy 路只对 ≥6 DoF 生效，
spec_hash 未变 = 无新预设文件）。失败集中拼凑：方块落在近基区（x < 0.19）的折叠臂位，
夹爪 1.7 cm 指块摩擦挤压余量不足——下一个调参假设是 preset 的抓取带/指块几何（新文件）。

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
[LAB-GUIDE §2.2](../docs/LAB-GUIDE.md)）。

once 与 at_end 相同是因为六预设都走 `terminate_on_success = true`（首次满足即终止；tabletop / panda / stack / bin-pick / drawer v2 / mobile-arm 同款）；`--set task.params.terminate_on_success=false` 跑满 700 步才会分开。

约定：
- `eval.protocol` 一律写 id（`course@v1` / `paper@v1`），集数/种子带由协议决定（`oriera_agent::eval`）；
- `eval.seed0` 是评测带起点（CLI 验收带 1000；RL 的 held-out 带 10_000_000，见 `train_ppo.py`）；
- `train.seeds` 是训练种子列表（`paper@v1` 要 ≥5 个）；脚本类基线没有训练，写 `null`；
- `gate` 引用 `oriera_agent::eval::baseline_gate` 的数字（金标桌面 0.90 / Panda·stack 0.75 / bin·drawer 0.60 / push 无）；
- `reference` 指向 `results/reference/<spec_hash8>/<protocol>.json`——没有这份文件 = 还没实测，别写数字。
