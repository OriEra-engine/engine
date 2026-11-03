//! DLS 数值逆运动学（M2，PLAN-embodied-general §五风险表「N-DoF 脚本基
//! 线」对策）：对**任意 N-DoF** [`RobotSpec`] 的末端（TCP）做阻尼最小二
//! 乘 IK——数值雅可比，不挑构型。4 轴桌面臂的解析 IK（采集例内）仍然
//! 是那条金标路的基线；这里给的是 Panda 7 轴等任意链的通用路。
//!
//! - **FK**：[`fk_pose`] 沿 spec 的链接/joint 表正向走链（root 固定、
//!   anchor 在父体系、axis 右手正方向、TCP = 末体 + `tcp_offset`、接近轴
//!   = `approach_axis_or_default`）——与 rapier multibody 的构建约定一致
//!   （子体系零位与父体系重合，正转角按右手绕父系 axis）。
//! - **DLS**：`q += Jᵀ (JJᵀ + λ²I)⁻¹ e`，`e = [目标 − TCP; w·(期望接近轴
//!   − 当前接近轴)]`——位置 3 行 + 工具朝向 3 行（期望默认世界向下）。
//!   抓取类任务的相位机需要「工具竖直向下」，纯位置 IK 会漂到歪头抓——
//!   6 行解对 4 轴恰好复现解析解的约束（yaw/2R/wrist 全确定），对 7 轴
//!   留 1 维零空间由阻尼自然消解。每步迭代后夹到关节限位（投影梯度）。
//!
//! 纯函数浮点数学，不碰物理后端——同输入必同输出（确定性契约）。

use oriera_subsys::robot::GRIPPER_GAP_DEFAULT;
use oriera_subsys::{JointKind, RobotSpec, GRIPPER_APPROACH_DEFAULT};

/// 3×3 row-major.
type Mat3 = [[f32; 3]; 3];

/// Rodrigues rotation about a unit axis.
fn axis_rotation(axis: [f32; 3], theta: f32) -> Mat3 {
    let (x, y, z) = (axis[0], axis[1], axis[2]);
    let (s, c) = theta.sin_cos();
    let t = 1.0 - c;
    [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ]
}

fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let cell = |i: usize, j: usize| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
    [
        [cell(0, 0), cell(0, 1), cell(0, 2)],
        [cell(1, 0), cell(1, 1), cell(1, 2)],
        [cell(2, 0), cell(2, 1), cell(2, 2)],
    ]
}

fn mat_vec(a: &Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        a[0][0] * v[0] + a[0][1] * v[1] + a[0][2] * v[2],
        a[1][0] * v[0] + a[1][1] * v[1] + a[1][2] * v[2],
        a[2][0] * v[0] + a[2][1] * v[1] + a[2][2] * v[2],
    ]
}

fn norm3(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// The forward-kinematics result the IK (and the scripted phase
/// machines) read: TCP position + the gripper approach direction in the
/// world frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FkPose {
    /// Tool-centre-point world position.
    pub tip: [f32; 3],
    /// Gripper approach axis in world (unit; `[1,0,0]` without a
    /// gripper — the legacy plate frame default).
    pub approach: [f32; 3],
    /// Gripper gap (plate separation) axis in world (unit; `[0,0,1]`
    /// without a gripper — the legacy default).
    pub gap: [f32; 3],
}

/// Forward kinematics of `q` (rad, joint order) over `robot`: root link
/// fixed at `links[root].origin`, every joint rotates its child frame
/// about the parent-frame axis (right-hand rule) — the same convention
/// the rapier multibody build uses, so FK and the sim agree.
pub fn fk_pose(robot: &RobotSpec, q: &[f32]) -> FkPose {
    let root = robot
        .root_link()
        .expect("RobotSpec::validate guarantees one root");
    let mut p: Vec<[f32; 3]> = vec![[0.0; 3]; robot.links.len()];
    let mut r: Vec<Mat3> =
        vec![[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]; robot.links.len()];
    p[root] = robot.links[root].origin;
    for (i, j) in robot.joints.iter().enumerate() {
        let angle = q.get(i).copied().unwrap_or(0.0);
        let axis_len = norm3(j.axis);
        let axis = [
            j.axis[0] / axis_len,
            j.axis[1] / axis_len,
            j.axis[2] / axis_len,
        ];
        let parent = j.parent_link;
        let child = j.child_link;
        p[child] = [
            p[parent][0]
                + r[parent][0][0] * j.anchor[0]
                + r[parent][0][1] * j.anchor[1]
                + r[parent][0][2] * j.anchor[2],
            p[parent][1]
                + r[parent][1][0] * j.anchor[0]
                + r[parent][1][1] * j.anchor[1]
                + r[parent][1][2] * j.anchor[2],
            p[parent][2]
                + r[parent][2][0] * j.anchor[0]
                + r[parent][2][1] * j.anchor[1]
                + r[parent][2][2] * j.anchor[2],
        ];
        // prismatic（M3-WPB 遗留）：子体不旋转、位置沿父系轴平移 q 米
        //（revolute 才有轴旋转）。mobile-arm 底盘的两个滑轨因此进 FK。
        if j.kind == JointKind::Prismatic {
            p[child][0] += r[parent][0][0] * axis[0] * angle
                + r[parent][0][1] * axis[1] * angle
                + r[parent][0][2] * axis[2] * angle;
            p[child][1] += r[parent][1][0] * axis[0] * angle
                + r[parent][1][1] * axis[1] * angle
                + r[parent][1][2] * axis[2] * angle;
            p[child][2] += r[parent][2][0] * axis[0] * angle
                + r[parent][2][1] * axis[1] * angle
                + r[parent][2][2] * axis[2] * angle;
            r[child] = r[parent];
        } else {
            let local = axis_rotation(axis, angle);
            r[child] = mat_mul(&r[parent], &local);
        }
    }
    let ee_link = robot.gripper.as_ref().map_or_else(
        || robot.joints.last().map_or(root, |j| j.child_link),
        |g| g.attach_link,
    );
    let tcp = robot.gripper.as_ref().map_or([0.0; 3], |g| g.tcp_offset);
    let approach = robot
        .gripper
        .as_ref()
        .map_or(GRIPPER_APPROACH_DEFAULT, |g| g.approach_axis_or_default());
    let unit = norm3(approach).max(1e-9);
    let approach = [approach[0] / unit, approach[1] / unit, approach[2] / unit];
    let gap = robot
        .gripper
        .as_ref()
        .map_or(GRIPPER_GAP_DEFAULT, |g| g.gap_axis_or_default());
    let gunit = norm3(gap).max(1e-9);
    let gap = [gap[0] / gunit, gap[1] / gunit, gap[2] / gunit];
    FkPose {
        tip: [
            p[ee_link][0]
                + r[ee_link][0][0] * tcp[0]
                + r[ee_link][0][1] * tcp[1]
                + r[ee_link][0][2] * tcp[2],
            p[ee_link][1]
                + r[ee_link][1][0] * tcp[0]
                + r[ee_link][1][1] * tcp[1]
                + r[ee_link][1][2] * tcp[2],
            p[ee_link][2]
                + r[ee_link][2][0] * tcp[0]
                + r[ee_link][2][1] * tcp[1]
                + r[ee_link][2][2] * tcp[2],
        ],
        approach: mat_vec(&r[ee_link], approach),
        gap: mat_vec(&r[ee_link], gap),
    }
}

/// One solve's constraint set: tip position (always), optional approach
/// direction, optional plate-gap direction (sign-free).
struct Goal {
    target: [f32; 3],
    approach: Option<[f32; 3]>,
    gap: Option<[f32; 3]>,
}

/// Deterministic restart RNG (fixed seed ⇒ the no-seed-input
/// reproducibility contract holds) — same constants as the env's LCG.
struct RestartRng(u64);

impl RestartRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self, lo: f32, hi: f32) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let f = (self.0 >> 33) as f32 / (u32::MAX >> 1) as f32;
        lo + f * (hi - lo)
    }
}

/// Is `a` a better solve than `b`? Position first, orientation second.
fn better(a: &SolveReport, b: &SolveReport) -> bool {
    (a.pos_error + a.rot_error) < (b.pos_error + b.rot_error)
}

/// Damped-least-squares IK over a cloned [`RobotSpec`]. Warm-starts
/// from `q0` (seed with the measured joint angles) and keeps the last
/// solution in `q` across calls — consecutive phase waypoints are
/// close, so the next solve is a short correction, not a fresh search.
pub struct DlsIk {
    robot: RobotSpec,
    /// Tool-orientation row weight (position rows are weight 1, metres).
    pub rot_weight: f32,
    /// Damping (larger = smoother/shorter steps, more nullspace give).
    pub lambda: f32,
    /// Per-iteration joint-angle step clamp (rad).
    pub max_step: f32,
    /// Finite-difference epsilon for the numeric Jacobian (rad).
    pub eps: f32,
    pub max_iterations: usize,
    /// Convergence tolerances (m, unit-less for the approach dot rows).
    pub tol_pos: f32,
    pub tol_rot: f32,
    /// Current joint solution (also the next solve's warm start).
    pub q: Vec<f32>,
    /// |target − tip| of the most recent solve (m) — diagnostics.
    pub last_pos_error: f32,
    /// Deterministic restarts tried when a solve does not converge
    /// (default 3). A tracking loop should set 0 after its first solve:
    /// a restart can land on another joint-space branch, and a servoed
    /// arm transitioning between branches sweeps its tool through the
    /// workspace.
    pub max_restarts: usize,
}

impl DlsIk {
    /// Build from a validated spec, warm-started at `q0` (home).
    pub fn new(robot: &RobotSpec, q0: &[f32]) -> Self {
        Self {
            robot: robot.clone(),
            rot_weight: 0.5,
            lambda: 0.05,
            max_step: 0.3,
            eps: 1e-4,
            max_iterations: 300,
            tol_pos: 1e-4,
            tol_rot: 2e-3,
            q: q0.to_vec(),
            last_pos_error: 0.0,
            max_restarts: 3,
        }
    }

    fn forward(&self, q: &[f32]) -> FkPose {
        fk_pose(&self.robot, q)
    }

    fn residual(&self, q: &[f32], goal: &Goal) -> Vec<f64> {
        let g = self.forward(q);
        let w = f64::from(self.rot_weight);
        let mut r = vec![
            f64::from(goal.target[0] - g.tip[0]),
            f64::from(goal.target[1] - g.tip[1]),
            f64::from(goal.target[2] - g.tip[2]),
        ];
        if let Some(dir) = goal.approach {
            r.push(w * f64::from(dir[0] - g.approach[0]));
            r.push(w * f64::from(dir[1] - g.approach[1]));
            r.push(w * f64::from(dir[2] - g.approach[2]));
        }
        if let Some(gap) = goal.gap {
            // Plate-separation alignment is sign-free (the two plates are
            // interchangeable): drive the gap axis to ±gap, whichever
            // sign is closer.
            let dot = gap[0] * g.gap[0] + gap[1] * g.gap[1] + gap[2] * g.gap[2];
            let sg = if dot < 0.0 { -1.0 } else { 1.0 };
            r.push(w * f64::from(sg * gap[0] - g.gap[0]));
            r.push(w * f64::from(sg * gap[1] - g.gap[1]));
            r.push(w * f64::from(sg * gap[2] - g.gap[2]));
        }
        r
    }

    /// Solve toward `target` with the approach axis pinned to `dir`
    /// (grasp-style). Returns the solution; `self.q` is left at it (the
    /// next call warm-starts there). `converged()` reports whether both
    /// tolerances are met — unreachable targets return the honest
    /// closest pose, not an error. Local-minimum escape: up to 3
    /// deterministic restarts (fixed-seed LCG perturbations of the
    /// original seed), keeping the best residual.
    pub fn solve_pose(&mut self, target: [f32; 3], dir: [f32; 3]) -> SolveReport {
        self.solve_with_restarts(&Goal {
            target,
            approach: Some(dir),
            gap: None,
        })
    }

    /// Pose solve with the plate-separation axis also pinned (9 rows:
    /// position + approach + gap; sign-free on the gap). Consistent on
    /// ≥6 DoF; a 4-axis arm only gets a least-squares compromise.
    pub fn solve_pose_aligned(
        &mut self,
        target: [f32; 3],
        dir: [f32; 3],
        gap: [f32; 3],
    ) -> SolveReport {
        self.solve_with_restarts(&Goal {
            target,
            approach: Some(dir),
            gap: Some(gap),
        })
    }

    /// Position-only solve (3 rows) — for waypoints where the tool
    /// orientation is free.
    pub fn solve_position(&mut self, target: [f32; 3]) -> SolveReport {
        self.solve_with_restarts(&Goal {
            target,
            approach: None,
            gap: None,
        })
    }

    fn solve_with_restarts(&mut self, goal: &Goal) -> SolveReport {
        let seed = self.q.clone();
        let mut best = self.solve_inner(goal);
        let mut best_q = self.q.clone();
        if !best.converged() {
            // Deterministic restarts: the fixed-seed LCG keeps the
            // no-seed-input reproducibility contract (same call ⇒ same
            // sequence ⇒ same solution).
            let mut rng = RestartRng::new(0x1117_1D15);
            for _ in 0..self.max_restarts {
                let q0: Vec<f32> = seed.iter().map(|v| v + rng.next(-1.2, 1.2)).collect();
                self.q = q0;
                let rep = self.solve_inner(goal);
                if better(&rep, &best) {
                    best = rep;
                    best_q = self.q.clone();
                }
                if best.converged() {
                    break;
                }
            }
            self.q = best_q;
        }
        self.last_pos_error = best.pos_error;
        best
    }

    fn solve_inner(&mut self, goal: &Goal) -> SolveReport {
        let dof = self.robot.joints.len();
        self.q.resize(dof, 0.0);
        let with_dir = goal.approach.is_some() || goal.gap.is_some();
        let n = 3
            + if goal.approach.is_some() { 3 } else { 0 }
            + if goal.gap.is_some() { 3 } else { 0 };
        let mut iterations = 0usize;
        let mut initial_error2 = 0.0f64;
        for iter in 0..self.max_iterations {
            let r = self.residual(&self.q, goal);
            let err2 = r.iter().map(|v| v * v).sum::<f64>();
            if iter == 0 {
                initial_error2 = err2;
            }
            // Converged? Position tolerance on the tip rows; rotation
            // tolerance on the (weighted) direction rows.
            let pos_err2: f64 = r[..3].iter().map(|v| v * v).sum();
            let rot_ok = !with_dir
                || r[3..]
                    .iter()
                    .all(|v| v.abs() < f64::from(self.tol_rot * self.rot_weight));
            if pos_err2.sqrt() < f64::from(self.tol_pos) && rot_ok {
                break;
            }
            // Numeric Jacobian: n rows × dof columns.
            let mut jac = vec![0.0f64; n * dof];
            for (col, _) in self.robot.joints.iter().enumerate() {
                let mut qp = self.q.clone();
                qp[col] += self.eps;
                let rp = self.residual(&qp, goal);
                for (row, _) in r.iter().enumerate() {
                    jac[row * dof + col] = (rp[row] - r[row]) / f64::from(self.eps);
                }
            }
            // Δq = Jᵀ (J Jᵀ + λ²I)⁻¹ r  (n×n solve, partial pivoting)
            let mut jjt = vec![0.0f64; n * n];
            let lam2 = f64::from(self.lambda) * f64::from(self.lambda);
            for i in 0..n {
                for k in 0..n {
                    let mut acc = 0.0;
                    for c in 0..dof {
                        acc += jac[i * dof + c] * jac[k * dof + c];
                    }
                    jjt[i * n + k] = acc + if i == k { lam2 } else { 0.0 };
                }
            }
            let Some(inv) = invert(&mut jjt, n) else {
                break;
            };
            // dq_c = Σ_i jac[i][c] · (inv·r)_i — this is +[Jᵀ(JJᵀ+λ²I)⁻¹r]_c
            // with J = ∂r/∂q (the residual Jacobian, = −∂f/∂q), so the
            // Gauss-Newton DESCENT step is q −= dq (the minus sign is the
            // whole algorithm — the 4-axis analytic IK hides it inside
            // atan2; the numeric solver must not).
            let mut dq = vec![0.0f64; dof];
            for c in 0..dof {
                let mut acc = 0.0;
                for i in 0..n {
                    let m = (0..n).map(|k| inv[i * n + k] * r[k]).sum::<f64>();
                    acc += jac[i * dof + c] * m;
                }
                dq[c] = acc;
            }
            // Step clamp + joint-limit projection.
            let mut worst = 0.0f32;
            for (c, j) in self.robot.joints.iter().enumerate() {
                let step = -dq[c] as f32;
                worst = worst.max(step.abs());
                self.q[c] = (self.q[c] + step).clamp(j.limits.0, j.limits.1);
            }
            iterations += 1;
            if worst < 1e-6 {
                break;
            }
        }
        let final_r = self.residual(&self.q, goal);
        let pos = (final_r[..3].iter().map(|v| v * v).sum::<f64>()).sqrt() as f32;
        let rot = if with_dir {
            (final_r[3..].iter().map(|v| v * v).sum::<f64>()).sqrt() as f32
                / self.rot_weight.max(1e-6)
        } else {
            0.0
        };
        SolveReport {
            iterations,
            pos_error: pos,
            rot_error: rot,
            initial_error2,
        }
    }
}

/// How one solve went: iteration count and the honest residual —
/// unreachable targets report their closest-pose error instead of
/// pretending success.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveReport {
    pub iterations: usize,
    /// |target − tip| after the solve (m).
    pub pos_error: f32,
    /// Angle between the solved approach axis and the requested one
    /// (unit-less on the unit-vector difference; ≈ sin of the angle).
    pub rot_error: f32,
    /// ‖residual‖² before the first iteration (diagnostics).
    pub initial_error2: f64,
}

impl SolveReport {
    /// Both tolerances met (the caller's convergence gate).
    pub fn converged(&self) -> bool {
        self.pos_error < 1e-3 && self.rot_error < 5e-3
    }
}

/// In-place Gauss-Jordan inverse with partial pivoting (`mat` is n×n
/// row-major; replaced by its inverse). `None` on a singular matrix —
/// the caller breaks that iteration (DLS restarts next tick).
fn invert(mat: &mut [f64], n: usize) -> Option<Vec<f64>> {
    let mut a = mat.to_vec();
    let mut inv = vec![0.0f64; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        // Pivot: the largest |a[row][col]| at or below `col`.
        let (pivot, _) = (col..n)
            .map(|r| (r, a[r * n + col].abs()))
            .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if a[pivot * n + col].abs() < 1e-12 {
            return None;
        }
        if pivot != col {
            for k in 0..n {
                a.swap(col * n + k, pivot * n + k);
                inv.swap(col * n + k, pivot * n + k);
            }
        }
        let d = a[col * n + col];
        for k in 0..n {
            a[col * n + k] /= d;
            inv[col * n + k] /= d;
        }
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = a[r * n + col];
            if f == 0.0 {
                continue;
            }
            for k in 0..n {
                a[r * n + k] -= f * a[col * n + k];
                inv[r * n + k] -= f * inv[col * n + k];
            }
        }
    }
    Some(inv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oriera_subsys::{ArmDescription, GripperSpec, JointSpec, LinkShape, LinkSpec};

    /// The 4-DoF preset projection — the base every test robot is
    /// derived from with struct-update syntax, so new RobotSpec /
    /// LinkSpec / GripperSpec fields (WP-A inertia / contact knobs)
    /// inherit the preset's values instead of breaking these literals.
    fn preset() -> RobotSpec {
        RobotSpec::from(&ArmDescription::default())
    }

    fn box_link(name: &str, half: [f32; 3], origin: [f32; 3], off: [f32; 3]) -> LinkSpec {
        LinkSpec {
            name: name.into(),
            shape: LinkShape::Box { half },
            origin,
            collider_offset: off,
            ..preset().links[1].clone()
        }
    }

    fn joint(
        name: &str,
        p: usize,
        c: usize,
        anchor: [f32; 3],
        axis: [f32; 3],
        limits: (f32, f32),
        kp: f32,
    ) -> JointSpec {
        JointSpec {
            name: name.into(),
            parent_link: p,
            child_link: c,
            anchor,
            axis,
            limits,
            kp,
            kd: kp / 15.0,
            ..preset().joints[0].clone()
        }
    }

    /// The 7-axis down-gripper test arm of `tests/gripper_down_smoke.rs`
    /// (yaw + 2R + pitch + roll + yaw + pitch, flange −Z = world down
    /// after the roll, plates hang off the approach axis).
    fn seven_axis_spec() -> RobotSpec {
        let base = preset();
        let sx = 0.02f32;
        let sy = 0.62f32;
        let base_h = 0.06f32;
        let r = 0.02f32;
        let links = vec![
            LinkSpec {
                name: "base".into(),
                shape: LinkShape::Box {
                    half: [0.04, base_h, 0.04],
                },
                origin: [sx, 0.5 + base_h, 0.0],
                collider_offset: [0.0; 3],
                ..base.links[0].clone()
            },
            box_link("carrier", [0.015; 3], [sx, sy, 0.0], [0.0; 3]),
            box_link("upper_arm", [0.07, r, r], [sx, sy, 0.0], [0.07, 0.0, 0.0]),
            box_link(
                "forearm",
                [0.06, r, r],
                [sx + 0.14, sy, 0.0],
                [0.06, 0.0, 0.0],
            ),
            box_link(
                "wrist",
                [0.03, r, r],
                [sx + 0.26, sy, 0.0],
                [0.03, 0.0, 0.0],
            ),
            box_link(
                "roll_seg",
                [0.015, r, r],
                [sx + 0.32, sy, 0.0],
                [0.015, 0.0, 0.0],
            ),
            box_link(
                "yaw_seg",
                [0.015, r, r],
                [sx + 0.35, sy, 0.0],
                [0.015, 0.0, 0.0],
            ),
            box_link(
                "flange",
                [0.01, r, r],
                [sx + 0.38, sy, 0.0],
                [0.01, 0.0, 0.0],
            ),
        ];
        let j =
            |name: &str,
             p: usize,
             c: usize,
             anchor: [f32; 3],
             axis: [f32; 3],
             limits: (f32, f32)| { joint(name, p, c, anchor, axis, limits, 400.0) };
        let pi = std::f32::consts::PI;
        let joints = vec![
            j(
                "base_yaw",
                0,
                1,
                [0.0, 0.12 - base_h, 0.0],
                [0.0, 1.0, 0.0],
                (-pi, pi),
            ),
            j("shoulder", 1, 2, [0.0; 3], [0.0, 0.0, 1.0], (-0.35, 1.45)),
            j(
                "elbow",
                2,
                3,
                [0.14, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                (-2.5, 0.35),
            ),
            j(
                "wrist_pitch",
                3,
                4,
                [0.12, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                (-2.0, 2.0),
            ),
            j(
                "wrist_roll",
                4,
                5,
                [0.06, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                (-pi, pi),
            ),
            j(
                "wrist_yaw",
                5,
                6,
                [0.03, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                (-2.0, 2.0),
            ),
            j(
                "tool_pitch",
                6,
                7,
                [0.03, 0.0, 0.0],
                [0.0, 0.0, 1.0],
                (-2.0, 2.0),
            ),
        ];
        let gripper = GripperSpec {
            attach_link: 7,
            finger_x: 0.06,
            tcp_offset: [0.0, 0.0, -0.06],
            finger_half: [0.030, 0.006, 0.045],
            approach_axis: Some([0.0, 0.0, -1.0]),
            gap_axis: Some([0.0, 1.0, 0.0]),
            ..base.gripper.clone().expect("preset has a gripper")
        };
        RobotSpec {
            name: "ik-test-7dof".into(),
            base: [sx, 0.5, 0.0],
            links,
            joints,
            gripper: Some(gripper),
            home: vec![0.0; 7],
            action_scale: vec![2.0; 7],
            ..base
        }
    }

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() < tol
    }

    fn vec_close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        close(a[0], b[0], tol) && close(a[1], b[1], tol) && close(a[2], b[2], tol)
    }

    #[test]
    fn fk_at_home_matches_the_preset_geometry() {
        let robot = RobotSpec::from(&ArmDescription::default());
        let g = fk_pose(&robot, &robot.home);
        // wrist at [0.40, 0.62, 0] + tcp 0.09 along +X
        assert!(vec_close(g.tip, [0.49, 0.62, 0.0], 1e-5), "{:?}", g.tip);
        assert!(
            vec_close(g.approach, [1.0, 0.0, 0.0], 1e-6),
            "{:?}",
            g.approach
        );
        // yaw 90° about +Y swings +X to −Z: tip = [0.02, 0.62, −0.47]
        let q = [std::f32::consts::FRAC_PI_2, 0.0, 0.0, 0.0];
        let g = fk_pose(&robot, &q);
        assert!(vec_close(g.tip, [0.02, 0.62, -0.47], 1e-5), "{:?}", g.tip);
        assert!(
            vec_close(g.approach, [0.0, 0.0, -1.0], 1e-5),
            "{:?}",
            g.approach
        );
        // shoulder +90° (about +Z) lifts the whole 0.47 m chain vertical:
        // tip = shoulder_y + l1 + l2 + l3 = [0.02, 1.09, 0]
        let q = [0.0, std::f32::consts::FRAC_PI_2, 0.0, 0.0];
        let g = fk_pose(&robot, &q);
        assert!(vec_close(g.tip, [0.02, 1.09, 0.0], 1e-5), "{:?}", g.tip);
    }

    #[test]
    fn dls_recovers_the_downward_grasp_pose_on_the_preset() {
        let robot = RobotSpec::from(&ArmDescription::default());
        // A reachable pick waypoint, inside the workspace (the boundary
        // at x≈0.16 would need shoulder past its 1.45 limit).
        let target = [0.20, 0.62, 0.0];
        let mut ik = DlsIk::new(&robot, &robot.home);
        let rep = ik.solve_pose(target, [0.0, -1.0, 0.0]);
        assert!(
            rep.converged(),
            "4-DoF must solve pos+down: {rep:?} (q {:?})",
            ik.q
        );
        let g = fk_pose(&robot, &ik.q);
        assert!(vec_close(g.tip, target, 1e-3), "{:?} vs {target:?}", g.tip);
        assert!(
            vec_close(g.approach, [0.0, -1.0, 0.0], 5e-3),
            "{:?}",
            g.approach
        );
        // Every joint within its limits (projected gradient never lies).
        for (j, qv) in robot.joints.iter().zip(&ik.q) {
            assert!(*qv >= j.limits.0 - 1e-5 && *qv <= j.limits.1 + 1e-5);
        }
        // Deterministic: same warm start ⇒ same solution bits.
        let mut ik2 = DlsIk::new(&robot, &robot.home);
        let rep2 = ik2.solve_pose(target, [0.0, -1.0, 0.0]);
        assert_eq!(rep, rep2);
        assert_eq!(ik.q, ik2.q);
    }

    #[test]
    fn dls_solves_the_seven_axis_arm_and_honors_limits() {
        let robot = seven_axis_spec();
        robot.validate().expect("test spec must validate");
        // The gripper-down test arm's settled home: the roll turns the
        // flange's −Z into world down, TCP 6 cm below the flange.
        let home = vec![0.0, 0.0, 0.0, 0.0, -std::f32::consts::FRAC_PI_2, 0.0, 0.0];
        let target = [0.30, 0.60, 0.05];
        let mut ik = DlsIk::new(&robot, &home);
        let rep = ik.solve_pose(target, [0.0, -1.0, 0.0]);
        assert!(
            rep.converged(),
            "7-DoF must solve pos+down: {rep:?} (q {:?})",
            ik.q
        );
        let g = fk_pose(&robot, &ik.q);
        assert!(vec_close(g.tip, target, 1e-3), "{:?} vs {target:?}", g.tip);
        assert!(
            vec_close(g.approach, [0.0, -1.0, 0.0], 5e-3),
            "{:?}",
            g.approach
        );
        for (j, qv) in robot.joints.iter().zip(&ik.q) {
            assert!(*qv >= j.limits.0 - 1e-5 && *qv <= j.limits.1 + 1e-5);
        }
        // A nearby waypoint re-solves quickly from the warm start.
        let rep2 = ik.solve_pose([0.32, 0.58, 0.0], [0.0, -1.0, 0.0]);
        assert!(rep2.converged(), "warm-started solve: {rep2:?}");
        assert!(rep2.iterations < 40, "warm start should be cheap: {rep2:?}");
    }

    #[test]
    fn dls_reports_an_honest_closest_pose_when_unreachable() {
        let robot = RobotSpec::from(&ArmDescription::default());
        // Far beyond the 0.4 m reach.
        let mut ik = DlsIk::new(&robot, &robot.home);
        let rep = ik.solve_pose([2.0, 0.6, 0.0], [0.0, -1.0, 0.0]);
        assert!(
            !rep.converged(),
            "stretched-out target must not claim success"
        );
        assert!(rep.pos_error > 0.5, "honest residual: {rep:?}");
        assert!(ik.q.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn position_only_solve_leaves_orientation_free() {
        let robot = seven_axis_spec();
        let home = vec![0.0, 0.0, 0.0, 0.0, -std::f32::consts::FRAC_PI_2, 0.0, 0.0];
        let mut ik = DlsIk::new(&robot, &home);
        let target = [0.25, 0.55, -0.05];
        let rep = ik.solve_position(target);
        assert!(rep.converged(), "{rep:?}");
        let g = fk_pose(&robot, &ik.q);
        assert!(vec_close(g.tip, target, 1e-3));
    }
}
