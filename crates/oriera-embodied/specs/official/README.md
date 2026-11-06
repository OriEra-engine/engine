# specs/official — 受治理的官方 spec 库（M0b，PLAN-embodied-general §9.1 P-9）

**规则一句话：这里的每个文件都是一份实验身份（`spec_hash`），改动任何非 `meta` 字段 = 新实验 ⇒ 必须另起新文件名（或新版本后缀），并在 PR 打 `physics-behavior-change` 标签、重跑 `contracts/golden/embodied_fingerprints.v2.json`；禁止原地改值。**

**P2 修订（PLAN-embodied-v3 §三，2026-11-05）**：同一预设的治理性修复——prop 近似升真 fixture、`robot.name` 命名对齐等**不改变物理行为**（或只强化保真度）的升级，走**同文件换 hash**：文件名不动、spec_hash 换新，INDEX 重登记 + `results/reference/<新hash8>/` 重测落盘 + 本表同步；旧 hash 的 reference 目录保留作历史档案（先例：`stack-4dof` 换名 `ffc96325` → `249d5122`，episode 动力学逐位一致）。

- 文件名 = `extends` 里引用的名字（`"extends": "tabletop-4dof-pick"` → `official/tabletop-4dof-pick.json`）。
- 实验室/个人实验放 `specs/lab/`（建议 `lab/<user>/`），用 `extends` 继承官方 spec、只写差异（见 `lab/example-cylinder.json`）；lab 文件不受治理，但同名异 hash 会被 `scripts/gen_spec_index.py` 拦下。**升格先例**：lab 变体验证充分后按治理规则平移进 `official/`——同名同 hash、只换登记 tier，lab 原件删除（防同名异 hash），升格脉络写进该文件 `meta.notes` 与下表；先例 `panda-contact-criterion`（P2-Wave2，lab `95f75626` → official 同 hash，升格判据 = 两口径同 seeds 逐集零翻转 + reference 重测，见 `baselines/README.md`）。
- `INDEX.json`（`python scripts/gen_spec_index.py` 生成）：`meta.name → spec_hash` 的登记表；同名异 hash 报错、同 hash 异名放行（同一实验的别名）。
- `tabletop-4dof-pick.json` 是 M0a 的 v2 金标，与 `specs/tabletop-4dof-pick.json`（WP-B 契约测试钉住的旧路径）逐字节一致——两份都不许动；spec_hash 锚 `c90869dcf6a04875`。
- 建议配合 `CODEOWNERS`：`crates/oriera-embodied/specs/official/ @oriera-embodied-maintainers`（仓库级文件不在 M0b 授权面内，此处只留建议）。

## 官方预设（八文件 = 七活跃 + 1 对照档案；七活跃 = M2 首批 5 个 + M3 mobile-arm + P2 contact-criterion 升格；drawer@v1→v2 滑轨替换，v1 为对照档案）

| 预设 | 任务 | 机器人 | 一句话（G-10 卡片） | 难度 | 基线闸 | DLS 基线实测（course@v1，12 集，2026-11-03） |
|---|---|---|---|---|---|---|
| `tabletop-4dof-pick` | `pick_place@v1` | tabletop-4dof（4 轴） | 抓起方块放到目标区——引擎默认作业基线（91.7% 锚） | 入门 | ≥90%（course@v1 硬闸同款） | **11/12 = 91.7%** pass · Wilson [64.6, 98.5] · `c90869dc` |
| `panda-7dof-pick` | `pick_place@v1` | panda7 + gripper2f（7 轴 + 外部两指） | 真 URDF 机械臂（Franka Panda）做同一个抓放——`--dump-panda-spec` 一次性生成 | 进阶 | ≥75%（新臂首版诚实门槛） | **10/12 = 83.3%** pass · Wilson [55.2, 95.3] · `b5535a74`（M2 首版 9/12 压线；P1-B2 相位机调参后重测 10/12，paper@v1 100 集 80%） |
| `panda-contact-criterion`（P2-Wave2 升格） | `pick_place@v1` | panda7 + gripper2f（同上） | 同 `panda-7dof-pick`，仅 `holding` 标签换接触力判据（双板法向力 ≥1.0 N 且滑速 ≤0.05 m/s 才算夹住）——`holding` 从「几何说合拢」变「物理说夹住」（U-6；物理逐位不变，判据是 v2 身份字段，hash 与母预设不同） | 进阶 | ≥75%（pick 闸；判据换装不换闸——success=在区、不读 holding，两口径同 seeds 逐集零翻转见 `baselines/README.md`） | **10/12 = 83.3%** pass · Wilson [55.2, 95.3] · `95f75626`（course+paper 双协议双跑 runs 逐位一致；paper@v1 **80/100 = 80.0%** pass · Wilson [71.1, 86.7]，2026-11-06） |
| `stack-4dof` | `stack@v1` | stack-4dof（tabletop 同构 4 轴臂，P2 命名治理） | 把方块 A 叠到底座 B 上（PLAN 原名 push-stack 收敛为 stack：push 走 `push@v1/v2` 单任务） | 进阶 | ≥75%（新任务首版） | **11/12 = 91.7%** pass · Wilson [64.6, 98.5] · `249d5122`（旧身份 `ffc96325` 同为 11/12，复测逐位一致） |
| `bin-pick-4dof` | `bin_pick@v1` | tabletop-4dof | 从真料箱（双层壁+底面 fixture，P2 升级）里把方块抓出来放到箱外目标区 | 挑战 | ≥60% | **1a7cfc40** |
| `drawer-4dof` | `drawer@v1` | tabletop-4dof | 【**对照档案（deprecated → drawer-4dof-v2）**】抓住抽屉把手块沿 -X 拉出 ≥0.08 m（可动盒体 prop 近似 prismatic；v2 真滑轨已落地） | 进阶 | ≥60%（首版） | **10/12 = 83.3%** pass · Wilson [55.2, 95.3] · `ef18ac07` |
| `drawer-4dof-v2`（P2-W6B） | `drawer@v2` | tabletop-4dof | 沿 -Z 拉开真 prismatic 抽屉 ≥0.08 m：面板 = slide prop（prismatic 滑轨，撞不走、限位钳制），柜体 = 真 fixtures | 进阶 | ≥60%（首版） | **12/12 = 100%** pass · Wilson [75.8, 100.0] · `f503b3a3`（course@v1 双跑逐位一致，2026-11-05） |
| `mobile-arm-4dof` | `pick_place@v1` | mobile-4dof（两滑轨+回转底盘+4 轴臂） | 作业点超臂展 0.475 m——底盘开到 ∓0.40 落回作业带（M3 新增） | 进阶 | ≥75%（pick 闸） | 首跑 settle/跟踪 0.0000 m · `9ee15317` |

实测数落盘在 `results/reference/<spec_hash8>/course_v1.json`（`--reference-out` 写）；七活跃预设双跑逐位一致（stack 于 P2 换名后以新身份 `249d5122` 复测，episode 数据与旧身份逐位相同）；drawer v2（`f503b3a3`，P2-W6B）同口径双跑逐位一致；panda-contact-criterion（`95f75626`，P2-Wave2 升格）course+paper 双协议双跑 runs 逐集位同（吞吐为计时元数据、两次自然不同）。`drawer-4dof`（ef18ac07）自 P2-W6B 起为**对照档案**（可动盒体近似 vs 真 prismatic 滑轨的对照），新实验请用 `drawer-4dof-v2`。

每份带 `meta{name, author, notes, difficulty}`；四个入口（CLI/Python/浏览器/Studio）吃同一份文件、同一个 `spec_hash`。

**Studio 三库即食**：Studio `/sim` 首屏的预设卡片墙挂官方活跃预设（七张卡与官方七活跃一一对应：drawer 卡指 v2 滑轨版，v1 为对照档案不下墙；默认选中作业基线 `tabletop-4dof-pick`），点卡片即整份换装、不重载；Studio 仓 `node scripts/sync-engine-snapshot.mjs` 把本目录 + `INDEX.json` 同步进 `engine-snapshot/specs/`（另生成 `extends` 展开版给浏览器内核，spec_hash 与本表一致）。新增官方预设 = 重跑同步器 + 在 Studio `src/views/Sim.vue` 的预设目录补一张卡（卡片一句话是人写的学生文案）。

基线跑法（成功率 + Wilson + once/at_end 双报 + 任务专属闸）：

```bash
cargo run -p oriera-agent --example embodied_sim_baseline --features rigid3d,spec,urdf -- \
  --spec crates/oriera-embodied/specs/official/<预设>.json --protocol course@v1 \
  --reference-out results/reference
```

## 字段中文速查表（G-7：字段 → 含义 → 学生能不能改 → 合法范围）

读法：`spec.` 后的路径就是 JSON 里的键路径。**"继承"** = 用 `extends` 时不写该字段就沿用基 spec 的值。**改了 = 新实验**的字段都会进 `spec_hash`；改 `meta.*`（名字/备注/难度）不进。

### 顶层

| 字段 | 含义 | 学生能不能改 | 合法范围 / 默认 |
|---|---|---|---|
| `schema` | 规格版本门 | ❌ 固定 | 必须 `"oriera.scene/2"`（旧草案用 `--migrate`） |
| `meta.name` | 预设名（INDEX 登记键） | ✅ 不进 hash | 非空字符串 |
| `meta.author` / `meta.notes` | 署名 / 说明 | ✅ 不进 hash | 任意文本 |
| `meta.difficulty` | 预设卡片难度标签（M2，G-10） | ✅ 不进 hash | `入门` / `进阶` / `挑战` |
| `dt` | 物理步长（秒） | ⚠️ 改了 = 新实验 | 有限正数（默认 1/60 ≈ 0.016666668） |
| `max_steps` | 每集步数上限（超时 = truncated） | ⚠️ 改了 = 新实验 | ≥1（金标 700） |
| `extends` | 继承官方基 spec（只写差异） | ✅ 覆盖层专用 | 官方库名（可省 `official/`、`.json`）；循环/过深报错 |

### `robot`（RobotSpec）

| 字段 | 含义 | 学生能不能改 | 合法范围 / 默认 |
|---|---|---|---|
| `robot.name` | 机器人名（LeRobot robot_type） | ⚠️ 进 hash | 字符串（金标 `tabletop-4dof`） |
| `robot.base` | 基座世界坐标（m，信息性） | ⚠️ 进 hash | `[x, y, z]` |
| `robot.links[]` | 逐链体：`name` / `shape`（Box/Capsule）/ `origin`（home 世界位）/ `collider_offset` / `density` / `friction` / `added_mass` / `mesh_uri` | ⚠️ 进 hash | 尺寸有限正数；`mesh_uri` 现阶段必须 null（网格碰撞 M2 落地前） |
| `robot.joints[]` | 逐关节：`name` / `parent_link` / `child_link` / `anchor`（父体系）/ `axis` / `limits`（rad）/ `effort` / `velocity_limit` / `kp` / `kd` / `armature` / `damping` | ⚠️ 进 hash | 拓扑序、轴非零、`limits.lo ≤ hi`、增益有限非负；panda 预设 kp/kd = 300/30 |
| `robot.gripper` | 夹爪：`attach_link` / `finger_x`（板中心沿接近轴）/ `tcp_offset` / `finger_half` / `gap_open` / `gap_closed` / `friction` / `effort` / `gap_rate` / `kind` / `approach_axis` / `gap_axis` | ⚠️ 进 hash | `gap_closed ≤ gap_open`；`approach_axis` 缺省 +X、`gap_axis` 缺省 +Z（板系朝向，M0b）；两轴不得平行 |
| `robot.home[]` | 初始关节角（rad，逐关节） | ⚠️ 进 hash | 长度 = 关节数；限位不含 0 时钳到最近限位（panda joint4 → −0.0698） |
| `robot.mass_scale` | 全链质量缩放（DR 旋钮的标称值） | ⚠️ 进 hash | 有限正数（默认 1.0） |
| `robot.action_scale` | 每关节动作缩放（rad/单位动作，包装层用） | ⚠️ 进 hash | 长度 = 关节数、有限正数（预设 [2.0; N]） |

### `scene`（SceneGraph，G-3：物体在 spec 里）

| 字段 | 含义 | 学生能不能改 | 合法范围 / 默认 |
|---|---|---|---|
| `scene.fixtures[]` | 静态家具盒体（`name` / `center` / `half` / `friction`）。fixture[0]→地面、[1]→桌面（语义位，勿挪）；更多（料箱壁/货架/滑道外壳）P0 起已解锁任意个数 | ⚠️ 进 hash | ≥1 个；half 全正；每块各自 `friction`（有限 ≥0，P0 起不钉 0.9；reset 走 `from_robot_fixtures` 全列表） |
| `scene.props[]` | 动态物体。**props[0] = 任务物体**（obs/判据跟踪它）；其余是场景件（stack 的底座 B、bin 的四面墙等） | ⚠️ 进 hash | ≥1 个 |
| `props[].name` | 物体名 | ⚠️ 进 hash | 字符串 |
| `props[].shape` | 碰撞体：`{"kind":"cuboid","half":[x,y,z]}` / `{"kind":"cylinder","half_h":h,"radius":r}` / `{"kind":"sphere","r":r}`（m，半尺寸） | ⚠️ 进 hash | 全部有限正数；**改这一个字段就能换物体**（学生验收项） |
| `props[].phys` | 标称物理：`density`（kg/m³）/ `friction` | ⚠️ 进 hash | 正数（默认 400 / 0.8） |
| `props[].placement` | 出生位：`{"range":{"x":[lo,hi],"y":h,"z":[lo,hi]}}`（XZ 均匀抽、y 固定）或 `{"fixed":[x,y,z]}`；抽签顺序 = 回执的一部分 | ⚠️ 进 hash | range 须 lo ≤ hi；料箱墙/底座用 fixed |
| `props[].slide` | **滑轨（P2-W6B，可省略）**：`{"axis":[x,y,z],"limits":[lo,hi],"damping":δ}`——该 prop 的动态体经 prismatic 关节接到出生位的静态锚体：只能沿轴滑、行程钳制（米，零点 = 出生位）、`damping`（N·s/m）= 滑轨摩擦模型；被夹爪摩擦拖动、法向推挤被关节吸收 | ⚠️ 进 hash（**省略 = 不写**，所有旧 spec 的 hash 不动） | axis 有限非零；lo ≤ hi 有限；damping ≥ 0；`drawer@v2` 强制 props[0] 必填 |

### `task`（TaskRef）

| 字段 | 含义 | 学生能不能改 | 合法范围 / 默认 |
|---|---|---|---|
| `task.id` | 注册表任务 id | ⚠️ 进 hash | `reach@v1` `pick_place@v1` `push@v1`（金标判据，只读保留）· `pick_place@v2` `push@v2` `reach@v2`（@v2 修订判据）· `stack@v1` `bin_pick@v1` `drawer@v1`（M2 新任务）· `drawer@v2`（P2-W6B 真 prismatic 面板，须配 props[0].slide）。裸名（`pick_place`）= 该族 @v1 |
| `task.params.goal_xz_tol` | 目标区 XZ 容差（m） | ⚠️ 进 hash | 有限正数（默认 0.09） |
| `task.params.goal_y_min` / `goal_y_max` | 目标区高度带（物体质心，m） | ⚠️ 进 hash | 有限、min < max（默认 0.5 / 0.62） |
| `task.params.reach_radius` | reach 终止半径（尖端−物体，m） | ⚠️ 进 hash | 有限正数（默认 0.055） |
| `task.params.min_separation` | 出生时物体与目标区最小 XZ 间距（m，不足重抽） | ⚠️ 进 hash | 有限正数（默认 0.12） |
| `task.params.max_redraws` | 间距重抽预算 | ⚠️ 进 hash | ≥0（默认 8） |
| `task.params.goal` | 目标区出生位（Placement，同上） | ⚠️ 进 hash | range / fixed；**这决定"放到哪"** |
| `task.params.terminate_on_success` | 首次满足判据即终止（@v2 协议参数位；false = 跑满，靠 once/at_end 双报） | ⚠️ 进 hash（**取默认值时从 hash 里规范化掉**，金标 hash 不动） | true（默认）/ false |
| `task.params.rest_steps` | @v2 静止判据步数 k | ⚠️ 同上 | ≥1（默认 10） |
| `task.params.rest_speed` | @v2 静止速度阈值（m/s） | ⚠️ 同上 | 有限正数（默认 0.01） |
| `task.params.release_below` | @v2 松爪判据：夹爪指令低于它 = 已松爪 | ⚠️ 同上 | (0, 1]（默认 0.5） |
| `task.params.stack_xz_tol` | stack：A 相对 B 的 XZ 对齐容差（m） | ⚠️ 同上 | 有限正数（默认 0.03） |
| `task.params.stack_y_tol` | stack：中心高差容差（m，叠好 = 两半高之和） | ⚠️ 同上 | 有限正数（默认 0.02） |
| `task.params.pull_axis` | drawer：拉出方向（世界系水平向量） | ⚠️ 同上 | 有限、水平分量非零（默认 [-1,0,0] = 朝臂基座） |
| `task.params.pull_distance` | drawer：拉出距离目标（m） | ⚠️ 同上 | 有限正数（默认 0.08） |

> M2 参数在**默认值时不进 spec_hash**（夹爪 approach_axis/gap_axis 同款先例）：v1 金标 spec 的身份一字不动；写了非默认值才算改实验。

### `control` / `randomization` / `physics`

| 字段 | 含义 | 学生能不能改 | 合法范围 / 默认 |
|---|---|---|---|
| `control.mode` | 控制模式 | ❌ 固定 | `"joint_pos"`（M0a 只此一种） |
| `control.action_scale` | 覆盖 `robot.action_scale` | ⚠️ 进 hash | null（用 robot 自带）或长度=关节数的正数数组 |
| `randomization.enabled` | reset 域随机化总开关 | ⚠️ 进 hash | true / false |
| `randomization.kp_jitter` / `mass_jitter` | 电机增益 / 连杆质量抖动半幅 | ⚠️ 进 hash | ≥0（默认 0 = 关） |
| `randomization.obs_noise` | 观测加性噪声半幅（holding 旗豁免） | ⚠️ 进 hash | ≥0（默认 0 = 关） |
| `randomization.prop_density` / `prop_friction` | props[0] 密度 / 摩擦绝对抽签带 | ⚠️ 进 hash | `[lo, hi]` 且 lo ≤ hi（默认 [240,640] / [0.5,1.1]） |
| `physics.*` | 求解器旋钮：`solver_iterations` / `substeps` / `contact_softness_hz` / `warmstart_coefficient`（null = rapier 默认） | ⚠️ 进 hash | null 或正数；指纹的一部分 |

### 判据速记（@v1 vs @v2，P-2）

| 任务 | success（v1，只读） | success（@v2 新 id） |
|---|---|---|
| pick_place | 在区（XZ<0.09 且高度带内） | 在区 **且已松爪（指令<0.5）且静止 10 步（<0.01 m/s）** |
| push | 在区（抓取还给奖励） | 在区 **且全程 `holding==false`**（任一帧 holding 判败；奖励无抓取项） |
| reach | 尖端距物体 <0.055 | 同 v1 + `terminate_on_success` 协议参数位 |
| stack / bin_pick / drawer | —（新任务） | 直接按 @v2 精神：对齐/拉出/出箱 + 松爪 + 静止 |

每集双报：`success_once`（首次满足）与 `success_at_end`（末帧满足）；v1 任务 `success` = once 保持旧语义。
