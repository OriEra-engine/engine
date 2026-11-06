//! DLS 数值 IK 基线采集器（M2，PLAN-embodied-general §四 M2 / §9.1
//! P-11）：对 `specs/official/` 任一预设跑 `course@v1` 口径的 12 集，
//! 输出成功率 + Wilson 95% CI + **success_once / success_at_end 双报**
//! （@v2 判据），并按任务专属闸（`eval::baseline_gate`：金标桌面预设
//! 仍 90%，新任务首版 75%/60% 诚实门槛）给出 PASS/FAIL。
//!
//! 与 `embodied_sim_collect.rs` 的分工（该文件冻结，WP-C 后续要动）：
//! - 基线策略 = **DLS 数值 IK**（`oriera_agent::ik::DlsIk`，数值雅可比，
//!   任意 N-DoF——Panda 7 轴靠它）；collect 的 4 轴解析 IK 金标路原样保留。
//! - 相位机沿用 collect 的「waypoint × dwell 收敛门控」思路，在本文件
//!   重写（未共享代码——collect 一行没动）。
//! - 不产 LeRobot 数据集、不开相机：这是**评测**入口，不是采集入口。
//!
//! 用法：
//! ```text
//! cargo run -p oriera-agent --example embodied_sim_baseline \
//!   --features rigid3d,spec,urdf -- --spec crates/oriera-embodied/specs/official/panda-7dof-pick.json \
//!   --protocol course@v1 [--reference-out results/reference] [--runs-root runs]
//! ```
//! Args: `--spec FILE`（默认金标 tabletop-4dof-pick）· `--set a.b=v`
//!       `--episodes N` · `--seed0/--seed N` · `--protocol ID` · `--gate hard|report`
//!       `--out DIR` · `--runs-root DIR` · `--reference-out DIR` · `--verify-determinism full|sample|off`
//!       `--dump-panda-spec FILE`（一次性生成逻辑：panda7.urdf + gripper2f.urdf
//!       → `panda-7dof-pick` 预设落盘，kp/kd 用 WP-D 验证过的 300/30）
//!       `--force`
//!       `--failure-phases`（P2 opt-in 遥测：失败集按 reach/grasp/lift/place 归因，
//!       eval_report 增 `failure_phases` 块 + 每集 `failure_phase`/`phase_signals`；
//!       不给则报告逐字节不变。判定规则 docs/LAB-GUIDE.md §2.2）
//! Output: `<out>/eval_report.json` + `<out>/run_manifest.json`
//!         （`--reference-out` 另写 `<dir>/<spec_hash8>/<protocol>.json`）
//!
//! 逐 tick 轨迹：`ORIERA_SIM_TRACE=<N>`（每 N tick 一行 grip/hold/obj/B/ee/ik 残差，
//! 非数字 = 20）、`ORIERA_SIM_TRACE_FK=1`（同行附 FK 接近/开合轴）、
//! `ORIERA_SIM_DEBUG=1`（每集末一行终态 + FK）——都打到 stderr，配合
//! `--failure-phases` 给出的失败 seed 用 `--seed0 <seed> --episodes 1` 单集复盘。

use oriera_agent::embodied_env::{task_entry, EmbodiedEnv, ObsLayout, TableTopEnv};
use oriera_agent::eval::{
    self, baseline_gate, classify_failure, summarize_failure_phases, EpisodeOutcome, EpisodeRow,
    EvalProtocol, FailurePhaseRules, Gate, PhaseSignals, SuccessView,
};
use oriera_agent::ik::DlsIk;
use oriera_agent::manifest::{write_run_manifest_opts, ManifestExtra};
use oriera_agent::spec::SceneSpec;
use oriera_subsys::rigid3d::robot_identity_hash;

// ---------------------------------------------------------------------------
// Phase machine — the collect example's convergence-gating idea
// (waypoint × dwell ticks), rewritten here so the frozen collect file is
// untouched. A phase's target is a pure function of the observation's
// task channels; the DLS solver turns it into joint targets.
// ---------------------------------------------------------------------------

/// Gripper-geometry-derived heights the phase scripts are written
/// against. The collect baseline's `above(o, 0.030)` grasp is specific
/// to the preset gripper, whose plates hang `finger_x − |tcp| = 4.5 cm`
/// BELOW the TCP; a gripper whose plates sit AT the TCP (the imported
/// gripper2f on the Panda) needs the TCP 1.5 cm *below* the object
/// centre for the same plate/object overlap. So every script height is
/// `plate_drop + h`, where `plate_drop` is how far the plate centre
/// hangs below the TCP along the approach axis and `h` is the desired
/// plate-centre height relative to the object centre — the preset
/// reproduces the proven 0.030 / 0.060 numbers exactly.
#[derive(Debug, Clone, Copy)]
struct PhaseGeom {
    /// `finger_x − (tcp_offset · approach_axis)` — plate centre below
    /// the TCP (m); 0.045 for the preset, ≈0 for the Panda gripper2f.
    plate_drop: f32,
    /// Plate half-extent along the approach axis (m): 0.03 for the
    /// preset's 6 cm plates, 0.0085 for the Panda's 1.7 cm finger blocks.
    plate_half: f32,
    /// Heavy-arm (≥6 DoF) knobs — OFF for the 4-axis preset so its
    /// proven golden path stays bit-identical:
    /// - hover-sag calibration: at the end of the approach hover the
    ///   measured TCP sits centimetres below the IK target (an 11 kg
    ///   Panda at 300/30 sags 3–7 cm); the offset is captured once per
    ///   episode and added to every later target;
    /// - plate-gap alignment: pin the plate-separation axis to world Z
    ///   so 1.7 cm finger blocks meet the axis-aligned cube's faces, not
    ///   its corners (the Panda's ready pose has joint7 = 45°).
    sag_comp: bool,
    gap_align: Option<[f32; 3]>,
    /// Heavy-arm grasp-depth offset (m; 0.0 for the 4-axis preset). The
    /// Panda's TCP-centred finger blocks grip the cube's upper quarter
    /// at the generic height (plate bottom 2.5 mm below the cube
    /// centre) — a shallow band the cube slides out of on lift. A
    /// −10 mm offset centres the plates on the cube (12.5 mm table
    /// clearance kept), so the squeeze band covers the cube's mid and
    /// the bottom edge can't lever a plate off its top.
    grasp_depth: f32,
}

impl PhaseGeom {
    fn from_robot(robot: &oriera_subsys::RobotSpec) -> Self {
        let heavy = robot.dof() >= 6;
        let (plate_drop, plate_half) = robot
            .gripper
            .as_ref()
            .map(|g| {
                let a = g.approach_axis_or_default();
                let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-9);
                let u = [a[0] / n, a[1] / n, a[2] / n];
                let tcp_along =
                    (g.tcp_offset[0] * u[0] + g.tcp_offset[1] * u[1] + g.tcp_offset[2] * u[2]) / n;
                // The plate collider is axis-aligned to the attach link's
                // frame; its half-extent along the approach is the
                // finger_half component on the approach's dominant axis.
                let idx = if u[0].abs() >= u[1].abs() && u[0].abs() >= u[2].abs() {
                    0
                } else if u[1].abs() >= u[2].abs() {
                    1
                } else {
                    2
                };
                (g.finger_x - tcp_along, g.finger_half[idx])
            })
            .unwrap_or((0.0, 0.0));
        Self {
            plate_drop: mm(f64::from(plate_drop)),
            plate_half: mm(f64::from(plate_half)),
            sag_comp: heavy,
            gap_align: heavy.then_some([0.0, 0.0, 1.0]),
            grasp_depth: if heavy { -0.000 } else { 0.0 },
        }
    }

    /// Grasp: the plate's TOP edge reaches 1.5 cm above the object
    /// centre ⇒ tip = obj + plate_drop + 0.015 − plate_half. The preset
    /// reproduces its proven `obj + 0.030` (plate centre 1.5 cm below
    /// the object centre, plates 6 cm tall); a small TCP-centred
    /// gripper (Panda finger blocks, 1.7 cm) gets its plates centred ON
    /// the object instead of half-buried in the table at the object's
    /// bottom edge — a buried plate wedges the cube and drags it.
    fn grasp(&self) -> f32 {
        mm(
            f64::from(self.plate_drop) + 0.015 - f64::from(self.plate_half)
                + f64::from(self.grasp_depth),
        )
    }

    /// Place over a zone whose `y` is the table-top reference: object
    /// rests 2.5 cm above it, release 5 mm higher ⇒ grasp + 0.030.
    fn place(&self) -> f32 {
        mm(f64::from(self.grasp()) + 0.030)
    }

    /// Stack grasp: plate centre 5 mm ABOVE the object centre so the
    /// plates' bottom edge is at the object's bottom — placing on B then
    /// never dips a plate below B's top ⇒ tip = obj + plate_drop + 0.005.
    fn stack_grasp(&self) -> f32 {
        mm(f64::from(self.plate_drop) + 0.005)
    }
}

/// Round a derived height to the millimetre (f64 → f32) so a geometry
/// path that lands on the preset's constants reproduces the literal
/// `0.030` / `0.060` bit for bit (an f32 subtraction chain is 1 ulp off,
/// which the chaotic contact physics amplifies into a different
/// episode) — and nobody needs sub-millimetre phase heights.
fn mm(v: f64) -> f32 {
    ((v * 1000.0).round() / 1000.0) as f32
}

/// A phase's waypoint law: (object, goal, measured TCP, geometry) → TCP
/// target.
type TargetFn = fn(obj: [f32; 3], goal: [f32; 3], ee: [f32; 3], geom: &PhaseGeom) -> [f32; 3];

struct Phase {
    gripper: f32,
    ticks: u64,
    /// Heavy arms only: freeze the object anchor at this phase's first
    /// tick. The live-object servo behind the closing/lift targets is a
    /// positive feedback loop on the Panda — plate friction drags the
    /// cube a few cm during closing and the target chases the dragged
    /// cube, amplifying the drift (paper@v1: 40% timeouts showed the
    /// cube shoved 2–8 cm off the grasp point). Pinning the anchor to
    /// the closing-start measurement closes on where the cube *was* and
    /// lifts from a fixed frame instead. 4-axis golden path: `false`.
    pin_object: bool,
    /// Heavy arms only: on this phase's LAST tick, re-measure the
    /// command→measurement offset and replace the hover sag with it.
    /// The hover calibration is one configuration sample; the steady
    /// state offset at the *grasp* configuration is the one that decides
    /// whether the plates land centred on the cube (measured 5–10 mm
    /// horizontal shortfall = edge grip = slip on lift). 4-axis: `false`.
    recal_sag: bool,
    /// Target TCP from the live object / goal / measured TCP positions.
    target: TargetFn,
}

/// Object-servo: the TCP target that moves the HELD object to `want`,
/// using the measured TCP→object offset (sag, off-centre grasp) —
/// `tip = ee + (want − obj)`. Feedback: re-read every tick, converges
/// when the object is where it should be, never presses it further.
fn carry_to(obj: [f32; 3], ee: [f32; 3], want: [f32; 3]) -> [f32; 3] {
    [
        ee[0] + (want[0] - obj[0]),
        ee[1] + (want[1] - obj[1]),
        ee[2] + (want[2] - obj[2]),
    ]
}

fn above(p: [f32; 3], h: f32) -> [f32; 3] {
    [p[0], p[1] + h, p[2]]
}

fn lift_to(a: [f32; 3], b: [f32; 3], h: f32) -> [f32; 3] {
    // Straight up from A to B's height + h (the stack carry arc).
    [a[0], (a[1]).max(b[1] + h), a[2]]
}

/// A point `d` ahead of `p` along the horizontal direction `dir` — the
/// drag "carrot" the TCP leads the drawer handle with.
fn toward(p: [f32; 3], dir: [f32; 3], d: f32) -> [f32; 3] {
    let n = (dir[0] * dir[0] + dir[2] * dir[2]).sqrt();
    if n < 1e-6 {
        return p;
    }
    [p[0] + dir[0] / n * d, p[1], p[2] + dir[2] / n * d]
}

/// pick / bin_pick / panda pick — the grasp-carry-place script (heights
/// = the collect baseline's, expressed through [`PhaseGeom`] so the
/// golden preset reproduces them bit for bit and other grippers get the
/// same plate/object geometry). `heavy` (≥6 DoF) switches on the
/// Panda-tuned schedule: longer closing dwell, a two-stage lift and
/// the closing-start object pin — the 4-axis golden path keeps its
/// proven numbers byte for byte.
fn pick_phases(heavy: bool) -> Vec<Phase> {
    if heavy {
        // Heavy-arm schedule (Panda paper@v1 tuning, 2026-11-04):
        // - closing dwell 40 → 60 (plates seat + static friction builds);
        // - an open-gripper SETTLE between descent and closing: the
        //   descent-end recalibration (see [`Phase::recal_sag`]) steps
        //   the command onto the measured shortfall, and that surge
        //   must finish BEFORE the plates touch — a moving kinematic
        //   plate wipes the cube along the table (measured +19 mm shove
        //   during closing = close on empty);
        // - closing pins the object anchor (see [`Phase::pin_object`]);
        // - grasp 10 mm deeper than the generic law: the Panda's
        //   TCP-centred finger blocks otherwise grip the cube's upper
        //   quarter, a band the cube slides out of on lift;
        // - the lift is staged (+5 / +11 / +20 cm) so a rate-limited
        //   11 kg arm accelerates the held cube gently instead of
        //   yanking it out of the plates (the old single-stage lift
        //   slipped the cube mid-rise or dragged it off the table).
        // Total 585 ticks < max_steps 700.
        return vec![
            Phase {
                gripper: 0.0,
                ticks: 90,
                pin_object: false,
                recal_sag: false,
                target: |o, _g, _e, _m| above(o, 0.12),
            },
            Phase {
                gripper: 0.0,
                ticks: 80,
                pin_object: false,
                recal_sag: true,
                target: |o, _g, _e, m| above(o, m.grasp()),
            },
            Phase {
                gripper: 0.0,
                ticks: 15,
                pin_object: true,
                recal_sag: false,
                target: |o, _g, _e, m| above(o, m.grasp()),
            },
            Phase {
                gripper: 1.0,
                ticks: 80,
                pin_object: true,
                recal_sag: false,
                target: |o, _g, _e, m| above(o, m.grasp()),
            },
            Phase {
                gripper: 1.0,
                ticks: 40,
                pin_object: true,
                recal_sag: false,
                target: |o, _g, _e, _m| above(o, 0.05),
            },
            Phase {
                gripper: 1.0,
                ticks: 40,
                pin_object: true,
                recal_sag: false,
                target: |o, _g, _e, _m| above(o, 0.11),
            },
            Phase {
                gripper: 1.0,
                ticks: 70,
                pin_object: true,
                recal_sag: false,
                target: |o, _g, _e, _m| above(o, 0.20),
            },
            Phase {
                gripper: 1.0,
                ticks: 95,
                pin_object: false,
                recal_sag: false,
                target: |_o, g, _e, _m| above(g, 0.20),
            },
            Phase {
                gripper: 1.0,
                ticks: 70,
                pin_object: false,
                recal_sag: false,
                target: |_o, g, _e, m| above(g, m.place()),
            },
            Phase {
                gripper: 0.0,
                ticks: 35,
                pin_object: false,
                recal_sag: false,
                target: |_o, g, _e, m| above(g, m.place()),
            },
            Phase {
                gripper: 0.0,
                ticks: 50,
                pin_object: false,
                recal_sag: false,
                target: |_o, g, _e, _m| above(g, 0.22),
            },
        ];
    }
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 40,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 95,
            pin_object: false,
            recal_sag: false,
            target: |_o, g, _e, _m| above(g, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |_o, g, _e, m| above(g, m.place()),
        },
        Phase {
            gripper: 0.0,
            ticks: 35,
            pin_object: false,
            recal_sag: false,
            target: |_o, g, _e, m| above(g, m.place()),
        },
        Phase {
            gripper: 0.0,
            ticks: 45,
            pin_object: false,
            recal_sag: false,
            target: |_o, g, _e, _m| above(g, 0.22),
        },
    ]
}

fn reach_phases() -> Vec<Phase> {
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 0.0,
            ticks: 400,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
    ]
}

/// stack@v1 — grasp A with the plates spanning its full height, then
/// OBJECT-SERVO the carry and the place: the TCP is commanded so that A
/// itself (measured) goes above B and down to 5 mm above the stacked
/// centre. The loaded 4-axis arm sags ~1.8 cm below a commanded tip and
/// an off-centre grasp shifts A by up to 3 cm — tip-space waypoints then
/// either press A into B (kinematic plates: A gets launched) or drop it
/// 2.5 cm off-centre; servoing A's measured pose absorbs both. Release,
/// retreat.
fn stack_phases() -> Vec<Phase> {
    vec![
        // Retract first: the sagged stretched home holds the plates at
        // block height (0.52–0.58) right over B; folding straight back
        // toward A sweeps them through B (trace: B dragged 7 cm at t≈150).
        // Rise to a point 12 cm OUTSIDE A and 18 cm above it — anchored on
        // the (static) object, never on the live TCP (a TCP-relative
        // target is a carrot that walks the arm behind its own base).
        Phase {
            gripper: 0.0,
            ticks: 40,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| [o[0] + 0.12, o[1] + 0.18, o[2]],
        },
        Phase {
            gripper: 0.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.stack_grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 40,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.stack_grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, g, e, _m| carry_to(o, e, lift_to(o, g, 0.12)),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, g, e, _m| carry_to(o, e, above(g, 0.12)),
        },
        // Release from 1.5 cm above the stacked centre: XZ is servoed to
        // ~2 mm, so the short drop stays on B, while the plates' bottom
        // edge (= A's bottom at this grasp) never dips into B's top — a
        // 5 mm margin was eaten by load sag and squeezed B out sideways.
        Phase {
            gripper: 1.0,
            ticks: 80,
            pin_object: false,
            recal_sag: false,
            target: |o, g, e, _m| carry_to(o, e, above(g, 0.015)),
        },
        Phase {
            gripper: 0.0,
            ticks: 30,
            pin_object: false,
            recal_sag: false,
            target: |o, g, e, _m| carry_to(o, e, above(g, 0.015)),
        },
        Phase {
            gripper: 0.0,
            ticks: 45,
            pin_object: false,
            recal_sag: false,
            target: |_o, g, _e, _m| above(g, 0.22),
        },
    ]
}

/// drawer@v1 — the drawer "handle" is a movable block prop (no prismatic
/// fixture until M3): grasp it, then DRAG it with a leading carrot (the
/// TCP target sits 3 cm ahead of the block toward the goal, re-read every
/// tick, so the block follows at the servo's pace instead of being
/// yanked), release, retreat.
fn drawer_phases() -> Vec<Phase> {
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 40,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 1.0,
            ticks: 240,
            pin_object: false,
            recal_sag: false,
            target: |o, g, _e, m| {
                // Aim 1 cm PAST the goal: with lead → 0 at the goal the block
                // parks a millimetre short of `pull_distance` (seen 3/12).
                let d = ((g[0] - o[0]).powi(2) + (g[2] - o[2]).powi(2)).sqrt() + 0.01;
                let lead = d.min(0.03);
                above(toward(o, [g[0] - o[0], 0.0, g[2] - o[2]], lead), m.grasp())
            },
        },
        Phase {
            gripper: 0.0,
            ticks: 30,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, m| above(o, m.grasp()),
        },
        Phase {
            gripper: 0.0,
            ticks: 45,
            pin_object: false,
            recal_sag: false,
            target: |o, _g, _e, _m| above(o, 0.22),
        },
    ]
}

/// `heavy`（≥6 DoF）只换 pick 的排程（Panda 调参面）；reach/stack/drawer
/// 四轴即官方口径，两种模式同一份。
fn phases_for(task_id: &str, heavy: bool) -> Result<Vec<Phase>, String> {
    match task_id.split('@').next().unwrap_or(task_id) {
        "reach" => Ok(reach_phases()),
        "pick_place" | "bin_pick" => Ok(pick_phases(heavy)),
        "stack" => Ok(stack_phases()),
        "drawer" => Ok(drawer_phases()),
        other => Err(format!(
            "相位机：没有 {other:?} 的脚本（reach/pick_place/bin_pick/stack/drawer）"
        )),
    }
}

/// Where the policy reads its waypoints in the observation vector —
/// looked up by name in the [`ObsLayout`], never hardcoded indices.
struct ObsIdx {
    ee: usize,
    obj_rel: usize,
    goal_rel: usize,
}

impl ObsIdx {
    fn from_layout(layout: &ObsLayout) -> Self {
        let find = |n: &str| {
            layout.index_of(n).unwrap_or_else(|| {
                panic!("obs layout 缺少 {n}（DLS 基线需要 ee/obj_rel/goal_rel）")
            })
        };
        Self {
            ee: find("ee.x"),
            obj_rel: find("obj_rel.x"),
            goal_rel: find("goal_rel.x"),
        }
    }
}

/// The scripted DLS baseline policy: observation → waypoint → DLS →
/// joint targets + gripper demand. The solver warm-starts from its own
/// previous solution (stored in `DlsIk.q`), so each tick is a short
/// correction toward a slowly moving waypoint.
/// Per-episode policy memory: the hover-sag calibration, the closing
/// phase's object pin and the last command (rate limit).
#[derive(Debug, Clone, Default)]
struct PolicyState {
    /// `target − measured TCP` captured on the approach hover's last tick
    /// (zero unless `PhaseGeom::sag_comp`); refreshed at any
    /// [`Phase::recal_sag`] phase's last tick.
    sag: [f32; 3],
    /// Live object position captured on the first tick of a
    /// [`Phase::pin_object`] phase (heavy arms): XZ/Y anchor for the
    /// closing + lift phases, cleared on any other phase.
    pin: Option<[f32; 3]>,
    /// Previous joint command (heavy-arm rate limit; empty = first tick).
    prev_cmd: Vec<f32>,
}

/// Heavy-arm per-tick joint command rate limit (rad/tick ≈ 2.1 rad/s at
/// 60 Hz with the 0.035 default): a smooth path between IK solutions,
/// so a solution that lands on another branch is walked, not jumped.
/// Panda tuning 2026-11-04: 0.035 → 0.026 — the loaded lift/carry was
/// fast enough to slide the cube out of the 2 mm/side friction squeeze
/// (paper@v1 failures lifted the arm cleanly while the cube stayed on
/// the table); the gentler cap trades transit ticks for grip
/// reliability (555-tick schedule vs 700-step budget still fits).
const HEAVY_RATE_LIMIT: f32 = 0.028;

#[allow(clippy::too_many_arguments)]
fn policy(
    obs: &[f32],
    idx: &ObsIdx,
    phases: &[Phase],
    step: u64,
    ik: &mut DlsIk,
    dof: usize,
    down: [f32; 3],
    geom: &PhaseGeom,
    state: &mut PolicyState,
) -> Vec<f32> {
    let ee = [obs[idx.ee], obs[idx.ee + 1], obs[idx.ee + 2]];
    let o = idx.obj_rel;
    let g = idx.goal_rel;
    let obj = [ee[0] + obs[o], ee[1] + obs[o + 1], ee[2] + obs[o + 2]];
    let goal = [ee[0] + obs[g], ee[1] + obs[g + 1], ee[2] + obs[g + 2]];
    let mut ticks = 0u64;
    let active_idx = phases.iter().position(|p| {
        ticks += p.ticks;
        step < ticks
    });
    let active = active_idx.map(|i| &phases[i]);
    let (mut raw, demand) = match active {
        Some(p) => ((p.target)(obj, goal, ee, geom), p.gripper),
        // Past the script: hover above the goal, gripper open.
        None => (above(goal, 0.22), 0.0),
    };
    let pin_active = geom.sag_comp && active.is_some_and(|p| p.pin_object);
    if pin_active {
        // Closing/lift anchor: the object position at this phase's first
        // tick (see [`Phase::pin_object`]); obj-anchored targets are
        // re-anchored to it by swapping the live object out of the law.
        let p = state.pin.get_or_insert(obj);
        raw = [
            p[0] + (raw[0] - obj[0]),
            p[1] + (raw[1] - obj[1]),
            p[2] + (raw[2] - obj[2]),
        ];
    } else {
        state.pin = None;
    }
    // Hover-sag calibration (heavy arms): on the approach hover's last
    // tick, remember how far below the commanded tip the arm settled.
    let hover_end = phases.first().map_or(0, |p| p.ticks.saturating_sub(1));
    if geom.sag_comp && step == hover_end {
        state.sag = [raw[0] - ee[0], raw[1] - ee[1], raw[2] - ee[2]];
    }
    // Grasp-configuration recalibration (heavy arms): on a recal phase's
    // last tick the arm has settled onto the grasp waypoint — the
    // remaining command−measurement gap is the *configuration-specific*
    // steady-state PD offset; fold it into the compensation so the
    // closing phase physically lands on the pinned cube (the hover
    // sample alone left a 5–10 mm horizontal shortfall = edge grip).
    if geom.sag_comp && active.is_some_and(|p| p.recal_sag) && step == ticks - 1 {
        let with_sag = [
            raw[0] + state.sag[0],
            raw[1] + state.sag[1],
            raw[2] + state.sag[2],
        ];
        state.sag = [
            with_sag[0] - ee[0],
            with_sag[1] - ee[1],
            with_sag[2] - ee[2],
        ];
    }
    let target = if geom.sag_comp && step > hover_end {
        [
            raw[0] + state.sag[0],
            raw[1] + state.sag[1],
            raw[2] + state.sag[2],
        ]
    } else {
        raw
    };
    // Restarts only on the episode's first solve (from home): inside the
    // tracking loop a restart may hop joint-space branches.
    ik.max_restarts = if step == 0 { 3 } else { 0 };
    // Lost solver (previous solve > 2 cm off): re-anchor its warm start on
    // the MEASURED joint angles (obs[0..dof]) — the real arm's posture is
    // always a valid, nearby seed; a drifted warm start can lunge into a
    // branch that hangs the tool below the table.
    if step > 0 && ik.last_pos_error > 0.02 && obs.len() >= dof {
        ik.q = obs[..dof].to_vec();
    }
    // Unreachable waypoints honestly stretch toward the target.
    let _ = match geom.gap_align {
        Some(gap) => ik.solve_pose_aligned(target, down, gap),
        None => ik.solve_pose(target, down),
    };
    let mut a = ik.q.clone();
    a.truncate(dof);
    if geom.sag_comp {
        if state.prev_cmd.len() == dof {
            for (c, p) in a.iter_mut().zip(state.prev_cmd.iter()) {
                *c = p + (*c - p).clamp(-HEAVY_RATE_LIMIT, HEAVY_RATE_LIMIT);
            }
        }
        state.prev_cmd = a.clone();
    }
    a.push(demand);
    a
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verify {
    Full,
    Sample,
    Off,
}

struct Args {
    spec: Option<String>,
    sets: Vec<String>,
    episodes: Option<u32>,
    seed0: u64,
    protocol: Option<String>,
    gate: Option<Gate>,
    verify: Verify,
    out: String,
    runs_root: Option<String>,
    reference_out: Option<String>,
    dump_panda_spec: Option<String>,
    force: bool,
    sanitize: bool,
    /// P2 `--failure-phases`：失败相位遥测进报告（opt-in；缺省报告逐字节不变）。
    failure_phases: bool,
}

const USAGE: &str = "--spec/--set/--episodes/--seed0/--seed/--protocol/--gate/--out/--runs-root/--reference-out/--verify-determinism/--dump-panda-spec/--failure-phases/--force";

fn fail(msg: &str) -> ! {
    eprintln!("错误：{msg}");
    std::process::exit(1)
}

fn parse_args() -> Args {
    let mut a = Args {
        spec: None,
        sets: Vec::new(),
        episodes: None,
        seed0: 1000,
        protocol: None,
        gate: None,
        verify: Verify::Full,
        out: "out/embodied-baseline".into(),
        runs_root: None,
        reference_out: None,
        dump_panda_spec: None,
        sanitize: false,
        force: false,
        failure_phases: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--spec" => a.spec = Some(it.next().expect("--spec 需要一个文件路径")),
            "--set" => a.sets.push(it.next().expect("--set 需要 a.b.c=v")),
            "--episodes" => {
                a.episodes = Some(
                    it.next()
                        .expect("--episodes 需要一个值")
                        .parse()
                        .expect("episodes: 数字"),
                )
            }
            "--seed0" | "--seed" => {
                let v = it
                    .next()
                    .unwrap_or_else(|| fail(&format!("{arg} 需要一个值")));
                a.seed0 = v.parse().expect("seed: 数字");
            }
            "--protocol" => a.protocol = Some(it.next().expect("--protocol 需要一个值")),
            "--gate" => {
                let v = it.next().expect("--gate 需要一个值");
                a.gate = Some(match v.as_str() {
                    "hard" => Gate::Hard90,
                    "report" => Gate::Report,
                    other => fail(&format!("--gate {other:?}：须为 hard 或 report")),
                });
            }
            "--verify-determinism" => {
                let v = it.next().expect("--verify-determinism 需要一个值");
                a.verify = match v.as_str() {
                    "full" => Verify::Full,
                    "sample" => Verify::Sample,
                    "off" => Verify::Off,
                    other => fail(&format!(
                        "--verify-determinism {other:?}：须为 full|sample|off"
                    )),
                };
            }
            "--out" => a.out = it.next().expect("--out 需要一个值"),
            "--runs-root" => a.runs_root = Some(it.next().expect("--runs-root 需要一个目录")),
            "--reference-out" => {
                a.reference_out = Some(it.next().expect("--reference-out 需要一个目录"))
            }
            "--sanitize" => a.sanitize = true,
            "--dump-panda-spec" => {
                a.dump_panda_spec = Some(it.next().expect("--dump-panda-spec 需要一个文件路径"))
            }
            "--force" => a.force = true,
            "--failure-phases" => a.failure_phases = true,
            other => fail(&format!("未知参数 {other}（可用 {USAGE}）")),
        }
    }
    a
}

fn resolve_spec(args: &Args) -> SceneSpec {
    let mut spec = match &args.spec {
        Some(path) => {
            let loaded = SceneSpec::load_file(std::path::Path::new(path))
                .unwrap_or_else(|e| fail(&format!("spec 加载失败：{e}")));
            loaded.spec
        }
        None => {
            SceneSpec::load_file(std::path::Path::new(
                "crates/oriera-embodied/specs/official/tabletop-4dof-pick.json",
            ))
            .unwrap_or_else(|e| fail(&format!("默认金标 spec 加载失败：{e}")))
            .spec
        }
    };
    spec.apply_sets(&args.sets)
        .unwrap_or_else(|e| fail(&format!("--set 失败：{e}")));
    spec.validate()
        .unwrap_or_else(|e| fail(&format!("spec 校验失败：{e}")));
    spec
}

// ---------------------------------------------------------------------------
// One-off panda preset generation (--dump-panda-spec)
// ---------------------------------------------------------------------------

/// panda7.urdf + gripper2f.urdf → `panda-7dof-pick` 预设 JSON（M2 一次
/// 性生成逻辑，落盘后入库受治理——改字段 = 新文件）。增益用 WP-D 验证过
/// 的 300/30（armature 0.05）；夹爪按 URDF 导出的板系朝向落轴
/// （approach = 法兰 −Y = 世界下，gap = 法兰 +Z），闭合位按「5 cm 方块
/// 2 mm/侧挤压」取 0.046（URDF 的 0.008 是真夹爪行程，运动学双板近似
/// 会把方块挤飞）。基座钉在桌面左缘（mount [0.02, 0.5, 0]），场景沿用
/// 金标的桌面 + 5 cm 方块工作区。
fn dump_panda_spec(out: &str) {
    let fixture = |name: &str| {
        let path = format!("crates/oriera-embodied/tests/fixtures/{name}");
        std::fs::read_to_string(&path).unwrap_or_else(|e| fail(&format!("读 {path} 失败：{e}")))
    };
    let arm = oriera_agent::urdf::UrdfReport::parse(&fixture("panda7.urdf"))
        .unwrap_or_else(|e| fail(&format!("panda7.urdf 解析失败：{e}")));
    let grip = oriera_agent::urdf::UrdfReport::parse(&fixture("gripper2f.urdf"))
        .unwrap_or_else(|e| fail(&format!("gripper2f.urdf 解析失败：{e}")));
    let mut opts = oriera_agent::urdf::RobotSpecOpts {
        default_kp: 300.0,
        default_kd: 30.0,
        default_armature: 0.05,
        // 落地安装在桌子后方：金标工作区（方块 x 0.14–0.22、目标 x 0.24–0.31）
        // 离基座轴 0.32–0.49 m——Panda 的舒适带。钉在桌沿（x 0.02）时方块离
        // 基座只有 0.14–0.20 m，落在它自身遮挡的近区，工具朝下的 DLS 解不
        // 存在（相位 1 残差 0.2 m，抬举目标不可达 → 抓到也丢）；退到 −0.25 时
        // 抬举点（方块上方 0.20 + 6 cm 沉降补偿、手朝下）又压在外缘（link7 离肩
        // 0.84 m vs 极限 0.855）——DLS 折中把方块沿桌面拖走；−0.18 两头都留余量。
        mount: [-0.18, 0.0, 0.0],
        ..oriera_agent::urdf::RobotSpecOpts::default()
    };
    let mut robot = arm
        .to_robot_spec_with_gripper(&grip, &mut opts)
        .unwrap_or_else(|e| fail(&format!("panda7 + gripper2f 导入失败：{e}")));
    robot
        .validate()
        .unwrap_or_else(|e| fail(&format!("robot 非法：{e}")));

    // 板系朝向：URDF 导出的 tcp_offset 即「法兰 → 指板中心」的世界（=
    // 挂载系）方向；approach 取它，gap 取法兰 +Z（指板薄边方向，与导入
    // 的 finger_half 排布一致）；finger_x 从法兰原点起算。
    let snap = |v: f32| if v.abs() < 1e-6 { 0.0 } else { v };
    let snap3 = |a: [f32; 3]| [snap(a[0]), snap(a[1]), snap(a[2])];
    if let Some(g) = robot.gripper.as_mut() {
        let n = (g.tcp_offset[0] * g.tcp_offset[0]
            + g.tcp_offset[1] * g.tcp_offset[1]
            + g.tcp_offset[2] * g.tcp_offset[2])
            .sqrt();
        let approach = snap3([
            g.tcp_offset[0] / n,
            g.tcp_offset[1] / n,
            g.tcp_offset[2] / n,
        ]);
        g.tcp_offset = snap3(g.tcp_offset);
        g.approach_axis = Some(approach);
        g.gap_axis = Some([0.0, 0.0, 1.0]);
        g.finger_x = n;
        // 运动学双板：按 5 cm 方块 2 mm/侧挤压钉闭合位（URDF 行程 0.008
        // 是真夹爪的，双板近似下会把方块弹飞）。
        g.gap_closed = 0.046;
        g.gap_open = 0.088;
    }
    // URDF rpy 三角函数的 1e-9 级噪声：规范化到 0，预设文件可读且可审。
    for l in robot.links.iter_mut() {
        l.origin = snap3(l.origin);
        l.collider_offset = snap3(l.collider_offset);
    }
    for j in robot.joints.iter_mut() {
        j.anchor = snap3(j.anchor);
        j.axis = snap3(j.axis);
    }
    // home = Franka 标准 ready 位（法兰朝下、TCP 前伸 ~0.3 m）：URDF 零位
    // 是竖直朝天的奇异位形（link7 在挂点上方 1.03 m），既不是抓取起点、
    // 也是 DLS 最差的热启动；ready 位全部在限位内（joint4 −2.356 ∈
    // [−3.07, −0.07]，joint6 1.571 ∈ [−0.0175, 3.75]）。
    robot.home = vec![0.0, -0.785, 0.0, -2.356, 0.0, 1.571, 0.785];

    let golden = SceneSpec::load_file(std::path::Path::new(
        "crates/oriera-embodied/specs/official/tabletop-4dof-pick.json",
    ))
    .unwrap_or_else(|e| fail(&format!("金标 spec 加载失败：{e}")))
    .spec;
    let spec = SceneSpec {
        schema: golden.schema.clone(),
        meta: oriera_agent::spec::SpecMeta {
            name: "panda-7dof-pick".into(),
            author: Some("oriera-embodied-maintainers".into()),
            notes: Some(
                "Panda 7-DoF（tests/fixtures/panda7.urdf 经 to_robot_spec_with_gripper 生成后落 JSON）\
                 + gripper2f 外部夹爪挂末端（板系 approach=法兰 −Y、gap=法兰 +Z，闭合位按 5 cm 方块 2 mm/侧挤压取 0.046）；\
                 kp/kd = WP-D 验证过的 300/30、armature 0.05；home = Franka ready 位；落地安装在桌子后方 [-0.18, 0, 0]\
                 （金标工作区离基座 0.32–0.49 m：出自遮挡近区、抬举点仍留 5 cm 外缘余量——退到 −0.25 时抬举点压在 0.855 m 外缘，DLS 折中把方块沿桌面拖走）；场景/任务沿用金标工作区\
                 （--dump-panda-spec 一次性生成，改字段 = 新文件）"
                    .into(),
            ),
            difficulty: Some("进阶".into()),
        },
        robot,
        scene: golden.scene.clone(),
        task: golden.task.clone(),
        control: golden.control.clone(),
        dt: golden.dt,
        max_steps: golden.max_steps,
        randomization: golden.randomization.clone(),
        physics: golden.physics,
    };
    spec.validate()
        .unwrap_or_else(|e| fail(&format!("panda 预设校验失败：{e}")));
    let path = std::path::Path::new(out);
    std::fs::write(path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
    println!(
        "  ✓ panda 预设 → {}（{} DoF，spec_hash {}）",
        path.display(),
        spec.robot.dof(),
        spec.spec_hash_hex()
    );
}

// ---------------------------------------------------------------------------
// Episode running + protocol
// ---------------------------------------------------------------------------

struct RunResult {
    rows: Vec<EpisodeRow>,
    views: Vec<SuccessView>,
    /// P2 failure-phase signals per kept episode (draw order) — always
    /// folded (read-only), emitted only under `--failure-phases`.
    signals: Vec<PhaseSignals>,
    hashes: Vec<u64>,
    total_steps: u64,
    draws: u64,
    redraws: u64,
}

fn make_env(spec: &SceneSpec) -> TableTopEnv {
    spec.to_env()
}

/// One episode: reset(seed) → phase script → DLS per tick. Dual success
/// (once / at_end) tracked here and returned alongside; the row's
/// `success` keeps the legacy once-semantics so the frozen protocol
/// struct is unchanged. The failure-phase signals are folded from
/// read-only env channels after every step (P2) — no write, so the
/// trajectory is the pre-P2 one bit for bit.
fn run_episode_once(
    env: &mut TableTopEnv,
    seed: u64,
    phases: &[Phase],
    ik: &mut DlsIk,
    idx: &ObsIdx,
    geom: &PhaseGeom,
) -> Result<(EpisodeOutcome, SuccessView, PhaseSignals), String> {
    let mut obs = env.reset(seed);
    let mut signals = PhaseSignals::start(env.cube_pos());
    let dof = env.robot.dof();
    let down = [0.0f32, -1.0, 0.0];
    // The solver restarts from home with the arm: an episode's IK path
    // must not depend on where the previous episode's solver ended (a
    // bad branch would otherwise leak into every later episode, and a
    // single-seed run would differ from the batch).
    ik.q = env.robot.home.clone();
    // ORIERA_SIM_TRACE=<every N ticks> (any non-number = 20).
    let trace_every: u64 = std::env::var("ORIERA_SIM_TRACE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let trace = std::env::var("ORIERA_SIM_TRACE").is_ok();
    let mut state = PolicyState::default();
    let mut step = 0u64;
    let mut terminated = false;
    let mut truncated = false;
    let mut once = false;
    let mut at_end = false;
    let mut reward_sum = 0.0f32;
    while step < env.spec().max_steps && !terminated && !truncated {
        let action = policy(&obs, idx, phases, step, ik, dof, down, geom, &mut state);
        let out = env.step(&action);
        signals.observe(env.ee_pos(), env.cube_pos(), env.goal(), env.holding());
        if trace && step.is_multiple_of(trace_every) {
            let c = env.cube_pos();
            let e = env.ee_pos();
            let b = env.prop1_pos();
            eprintln!(
                "    t={step:3} grip={:.0} hold={} obj=({:.3},{:.3},{:.3}) B=({:.3},{:.3},{:.3}) ee=({:.3},{:.3},{:.3}) ik={:.4}",
                action[dof],
                env.holding(),
                c[0], c[1], c[2],
                b[0], b[1], b[2],
                e[0], e[1], e[2],
                ik.last_pos_error
            );
            if std::env::var("ORIERA_SIM_TRACE_FK").is_ok() {
                let fk = oriera_agent::ik::fk_pose(&env.robot, &obs[..dof.min(obs.len())]);
                eprintln!(
                    "      fk gap=({:.3},{:.3},{:.3}) appr=({:.3},{:.3},{:.3})",
                    fk.gap[0], fk.gap[1], fk.gap[2], fk.approach[0], fk.approach[1], fk.approach[2]
                );
            }
        }
        obs = out.obs;
        reward_sum += out.reward;
        terminated = out.terminated;
        truncated = out.truncated;
        once |= out.success;
        at_end = out.success;
        step += 1;
    }
    if step < 5 {
        return Ok((EpisodeOutcome::Degenerate, SuccessView::default(), signals));
    }
    let fp = env.fingerprint();
    let rand = env.randomization_receipt().to_string();
    if std::env::var("ORIERA_SIM_DEBUG").is_ok() {
        let c = env.cube_pos();
        let g = env.goal();
        let e = env.ee_pos();
        // FK over the MEASURED angles: world approach/gap axes = the
        // flange orientation the plates actually closed along (a tilted
        // gap axis wedges the cube instead of gripping it).
        let fk = oriera_agent::ik::fk_pose(&env.robot, &obs[..dof.min(obs.len())]);
        eprintln!(
            "seed {seed}: once={once} at_end={at_end} steps={step} hold={} obj=({:.3},{:.3},{:.3}) goal=({:.3},{:.3},{:.3}) ee=({:.3},{:.3},{:.3})",
            env.holding(),
            c[0], c[1], c[2],
            g[0], g[1], g[2],
            e[0], e[1], e[2]
        );
        eprintln!(
            "  fk q=[{}]",
            obs[..dof]
                .iter()
                .map(|v| format!("{v:.3}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        eprintln!(
            "  fk approach=({:.3},{:.3},{:.3}) gap=({:.3},{:.3},{:.3}) tip=({:.3},{:.3},{:.3})",
            fk.approach[0],
            fk.approach[1],
            fk.approach[2],
            fk.gap[0],
            fk.gap[1],
            fk.gap[2],
            fk.tip[0],
            fk.tip[1],
            fk.tip[2]
        );
    }
    let view = SuccessView { once, at_end };
    Ok((
        EpisodeOutcome::Done(EpisodeRow {
            seed,
            success: once,
            steps: step,
            reward_sum,
            fingerprint_v1: fp,
            rand,
        }),
        view,
        signals,
    ))
}

fn collect(spec: &SceneSpec, protocol: &EvalProtocol) -> Result<RunResult, String> {
    let mut env = make_env(spec);
    let idx = ObsIdx::from_layout(env.obs_layout());
    let geom = PhaseGeom::from_robot(&env.robot);
    let phases = phases_for(spec.task.id.as_str(), geom.sag_comp)?;
    let mut ik = DlsIk::new(&env.robot, &env.robot.home);
    let mut total_steps = 0u64;
    let mut views: Vec<SuccessView> = Vec::new();
    let mut signals: Vec<PhaseSignals> = Vec::new();
    let result = eval::evaluate(protocol, |seed, _draw| {
        let (out, view, sig) = run_episode_once(&mut env, seed, &phases, &mut ik, &idx, &geom)?;
        if let EpisodeOutcome::Done(row) = &out {
            total_steps += row.steps;
            views.push(view);
            signals.push(sig);
        }
        Ok(out)
    })?;
    Ok(RunResult {
        views,
        signals,
        hashes: result.rows.iter().map(|r| r.fingerprint_v1).collect(),
        rows: result.rows,
        total_steps,
        draws: result.draws,
        redraws: result.redraws,
    })
}

fn main() {
    let args = parse_args();
    if let Some(out) = &args.dump_panda_spec {
        dump_panda_spec(out);
        return;
    }
    let spec = resolve_spec(&args);
    let spec_hash = spec.spec_hash_hex();

    let entry = task_entry(&spec.task.id).unwrap_or_else(|e| fail(&e));
    let protocol = EvalProtocol::parse(
        args.protocol.as_deref().unwrap_or(eval::PROTOCOL_COURSE_V1),
        args.episodes,
        args.seed0,
    )
    .unwrap_or_else(|e| fail(&e))
    .with_gate(args.gate)
    .unwrap_or_else(|e| fail(&e))
    .with_success_criteria(entry.success_criteria)
    .unwrap_or_else(|e| fail(&e));

    let out: std::path::PathBuf = match &args.runs_root {
        Some(root) => {
            let proto8 = protocol.protocol_hash8();
            let method = format!("dls-ik-p{proto8}");
            let dir = std::path::Path::new(root)
                .join(spec.spec_hash8())
                .join(&method)
                .join(protocol.seed0.to_string());
            let done = dir.join("eval_report.json");
            if done.exists() && !args.force {
                println!(
                    "  ↷ 断点续跑：{} 已有 eval_report.json，跳过（--force 重跑）",
                    dir.display()
                );
                return;
            }
            dir
        }
        None => std::path::Path::new(&args.out).to_path_buf(),
    };
    std::fs::create_dir_all(&out).unwrap();

    write_run_manifest_opts(
        out.join("run_manifest.json"),
        &spec,
        &ManifestExtra {
            protocol_id: protocol.id.clone(),
            protocol_hash: protocol.protocol_hash_hex(),
            method: "dls-ik".into(),
            seed: Some(protocol.seed0),
        },
        args.sanitize,
    )
    .unwrap_or_else(|e| fail(&format!("run_manifest.json 写入失败：{e}")));

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║  OriEra DLS 数值 IK 基线 · 七预设 × course@v1 双报            ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!(
        "  任务 {}（{}，判据 {}） · 协议 {}（{} 集，种子 {}..）",
        env_phrase(&spec),
        spec.task.id,
        entry.success_criteria,
        protocol.id,
        protocol.episodes,
        protocol.seed0
    );
    println!(
        "  robot {} ({} DoF) · spec_hash {} · protocol_hash {}",
        spec.robot.name,
        spec.robot.dof(),
        spec_hash,
        protocol.protocol_hash_hex()
    );

    let t0 = std::time::Instant::now();
    let r1 = collect(&spec, &protocol).unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
    match args.verify {
        Verify::Full => {
            let r2 = collect(&spec, &protocol).unwrap_or_else(|e| fail(&format!("采集失败：{e}")));
            assert_eq!(r1.hashes, r2.hashes, "episode fingerprints must match");
            assert_eq!(r1.rows, r2.rows, "episode outcomes must be bit-identical");
            println!("  ✓ 确定性复核（full）：双跑逐位一致");
        }
        Verify::Sample => {
            let seed0 = r1.rows.first().expect("at least one episode").seed;
            let mut env = make_env(&spec);
            let idx = ObsIdx::from_layout(env.obs_layout());
            let geom = PhaseGeom::from_robot(&env.robot);
            let phases = phases_for(&spec.task.id, geom.sag_comp).unwrap_or_else(|e| fail(&e));
            let mut ik = DlsIk::new(&env.robot, &env.robot.home);
            let (re, _, _) = run_episode_once(&mut env, seed0, &phases, &mut ik, &idx, &geom)
                .unwrap_or_else(|e| fail(&format!("sample 复核失败：{e}")));
            let row = match re {
                EpisodeOutcome::Done(row) => row,
                EpisodeOutcome::Degenerate => panic!("sample 复核：首集不应是退化集"),
            };
            let orig = r1.rows.first().unwrap();
            assert_eq!(
                (
                    row.seed,
                    row.steps,
                    row.success,
                    row.reward_sum.to_bits(),
                    row.fingerprint_v1
                ),
                (
                    orig.seed,
                    orig.steps,
                    orig.success,
                    orig.reward_sum.to_bits(),
                    r1.hashes[0]
                ),
                "sample 复核：首集逐位不一致"
            );
            println!("  ✓ 确定性复核（sample）：首集 seed {seed0} 重跑逐位一致");
        }
        Verify::Off => println!("  · 确定性复核关闭（--verify-determinism=off）"),
    }
    let secs = t0.elapsed().as_secs_f64();

    let dual = eval::summarize_success(&r1.views);
    let rate = result_rate(&r1);
    println!(
        "  ✓ {} 集 · {} transitions · once {}/{}（{:.1}%）· at_end {}/{} · 吞吐 {:.0} steps/s（单跑 {:.1}s）",
        dual.episodes,
        r1.total_steps,
        dual.successes_once,
        dual.episodes,
        dual.success_once_rate * 100.0,
        dual.successes_at_end,
        dual.episodes,
        r1.total_steps as f64 / secs.max(1e-9),
        secs
    );

    // 任务专属闸（P-11）：金标桌面预设仍 90%，新任务首版 75%/60% 诚实
    // 门槛。达不到 = 如实报告数字（eval_report 照写）+ 非零退出（仅
    // `--gate hard`，即协议默认；`--gate report` 只记录），不许调判据凑。
    let (gate_threshold, gate_why) = baseline_gate(&spec.task.id, &spec.robot.name)
        .unwrap_or((0.0, "本任务不设基线闸（只记录）"));
    let pass = rate >= gate_threshold;
    // P0-3：report-only 协议（paper@v1）下「fail」是理论参考而非拦截
    // ——字样改为「记录」，避免实验者误判流程被拦（实测 exit 0）。
    let verdict_word = if gate_threshold <= 0.0 {
        "not-applied"
    } else if pass {
        "pass"
    } else if protocol.gate != Gate::Hard90 {
        // report-only：理论参考未达标只记录（实测 exit 0）
        "record-only"
    } else {
        "fail"
    };
    println!(
        "  闸 [{verdict_word}] ≥ {:.0}%（{gate_why}）",
        gate_threshold * 100.0
    );

    // P2 失败相位遥测（opt-in）：每集按 failure_phase@v1 规则归因（成功集
    // 为 null），聚合块 + 每集信号只在 `--failure-phases` 下进报告——不给
    // 则 eval_report.json 逐字节不变（只增不改，既有键零改动）。
    let phase_rules = FailurePhaseRules::for_task(&spec.task.id, &spec.task.params);
    let phase_rows: Vec<(u64, Option<eval::FailurePhase>)> = r1
        .rows
        .iter()
        .zip(&r1.signals)
        .map(|(row, sig)| (row.seed, classify_failure(row.success, sig, &phase_rules)))
        .collect();
    let phase_summary = summarize_failure_phases(&phase_rows, phase_rules);
    if args.failure_phases {
        let b = &phase_summary.by_phase;
        let s = &phase_summary.seeds;
        let seeds_txt: Vec<String> = [
            ("reach", &s.reach),
            ("grasp", &s.grasp),
            ("lift", &s.lift),
            ("place", &s.place),
        ]
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k} {v:?}"))
        .collect();
        println!(
            "  · 失败相位（{}）：失败 {}/{} → reach {} · grasp {} · lift {} · place {}{}",
            phase_summary.scheme,
            phase_summary.failures,
            phase_summary.episodes,
            b.reach,
            b.grasp,
            b.lift,
            b.place,
            if seeds_txt.is_empty() {
                String::new()
            } else {
                format!("（seeds：{}）", seeds_txt.join("；"))
            }
        );
    }

    // eval_report.json（双报：once / at_end + Wilson；协议 + 判据 + 闸）
    let per_ep: Vec<serde_json::Value> = r1
        .rows
        .iter()
        .zip(r1.views.iter())
        .enumerate()
        .map(|(i, (row, view))| {
            let mut v = serde_json::json!({
                "seed": row.seed,
                "success": row.success,
                "success_once": view.once,
                "success_at_end": view.at_end,
                "steps": row.steps,
                "reward_sum": ((row.reward_sum as f64) * 1000.0).round() / 1000.0,
                "rand": row.rand.replace('"', "'"),
            });
            if args.failure_phases {
                v["failure_phase"] = serde_json::json!(phase_rows[i].1.map(|p| p.as_str()));
                v["phase_signals"] = phase_signals_json(&r1.signals[i]);
            }
            v
        })
        .collect();
    let mut report = serde_json::json!({
        "task": entry.phrase,
        "task_id": spec.task.id,
        "method": "dls-ik",
        "protocol": {
            "id": protocol.id,
            "parameter_hash": protocol.protocol_hash_hex(),
            "episodes": protocol.episodes,
            "seed0": protocol.seed0,
            "degenerate": protocol.degenerate,
            "gate": protocol.gate,
            "success_criteria": protocol.success_criteria,
        },
        "baseline_gate": {
            "threshold": gate_threshold,
            "why": gate_why,
            "verdict": verdict_word,
        },
        "spec_hash": spec_hash,
        "identity_hash": format!("{:016x}", robot_identity_hash(&spec.robot)),
        "engine": concat!("oriera-engine/", env!("CARGO_PKG_VERSION")),
        "backend": "rigid3d",
        "dt": spec.dt,
        "episodes": dual.episodes,
        "draws": r1.draws,
        "redraws": r1.redraws,
        "successes": dual.successes_once,
        "success_rate": rate,
        "success_once_rate": dual.success_once_rate,
        "success_at_end_rate": dual.success_at_end_rate,
        "wilson95_once": dual.once_wilson95,
        "wilson95_at_end": dual.at_end_wilson95,
        "throughput_steps_per_s": r1.total_steps as f64 / secs.max(1e-9),
        "verify_determinism": match args.verify { Verify::Full => "full", Verify::Sample => "sample", Verify::Off => "off" },
        "runs": per_ep,
    });
    if args.failure_phases {
        // 经字符串回读：f32 阈值按 ryu 最短表示落盘（0.09 而非 f64 展开的
        // 0.09000000357627869）。
        report["failure_phases"] = serde_json::from_str(
            &serde_json::to_string(&phase_summary).expect("failure-phase summary serializes"),
        )
        .expect("failure-phase summary re-parses");
    }
    let report_path = out.join("eval_report.json");
    std::fs::write(
        &report_path,
        format!("{}\n", serde_json::to_string(&report).unwrap()),
    )
    .unwrap();

    println!(
        "  ✓ 评测：once {:.1}%（Wilson [{:.1}%, {:.1}%]）· at_end {:.1}%（Wilson [{:.1}%, {:.1}%]）· 协议 {}（{}）",
        dual.success_once_rate * 100.0,
        dual.once_wilson95.0 * 100.0,
        dual.once_wilson95.1 * 100.0,
        dual.success_at_end_rate * 100.0,
        dual.at_end_wilson95.0 * 100.0,
        dual.at_end_wilson95.1 * 100.0,
        protocol.id,
        protocol.protocol_hash_hex()
    );
    println!(
        "  ✓ 已写出 {}（manifest {}）",
        report_path.display(),
        out.join("run_manifest.json").display()
    );

    // results/reference/<spec_hash8>/<protocol>.json —— 本次实测参考数
    // （baselines 注册表 results/reference/ 由 --reference-out 落盘）。
    if let Some(dir) = &args.reference_out {
        let dst = std::path::Path::new(dir).join(spec.spec_hash8());
        std::fs::create_dir_all(&dst).unwrap();
        let path = dst.join(format!("{}.json", protocol.id.replace('@', "_")));
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
        )
        .unwrap_or_else(|e| fail(&format!("--reference-out 写入失败：{e}")));
        println!("  ✓ 参考数 → {}", path.display());
    }

    // 闸结论最后生效：报告与参考数已落盘（如实记录），`--gate hard`
    // （协议默认）下未过门槛以退出码 2 收尾——与 collect 的 90% 闸同款
    // 人话（G-2），`--gate report` 只记录不拦流程。
    if gate_threshold > 0.0 && !pass && protocol.gate == Gate::Hard90 {
        eprintln!(
            "\n【基线闸未过】DLS 基线只成功 {}/{} 集（{:.1}%），低于 {:.0}% 门槛 —— 数字已如实写入 {}，不调判据凑。\n\
             这是「新任务首版」的诚实数字：先看失败集（steps 小 = 抓不住，大 = 放不准），\n\
             再决定是修相位机、调预设物理还是维持门槛；确认是能力不足就如实入册。只记录不拦流程请加 --gate report。",
            dual.successes_once,
            dual.episodes,
            rate * 100.0,
            gate_threshold * 100.0,
            report_path.display()
        );
        std::process::exit(2);
    }
}

fn env_phrase(spec: &SceneSpec) -> &'static str {
    task_entry(&spec.task.id).map(|e| e.phrase).unwrap_or("")
}

/// Round to 0.1 mm for the report (the classifier ran on full precision).
fn r4(v: f32) -> f64 {
    ((v as f64) * 1e4).round() / 1e4
}

/// One episode's `phase_signals` report object (`--failure-phases`).
fn phase_signals_json(s: &PhaseSignals) -> serde_json::Value {
    serde_json::json!({
        "obj0": s.obj0.map(r4),
        "ticks": s.ticks,
        "min_ee_obj": r4(s.min_ee_obj),
        "ever_holding": s.ever_holding,
        "holding_ticks": s.holding_ticks,
        "max_lift_held": r4(s.max_lift_held),
        "min_goal_xz_held": s.min_goal_xz_held.map(r4),
        "final_goal_xz": r4(s.final_goal_xz),
        "holding_at_end": s.holding_at_end,
    })
}

/// The legacy once-rate (EpisodeRow.success), identical to
/// `result.success_rate()` without cloning the rows.
fn result_rate(r: &RunResult) -> f64 {
    let k = r.rows.iter().filter(|row| row.success).count();
    k as f64 / r.rows.len().max(1) as f64
}
