//! Platform-neutral core of the wasm glue (no `wasm_bindgen` here, so
//! `cargo test -p oriera-wasm` exercises it natively).
//!
//! One physics, four front doors (PLAN-embodied-general §3.2): this
//! module wraps [`TableTopEnv`] exactly the way `python.rs` does for
//! PyO3 — build from a `oriera.scene/2` spec, `reset(seed)`, `step`,
//! `fingerprint()` — and adds what a *renderer* needs on top:
//!
//! * [`SimCore::poses_json`] — world poses of every link, the TCP, the
//!   gripper plates, the task prop and the goal, derived by forward
//!   kinematics from the measured joint angles (the multibody's own
//!   generalized coordinates; body origins sit on their joint anchors
//!   by construction, so FK from the spec reproduces the body poses).
//! * [`SimCore::spec_json`] — the resolved spec, so the renderer builds
//!   its meshes from the same shapes the colliders use.
//! * [`SimCore::baseline_action`] — an f32-exact mirror of the
//!   `embodied_sim_collect` pick_place scripted baseline, used by the
//!   golden check to compare native vs wasm fingerprints with the
//!   *same Rust code* on both sides (§五 risk: libm/FMA bit drift).
//!
//! What this wrapper can NOT read through the public `TableTopEnv` API
//! (reported, not patched — `embodied_env.rs` is another work package's
//! file): the prop's live rotation (`Pose3.rot` sits behind the env's
//! private `world`; only `cube_pos()` is public — the payload flags
//! this with `"rot_known": false`; the 3-line fix is a
//! `TableTopEnv::prop_pose()` forwarder), clutter props beyond
//! `props[0]`, and a noise-free joint-angle read when `obs_noise > 0`
//! (FK uses the observation channel). The plate gap is no longer in
//! that list: the ramp mirror is bit-exact (same f32 recurrence) and
//! `gap()` exposes it (M2-A closes the M1 leftover; the engine-side
//! `MultibodySystem::gap()` read-only view exists for when the env
//! forwards it).

use oriera_agent::embodied_env::{EmbodiedEnv, StepOutcome, TableTopEnv, TaskKind};
use oriera_agent::spec::SceneSpec;

/// The engine's default experiment (= `TableTopEnv::new()`, the
/// `tabletop-4dof-pick` golden) as `oriera.scene/2` JSON — the browser
/// demo's preset when no spec is handed in. Same `spec_hash` as
/// `crates/oriera-agent/specs/tabletop-4dof-pick.json` (pinned by test).
pub fn preset_spec_json() -> String {
    serde_json::to_string(&SceneSpec::preset(TaskKind::PickPlace)).expect("preset serializes")
}

/// Quaternion `[x, y, z, w]` of a rotation `angle` about the unit `axis`.
fn quat_axis_angle(axis: [f32; 3], angle: f32) -> [f32; 4] {
    let (s, c) = (angle * 0.5).sin_cos();
    [axis[0] * s, axis[1] * s, axis[2] * s, c]
}

/// Hamilton product `a ⊗ b` (apply `b` first, then `a`).
fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Rotate `v` by the unit quaternion `q`.
fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v);
    let t = [t[0] * 2.0, t[1] * 2.0, t[2] * 2.0];
    let c = cross(u, t);
    [
        v[0] + q[3] * t[0] + c[0],
        v[1] + q[3] * t[1] + c[1],
        v[2] + q[3] * t[2] + c[2],
    ]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// World pose of one rigid frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
}

impl Pose {
    const IDENTITY_ROT: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
}

// ── Scripted baseline mirror (examples/embodied_sim_collect.rs) ──────
// Copied operation-for-operation in f32 so the wasm build and the
// native collector feed the physics bit-identical targets; any
// fingerprint difference is then the physics, not the policy.

struct Phase {
    gripper: f32,
    ticks: u64,
    target: fn(obj: [f32; 3], goal: [f32; 3]) -> [f32; 3],
}

fn above(p: [f32; 3], h: f32) -> [f32; 3] {
    [p[0], p[1] + h, p[2]]
}

fn pick_place_phases() -> Vec<Phase> {
    vec![
        Phase {
            gripper: 0.0,
            ticks: 70,
            target: |o, _g| above(o, 0.12),
        },
        Phase {
            gripper: 0.0,
            ticks: 60,
            target: |o, _g| above(o, 0.030),
        },
        Phase {
            gripper: 1.0,
            ticks: 40,
            target: |o, _g| above(o, 0.030),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            target: |o, _g| above(o, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 95,
            target: |_o, g| above(g, 0.20),
        },
        Phase {
            gripper: 1.0,
            ticks: 70,
            target: |_o, g| above(g, 0.06),
        },
        Phase {
            gripper: 0.0,
            ticks: 35,
            target: |_o, g| above(g, 0.06),
        },
        Phase {
            gripper: 0.0,
            ticks: 45,
            target: |_o, g| above(g, 0.22),
        },
    ]
}

/// The 4-DoF analytic-IK arm parameters, lifted from the spec the same
/// way the collector lifts them.
struct IkArm {
    base: [f32; 3],
    shoulder_y: f32,
    links: [f32; 3],
    limits: [(f32, f32); 4],
}

impl IkArm {
    fn from_env(env: &TableTopEnv) -> Option<Self> {
        let r = &env.robot;
        if r.dof() != 4 {
            return None;
        }
        let g = r.gripper.as_ref()?;
        let shoulder_link = r.joints[1].child_link;
        Some(Self {
            base: r.base,
            shoulder_y: r.links[shoulder_link].origin[1],
            links: [
                r.joints[2].anchor[0],
                r.joints[3].anchor[0],
                g.tcp_offset[0],
            ],
            limits: [
                r.joints[0].limits,
                r.joints[1].limits,
                r.joints[2].limits,
                r.joints[3].limits,
            ],
        })
    }
}

fn ik(arm: &IkArm, target: [f32; 3]) -> [f32; 4] {
    let (l1, l2) = (arm.links[0], arm.links[1]);
    let shoulder_y = arm.shoulder_y;
    let dx = target[0] - arm.base[0];
    let dz = target[2] - arm.base[2];
    let yaw = f32::atan2(-dz, dx);
    let d = (dx * dx + dz * dz).sqrt().clamp(0.06, l1 + l2 - 0.005);
    let h = (target[1] + arm.links[2] - shoulder_y).clamp(-0.38, 0.30);
    let cos_e = ((d * d + h * h - l1 * l1 - l2 * l2) / (2.0 * l1 * l2)).clamp(-1.0, 1.0);
    let elbow = -cos_e.acos();
    let shoulder = f32::atan2(h, d) - f32::atan2(l2 * elbow.sin(), l1 + l2 * elbow.cos());
    let wrist = -std::f32::consts::FRAC_PI_2 - shoulder - elbow;
    let clamp = |v: f32, i: usize| v.clamp(arm.limits[i].0, arm.limits[i].1);
    [
        clamp(yaw, 0),
        clamp(shoulder, 1),
        clamp(elbow, 2),
        clamp(wrist, 3),
    ]
}

/// Where the baseline reads its waypoints — by layout name, as the
/// collector does.
struct ObsIdx {
    ee: usize,
    obj_rel: usize,
    goal_rel: usize,
}

/// The simulation core: one [`TableTopEnv`] plus the render-side
/// bookkeeping the public API does not expose.
pub struct SimCore {
    env: TableTopEnv,
    spec: SceneSpec,
    spec_hash: String,
    migrated: bool,
    last_obs: Vec<f32>,
    /// Mirror of `MultibodySystem::gap_now` (private): the plate ramp.
    gap_now: f32,
    /// Last clamped gripper demand (0 open … 1 closed).
    gripper_cmd: f32,
    /// Steps since the last reset (the baseline's phase clock).
    steps: u64,
    /// Engine ticks since reset incl. the settle ticks (HUD).
    tick: u64,
    ik: Option<IkArm>,
    obs_idx: Option<ObsIdx>,
}

impl SimCore {
    /// Build from `oriera.scene/2` JSON (v1 drafts migrate on the
    /// fly). Validation errors come back as the engine's own Chinese
    /// field-level messages.
    pub fn from_spec_json(json: &str) -> Result<Self, String> {
        let loaded = SceneSpec::load(json)?;
        let spec = loaded.spec;
        spec.validate()?;
        let env = spec.to_env();
        let spec_hash = spec.spec_hash_hex();
        let gap0 = env.robot.gripper.as_ref().map_or(0.0, |g| g.gap_open);
        let ik = IkArm::from_env(&env);
        let obs_idx = {
            let l = env.obs_layout();
            match (
                l.index_of("ee.x"),
                l.index_of("obj_rel.x"),
                l.index_of("goal_rel.x"),
            ) {
                (Some(ee), Some(obj_rel), Some(goal_rel)) => Some(ObsIdx {
                    ee,
                    obj_rel,
                    goal_rel,
                }),
                _ => None,
            }
        };
        Ok(Self {
            env,
            spec,
            spec_hash,
            migrated: loaded.migrated,
            last_obs: Vec::new(),
            gap_now: gap0,
            gripper_cmd: 0.0,
            steps: 0,
            tick: 0,
            ik,
            obs_idx,
        })
    }

    /// Reset to a fresh episode; `seed` fully determines it.
    pub fn reset(&mut self, seed: u64) -> Vec<f32> {
        let obs = self.env.reset(seed);
        // The physics rebuilt its system: plates sit at `gap_open` and
        // the settle ticks ran with gripper demand 0.
        self.gap_now = self.env.robot.gripper.as_ref().map_or(0.0, |g| g.gap_open);
        self.gripper_cmd = 0.0;
        self.steps = 0;
        self.tick = oriera_agent::embodied_env::SETTLE_TICKS;
        self.last_obs = obs.clone();
        obs
    }

    /// Advance one control step.
    pub fn step(&mut self, action: &[f32]) -> StepOutcome {
        let out = self.env.step(action);
        // Non-finite actions are rejected by the env (no tick happens);
        // mirror that: only advance our clocks on a real step.
        if action.iter().all(|a| a.is_finite()) {
            let dof = self.env.robot.dof();
            self.gripper_cmd = action.get(dof).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            if let Some(g) = self.env.robot.gripper.as_ref() {
                let target = g.gap_open + (g.gap_closed - g.gap_open) * self.gripper_cmd;
                self.gap_now += (target - self.gap_now).clamp(-g.gap_rate, g.gap_rate);
            }
            self.steps += 1;
            self.tick += 1;
        }
        self.last_obs = out.obs.clone();
        out
    }

    /// The last observation (reset or step).
    pub fn obs(&self) -> &[f32] {
        &self.last_obs
    }

    /// Per-episode state fingerprint (#67 semantics).
    pub fn fingerprint(&self) -> u64 {
        self.env.fingerprint()
    }

    /// Scheme-v2 complete-identity fingerprint (PLAN-embodied-v3 §二 #17):
    /// `state ⊕ robot identity ⊕ spec_hash` — the same fold as the native
    /// collector's receipt block. The fold itself lives in
    /// `oriera_agent::eval::fingerprint_set` (which calls the pub
    /// `oriera_subsys::rigid3d::{state_hash_from_v1, robot_identity_hash,
    /// fingerprint_v2_from_parts}`); driving it from here means the wasm
    /// front door hands out the same three-axis receipt as the CLI instead
    /// of the thin v1 view only — no second implementation, nothing that
    /// can drift.
    pub fn fingerprint_v2(&self) -> u64 {
        let spec_hash = u64::from_str_radix(&self.spec_hash, 16).unwrap_or(u64::MAX);
        let v1 = self.env.fingerprint();
        oriera_agent::eval::fingerprint_set(&self.env.robot, spec_hash, &[v1])
            .episode_fingerprints_v2[0]
    }

    pub fn spec_hash(&self) -> &str {
        &self.spec_hash
    }

    pub fn migrated(&self) -> bool {
        self.migrated
    }

    pub fn task_phrase(&self) -> &'static str {
        self.env.task.phrase()
    }

    pub fn task_id(&self) -> &'static str {
        self.env.task_id()
    }

    pub fn dof(&self) -> usize {
        self.env.robot.dof()
    }

    pub fn dt(&self) -> f32 {
        self.env.dt
    }

    pub fn max_steps(&self) -> u64 {
        self.env.max_steps
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn goal(&self) -> [f32; 3] {
        self.env.goal()
    }

    pub fn cube_pos(&self) -> [f32; 3] {
        self.env.cube_pos()
    }

    pub fn ee_pos(&self) -> [f32; 3] {
        self.env.ee_pos()
    }

    pub fn holding(&self) -> bool {
        self.env.holding()
    }

    pub fn gripper(&self) -> f32 {
        self.gripper_cmd
    }

    pub fn randomization_receipt(&self) -> &str {
        self.env.randomization_receipt()
    }

    /// The resolved spec (post-migration) as JSON — the renderer's
    /// source for link/prop shapes, fixtures, joint limits and task
    /// parameters.
    pub fn spec_json(&self) -> String {
        serde_json::to_string(&self.spec).expect("SceneSpec serializes")
    }

    /// Observation/action layouts as JSON:
    /// `{obs:[names], action:[names], obs_slices:[[name,start,end]],
    /// action_slices:[…], obs_dim, action_dim, dof}`.
    pub fn layout_json(&self) -> String {
        let slices = |l: &oriera_agent::embodied_env::ObsLayout| -> Vec<serde_json::Value> {
            l.slices_with_offsets()
                .into_iter()
                .map(|(n, s, e)| serde_json::json!([n, s, e]))
                .collect()
        };
        let obs_l = self.env.obs_layout();
        let act_l = self.env.action_layout();
        serde_json::json!({
            "obs": obs_l.flat_names(),
            "action": act_l.flat_names(),
            "obs_slices": slices(obs_l),
            "action_slices": slices(act_l),
            "obs_dim": obs_l.total,
            "action_dim": act_l.total,
            "dof": self.env.robot.dof(),
            "task_id": self.env.task_id(),
            "task_phrase": self.env.task.phrase(),
            "dt": self.env.dt,
            "max_steps": self.env.max_steps,
        })
        .to_string()
    }

    /// Measured joint angles (the observation's `arm` slice).
    pub fn angles(&self) -> Vec<f32> {
        let dof = self.env.robot.dof();
        if self.last_obs.len() >= dof {
            self.last_obs[..dof].to_vec()
        } else {
            self.env.robot.home.clone()
        }
    }

    /// Current gripper plate gap (m). The mirror is bit-exact by
    /// construction: the engine's ramp (`MultibodySystem::gap_now`) is
    /// the same f32 recurrence over the same demand history and the
    /// same spec constants — this is the M1-leftover read-only view so
    /// renderers stop re-deriving it (U-遗 M1 #2; the engine-side
    /// `MultibodySystem::gap()` exists since M2-A, behind the env's
    /// private `sys` until `TableTopEnv` grows a forwarder).
    pub fn gap(&self) -> f32 {
        self.gap_now
    }

    /// Forward kinematics over the spec chain from `angles`: one world
    /// pose per link, in link order.
    pub fn link_poses(&self, angles: &[f32]) -> Vec<Pose> {
        let r = &self.env.robot;
        let mut poses = vec![
            Pose {
                pos: [0.0; 3],
                rot: Pose::IDENTITY_ROT,
            };
            r.links.len()
        ];
        if let Some(root) = r.root_link() {
            poses[root].pos = r.links[root].origin;
        }
        // Joints are in topological order (validated): the parent's
        // pose is final before its children are visited.
        for (k, j) in r.joints.iter().enumerate() {
            let parent = poses[j.parent_link];
            let theta = angles.get(k).copied().unwrap_or(0.0);
            let rot = quat_mul(parent.rot, quat_axis_angle(j.axis, theta));
            let pos = add(parent.pos, quat_rotate(parent.rot, j.anchor));
            poses[j.child_link] = Pose { pos, rot };
        }
        poses
    }

    /// Everything the renderer places, as JSON (see module docs for the
    /// fields the public API cannot provide — marked in the payload).
    pub fn poses_json(&self) -> String {
        let r = &self.env.robot;
        let angles = self.angles();
        let links = self.link_poses(&angles);
        let pose_json = |p: &Pose| serde_json::json!({ "pos": p.pos, "rot": p.rot });
        let link_arr: Vec<serde_json::Value> = r
            .links
            .iter()
            .zip(links.iter())
            .map(|(l, p)| serde_json::json!({ "name": l.name, "pos": p.pos, "rot": p.rot }))
            .collect();

        // TCP + plates ride the gripper's attach link (or the last
        // joint's child without a gripper).
        let ee_link = r.gripper.as_ref().map_or_else(
            || r.joints.last().map_or(0, |j| j.child_link),
            |g| g.attach_link,
        );
        let ee_frame = links[ee_link];
        let tcp_local = r.gripper.as_ref().map_or([0.0; 3], |g| g.tcp_offset);
        let ee = Pose {
            pos: add(ee_frame.pos, quat_rotate(ee_frame.rot, tcp_local)),
            rot: ee_frame.rot,
        };
        let fingers = r.gripper.as_ref().map(|g| {
            // M0b plate frame: place the plates along the spec's
            // approach/gap axes (legacy +X/+Z when unset — those
            // normalize to the exact unit vectors, so the default
            // placement is bit-identical to the pre-M2 mirror).
            let ap = g.approach_axis_or_default();
            let gp = g.gap_axis_or_default();
            let centre = add(
                ee_frame.pos,
                quat_rotate(
                    ee_frame.rot,
                    [ap[0] * g.finger_x, ap[1] * g.finger_x, ap[2] * g.finger_x],
                ),
            );
            let half = self.gap_now * 0.5;
            let side = quat_rotate(ee_frame.rot, [gp[0] * half, gp[1] * half, gp[2] * half]);
            let left = Pose {
                pos: add(centre, side),
                rot: ee_frame.rot,
            };
            let right = Pose {
                pos: [
                    centre[0] - side[0],
                    centre[1] - side[1],
                    centre[2] - side[2],
                ],
                rot: ee_frame.rot,
            };
            serde_json::json!({
                "gap": self.gap(),
                "half": g.finger_half,
                "left": pose_json(&left),
                "right": pose_json(&right),
            })
        });
        // 全量 props（M2-WPC 集成：stack 的 B 块 / bin 四壁都要渲染）
        let props: Vec<_> = self
            .env
            .scene
            .props
            .iter()
            .enumerate()
            .map(|(i, p)| match self.env.prop_pose(i) {
                Some(pose) => serde_json::json!({
                    "name": p.name,
                    "pos": pose.pos,
                    "rot": pose.rot,
                    "rot_known": true,
                }),
                None => serde_json::json!({
                    "name": p.name,
                    "pos": [0.0, 0.0, 0.0],
                    "rot": Pose::IDENTITY_ROT,
                    "rot_known": false,
                }),
            })
            .collect();
        serde_json::json!({
            "tick": self.tick,
            "steps": self.steps,
            "angles": angles,
            "links": link_arr,
            "ee": pose_json(&ee),
            "ee_telemetry": self.env.ee_pos(),
            "fingers": fingers,
            "props": props,
            "goal": self.env.goal(),
            "holding": self.env.holding(),
            "gripper": self.gripper_cmd,
        })
        .to_string()
    }

    /// The collector's pick_place scripted baseline for `step` (0-based
    /// since reset), read off the last observation. `None` when the
    /// spec is not the 4-DoF + gripper preset family or the task is not
    /// pick_place (the browser falls back to its JS policy).
    pub fn baseline_action(&self, step: u64) -> Option<Vec<f32>> {
        if self.env.task_id() != "pick_place@v1" {
            return None;
        }
        let arm = self.ik.as_ref()?;
        let idx = self.obs_idx.as_ref()?;
        let obs = &self.last_obs;
        if obs.len() < idx.goal_rel + 3 {
            return None;
        }
        let ee = [obs[idx.ee], obs[idx.ee + 1], obs[idx.ee + 2]];
        let o = idx.obj_rel;
        let g = idx.goal_rel;
        let obj = [ee[0] + obs[o], ee[1] + obs[o + 1], ee[2] + obs[o + 2]];
        let goal = [ee[0] + obs[g], ee[1] + obs[g + 1], ee[2] + obs[g + 2]];
        let mut ticks = 0u64;
        for phase in pick_place_phases() {
            ticks += phase.ticks;
            if step < ticks {
                let mut a = ik(arm, (phase.target)(obj, goal)).to_vec();
                a.push(phase.gripper);
                return Some(a);
            }
        }
        let mut a = ik(arm, above(goal, 0.22)).to_vec();
        a.push(0.0);
        Some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = include_str!("../../oriera-agent/specs/tabletop-4dof-pick.json");

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn fk_tip_tracks_physics_telemetry_through_an_episode() {
        // The FK that places the renderer's meshes must agree with the
        // physics' own TCP (JointTelemetry.ee_pos) — at home and after
        // the arm has swung through the baseline's waypoints.
        let mut sim = SimCore::from_spec_json(SPEC).expect("spec loads");
        sim.reset(1000);
        let mut worst = 0.0f32;
        for step in 0..300u64 {
            let a = sim
                .baseline_action(step)
                .expect("4-DoF pick_place baseline");
            sim.step(&a);
            let poses: serde_json::Value = serde_json::from_str(&sim.poses_json()).unwrap();
            let fk: Vec<f32> = poses["ee"]["pos"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect();
            let d = dist([fk[0], fk[1], fk[2]], sim.ee_pos());
            worst = worst.max(d);
        }
        assert!(worst < 2e-3, "FK tip vs telemetry drift {worst} m");
    }

    #[test]
    fn double_run_is_bit_identical() {
        let run = || {
            let mut sim = SimCore::from_spec_json(SPEC).unwrap();
            sim.reset(1000);
            for step in 0..200u64 {
                let a = sim.baseline_action(step).unwrap();
                sim.step(&a);
            }
            (sim.fingerprint(), sim.obs().to_vec())
        };
        assert_eq!(run(), run());
    }

    /// Pull an array of big u64s out of the golden JSON text — serde's
    /// `Value` would route them through f64 and lose bits past 2⁵³, and
    /// this check is exactly about the low bits.
    fn golden_u64_array(text: &str, key: &str) -> Vec<u64> {
        let pat = format!("\"{key}\": [");
        let start = text
            .find(&pat)
            .unwrap_or_else(|| panic!("golden lacks {key}"))
            + pat.len();
        let end = start + text[start..].find(']').expect("golden array unterminated");
        text[start..end]
            .split(',')
            .map(|s| s.trim().parse::<u64>().expect("golden u64"))
            .collect()
    }

    #[test]
    fn fingerprint_v2_matches_the_native_golden_anchor() {
        // Episode 1 (seed 1000) of `contracts/golden/
        // embodied_fingerprints.v2.json` — the native collector's receipt.
        // The wasm side must reproduce BOTH the legacy v1 fold and the
        // scheme-v2 complete-identity fold bit-for-bit (the v2 exit rides
        // `eval::fingerprint_set`, the exact native code path).
        let golden = include_str!("../../../contracts/golden/embodied_fingerprints.v2.json");
        let v1_golden = golden_u64_array(golden, "episode_fingerprints_v1");
        let v2_golden = golden_u64_array(golden, "episode_fingerprints_v2");
        assert_eq!(v1_golden.len(), 12);
        assert_eq!(v2_golden.len(), 12);

        let mut sim = SimCore::from_spec_json(SPEC).expect("spec loads");
        sim.reset(1000);
        let mut step = 0u64;
        loop {
            let a = sim
                .baseline_action(step)
                .expect("4-DoF pick_place baseline");
            let out = sim.step(&a);
            step += 1;
            if out.terminated || step >= sim.max_steps() {
                break;
            }
        }
        assert_eq!(sim.fingerprint(), v1_golden[0], "v1 fold vs golden ep1");
        assert_eq!(sim.fingerprint_v2(), v2_golden[0], "v2 fold vs golden ep1");
    }

    #[test]
    fn layout_and_spec_round_trip() {
        let sim = SimCore::from_spec_json(SPEC).unwrap();
        let l: serde_json::Value = serde_json::from_str(&sim.layout_json()).unwrap();
        assert_eq!(l["obs_dim"], 21);
        assert_eq!(l["action_dim"], 5);
        assert_eq!(l["obs"][0], "base_yaw.pos");
        let s: serde_json::Value = serde_json::from_str(&sim.spec_json()).unwrap();
        assert_eq!(s["schema"], "oriera.scene/2");
        assert_eq!(sim.spec_hash().len(), 16);
        assert_eq!(
            sim.task_phrase(),
            "pick up the cube and place it at the target"
        );
    }

    #[test]
    fn preset_spec_is_the_golden_file() {
        let a = SimCore::from_spec_json(&preset_spec_json()).unwrap();
        let b = SimCore::from_spec_json(SPEC).unwrap();
        assert_eq!(
            a.spec_hash(),
            b.spec_hash(),
            "preset() drifted from specs/tabletop-4dof-pick.json"
        );
    }

    #[test]
    fn bad_spec_is_a_readable_error() {
        let err = match SimCore::from_spec_json("{\"schema\":\"oriera.scene/2\"}") {
            Err(e) => e,
            Ok(_) => panic!("empty schema must be rejected"),
        };
        assert!(!err.is_empty());
        let err = match SimCore::from_spec_json("not json") {
            Err(e) => e,
            Ok(_) => panic!("non-JSON must be rejected"),
        };
        assert!(err.contains("JSON"));
    }
}
