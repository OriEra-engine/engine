// OriEra 具身仿真台 · 渲染器内核（example/15-embodied-sim）。
//
// M1（PLAN-embodied-general §3.2「一套物理，多个前端」）：本文件不再是
// Rust 引擎的 JS 镜像——物理、奖励、任务判定、随机化、指纹全部来自
// vendor/oriera/ 的 oriera-wasm（与 Rust CLI / Python wheel 同一份
// TableTopEnv）。这里只剩：three.js 渲染、交互（键盘/滑条/相机）、
// 录制与导出、CDP 钩子，以及一段「渲染层策略」（PHASES+ik，仅喂动作，
// 不参与判定）。同 spec 同 seed ⇒ 浏览器与 CLI 指纹逐位一致
// （tools/wasm_golden_check.mjs 逐集验收）。
//
// postMessage 桥（iframe 内嵌于 Studio Sim 工作台时）：
//   父→子 {source:"oriera-sim", cmd:"reset"|"policy"|"grip"|"record"|"export"|"spec", ...}
//   子→父 {source:"oriera-sim", type:"state"|"episodes", ...}

import * as THREE from "three";
import init, { OrieraSim } from "../vendor/oriera/oriera_wasm.js";
import { EffectComposer } from "../vendor/jsm/post/EffectComposer.js";
import { RenderPass } from "../vendor/jsm/post/RenderPass.js";
import { UnrealBloomPass } from "../vendor/jsm/post/UnrealBloomPass.js";
import { OutputPass } from "../vendor/jsm/post/OutputPass.js";
import { RoomEnvironment } from "../vendor/jsm/environments/RoomEnvironment.js";
import { RGBELoader } from "../vendor/jsm/loaders/RGBELoader.js";

// ── 渲染层策略（对照 examples/embodied_sim_collect.rs round-4）──────
// 这些只「喂目标角」，不参与奖励/判定——那在 wasm 里。D 为了紧贴
// Rust f32 的 IK，逐运算 Math.fround（f64 双精度中间值舍回 f32）。
const f32 = Math.fround;
const above = (p, h) => [p[0], p[1] + h, p[2]];
const PHASES = [
  { grip: 0, ticks: 70, target: (o) => above(o, 0.12) },
  { grip: 0, ticks: 60, target: (o) => above(o, 0.030) },
  { grip: 1, ticks: 40, target: (o) => above(o, 0.030) },
  { grip: 1, ticks: 70, target: (o) => above(o, 0.20) },
  { grip: 1, ticks: 95, target: (_o, g) => above(g, 0.20) },
  { grip: 1, ticks: 70, target: (_o, g) => above(g, 0.06) },
  { grip: 0, ticks: 35, target: (_o, g) => above(g, 0.06) },
  { grip: 0, ticks: 45, target: (_o, g) => above(g, 0.22) },
];

// IK 臂参数从 spec 提取（与 Rust IkArm::from_robot 同口径）：底座、
// 肩高（肩连杆 origin.y）、肩-肘/肘-腕锚距、TCP 偏移、关节限位。
function ikArmFromSpec(spec) {
  const j = spec.robot.joints;
  if (j.length !== 4) return null;
  const g = spec.robot.gripper;
  if (!g) return null;
  const shoulderLink = j[1].child_link;
  return {
    base: spec.robot.base,
    shoulderY: spec.robot.links[shoulderLink].origin[1],
    links: [j[2].anchor[0], j[3].anchor[0], g.tcp_offset[0]],
    limits: j.map((x) => x.limits),
  };
}

// 解析 IK（4 轴：底座回转 + 平面 2R + 腕俯仰朝下），f32 逐位仿 Rust。
function ik(arm, target) {
  const [l1, l2] = arm.links;
  const shoulderY = arm.shoulderY;
  const dx = f32(target[0] - arm.base[0]);
  const dz = f32(target[2] - arm.base[2]);
  const yaw = f32(Math.atan2(-dz, dx));
  const d = f32(Math.min(Math.max(f32(Math.hypot(dx, dz)), 0.06), f32(l1 + l2 - 0.005)));
  const h = f32(Math.min(Math.max(f32(target[1] + arm.links[2] - shoulderY), -0.38), 0.30));
  let cosE = f32((f32(f32(f32(d * d) + f32(h * h)) - f32(f32(l1 * l1) + f32(l2 * l2))) / f32(2 * l1 * l2)));
  cosE = Math.min(Math.max(cosE, -1), 1);
  const elbow = f32(-Math.acos(cosE));
  const shoulder = f32(Math.atan2(h, d) - Math.atan2(f32(l2 * Math.sin(elbow)), f32(l1 + f32(l2 * Math.cos(elbow)))));
  const wrist = f32(f32(-Math.PI / 2 - shoulder) - elbow);
  const cl = (v, i) => Math.min(Math.max(v, arm.limits[i][0]), arm.limits[i][1]);
  return [cl(yaw, 0), cl(shoulder, 1), cl(elbow, 2), cl(wrist, 3)];
}

// 开环固定节拍相位机（与 Rust policy() 同形——step 计数器对累计
// ticks 判段；收敛门控是旧 JS 物理的代偿，已随物理退役）。
function policyTick(phases, step) {
  let ticks = 0;
  for (let i = 0; i < phases.length; i++) {
    ticks += phases[i].ticks;
    if (step < ticks) return i;
  }
  return -1;
}

// ── N 轴演示策略（P1-B2，创作者-2：Panda「只能遥控不能演示」）──────
// CLI 的 ik.rs 已有 DLS 数值 IK（embodied_sim_baseline 的 paper@v1 口径
// 80/100 就靠它）；这里移植一条**简化版**给浏览器演示（f64 数学，不复刻
// Rust 的 f32 逐位口径——演示内核与引擎基线口径不同的免责声明照旧）：
//   fkPose    = ik.rs fk_pose 的同约定移植（根 link 钉在 origin，转动关
//               节右手定则旋转、prismatic 沿父系轴平移 q 米）；
//   dlsSolve  = DlsIk 的 9 行（位置 + approach + gap 免符号）阻尼最小二
//               乘，数值雅可比、热启动、限幅、关节限位投影——无重启搜
//               索（演示够用）。
// 相位脚本 = baseline 调参后的 heavy 排程（悬停 90 → 下探 80【沉降末
// 重标定】→ 沉降 15【钉目标】→ 闭爪 80 → 三级抬升 40/40/70 → 携带 95 →
// 投放 70 → 开爪 35 → 退 50），标定/钉住/限幅逻辑与 Rust policy() 同形。

// spec 的夹爪几何 → 相位高度（与 baseline 的 PhaseGeom 同口径，mm 取整）。
function phaseGeom(spec) {
  const g = spec.robot.gripper;
  if (!g) return null;
  const a = g.approach_axis || [1, 0, 0];
  const n = Math.hypot(a[0], a[1], a[2]) || 1;
  const u = [a[0] / n, a[1] / n, a[2] / n];
  const tcpAlong = (g.tcp_offset[0] * u[0] + g.tcp_offset[1] * u[1] + g.tcp_offset[2] * u[2]) / n;
  const idx =
    Math.abs(u[0]) >= Math.abs(u[1]) && Math.abs(u[0]) >= Math.abs(u[2]) ? 0
    : Math.abs(u[1]) >= Math.abs(u[2]) ? 1 : 2;
  const mm = (v) => Math.round(v * 1000) / 1000;
  const plateDrop = mm(g.finger_x - tcpAlong);
  const plateHalf = mm(g.finger_half[idx]);
  const heavy = spec.robot.joints.length >= 6;
  const grasp = mm(plateDrop + 0.015 - plateHalf);
  return { heavy, grasp, place: mm(grasp + 0.03) };
}

// heavy 排程（Panda 演示；与 embodied_sim_baseline pick_phases(true) 同表）。
function heavyPhases(geom) {
  const { grasp, place } = geom;
  return [
    { grip: 0, ticks: 90, target: (o) => above(o, 0.12) },
    { grip: 0, ticks: 80, target: (o) => above(o, grasp) },
    { grip: 0, ticks: 15, pin: true, target: (o) => above(o, grasp) },
    { grip: 1, ticks: 80, pin: true, target: (o) => above(o, grasp) },
    { grip: 1, ticks: 40, pin: true, target: (o) => above(o, 0.05) },
    { grip: 1, ticks: 40, pin: true, target: (o) => above(o, 0.11) },
    { grip: 1, ticks: 70, pin: true, target: (o) => above(o, 0.2) },
    { grip: 1, ticks: 95, target: (_o, g) => above(g, 0.2) },
    { grip: 1, ticks: 70, target: (_o, g) => above(g, place) },
    { grip: 0, ticks: 35, target: (_o, g) => above(g, place) },
    { grip: 0, ticks: 50, target: (_o, g) => above(g, 0.22) },
  ];
}

// FK（转动 + prismatic）：返回 { tip, approach, gap }（世界系，单位向量）。
function fkPose(spec, q) {
  const links = spec.robot.links, joints = spec.robot.joints;
  const p = links.map(() => [0, 0, 0]);
  const r = links.map(() => [[1, 0, 0], [0, 1, 0], [0, 0, 1]]);
  p[0] = links[0].origin.slice();
  const rotAxis = (u, ang) => {
    const c = Math.cos(ang), s = Math.sin(ang), t = 1 - c;
    return [
      [c + t * u[0] * u[0], t * u[0] * u[1] - s * u[2], t * u[0] * u[2] + s * u[1]],
      [t * u[0] * u[1] + s * u[2], c + t * u[1] * u[1], t * u[1] * u[2] - s * u[0]],
      [t * u[0] * u[2] - s * u[1], t * u[1] * u[2] + s * u[0], c + t * u[2] * u[2]],
    ];
  };
  const mul = (a, b) =>
    a.map((row) => b[0].map((_, j) => row.reduce((acc, x, k) => acc + x * b[k][j], 0)));
  const mulVec = (a, v) => a.map((row) => row.reduce((acc, x, k) => acc + x * v[k], 0));
  joints.forEach((j, i) => {
    const ang = q[i] || 0;
    const axisLen = Math.hypot(...j.axis) || 1;
    const u = j.axis.map((x) => x / axisLen);
    const par = j.parent_link, ch = j.child_link;
    p[ch] = mulVec(r[par], j.anchor).map((x, k) => p[par][k] + x);
    if (j.kind === "Prismatic") {
      const side = mulVec(r[par], u).map((x) => x * ang);
      p[ch] = p[ch].map((x, k) => x + side[k]);
      r[ch] = r[par].map((row) => row.slice());
    } else {
      r[ch] = mul(r[par], rotAxis(u, ang));
    }
  });
  const g = spec.robot.gripper;
  const eeLink = g ? g.attach_link : joints[joints.length - 1].child_link;
  const unit = (v) => {
    const n = Math.hypot(v[0], v[1], v[2]) || 1;
    return [v[0] / n, v[1] / n, v[2] / n];
  };
  const tcp = g ? g.tcp_offset : [0, 0, 0];
  const approach = unit(g ? g.approach_axis || [1, 0, 0] : [1, 0, 0]);
  const gap = unit(g ? g.gap_axis || [0, 0, 1] : [0, 0, 1]);
  return {
    tip: mulVec(r[eeLink], tcp).map((x, k) => p[eeLink][k] + x),
    approach: mulVec(r[eeLink], approach),
    gap: mulVec(r[eeLink], gap),
  };
}

// 简化 DLS：9 行残差（位置 + approach + gap 免符号）× dof 列数值雅可比，
// Δq = Jᵀ(JJᵀ + λ²I)⁻¹r（高斯消元带部分主元），步幅钳 + 关节限位投影。
// `st.q` 热启动（进出）；返回 |target − tip|（诊断，同 Rust last_pos_error）。
function dlsSolve(spec, st, target, down, gapAxis) {
  const j = spec.robot.joints;
  const dof = j.length;
  st.q = Array.from(st.q ?? []); // 热启动载体可能是 Float32Array（obs 切片）
  while (st.q.length < dof) st.q.push(0);
  const eps = 1e-4, lambda = 0.05, maxIter = 80, tol = 1e-4, w = 0.5;
  const residual = (q) => {
    const f = fkPose(spec, q);
    const r = [target[0] - f.tip[0], target[1] - f.tip[1], target[2] - f.tip[2]];
    for (let k = 0; k < 3; k++) r.push(w * (down[k] - f.approach[k]));
    const dot = gapAxis[0] * f.gap[0] + gapAxis[1] * f.gap[1] + gapAxis[2] * f.gap[2];
    const sg = dot < 0 ? -1 : 1;
    for (let k = 0; k < 3; k++) r.push(w * (sg * gapAxis[k] - f.gap[k]));
    return r;
  };
  const solve9 = (m, rhs) => {
    // 部分主元高斯消元（n×n 原地，返回解；奇异返回 null）
    const n = rhs.length;
    const a = m.map((row, i) => [...row, rhs[i]]);
    for (let col = 0; col < n; col++) {
      let piv = col;
      for (let r2 = col + 1; r2 < n; r2++) if (Math.abs(a[r2][col]) > Math.abs(a[piv][col])) piv = r2;
      if (Math.abs(a[piv][col]) < 1e-12) return null;
      [a[col], a[piv]] = [a[piv], a[col]];
      for (let r2 = 0; r2 < n; r2++) {
        if (r2 === col) continue;
        const f = a[r2][col] / a[col][col];
        for (let k = col; k <= n; k++) a[r2][k] -= f * a[col][k];
      }
    }
    return a.map((row, i) => row[n] / row[i]); // 对角元 = row[i]（行 i 列 i）
  };
  let posErr = Infinity;
  for (let iter = 0; iter < maxIter; iter++) {
    const r = residual(st.q);
    posErr = Math.hypot(r[0], r[1], r[2]);
    const rotOk = r.slice(3).every((v) => Math.abs(v) < 2e-3 * w);
    if (posErr < tol && rotOk) break;
    const n = r.length;
    const jac = [];
    for (let c = 0; c < dof; c++) {
      const qp = st.q.slice();
      qp[c] += eps;
      const rp = residual(qp);
      jac.push(rp.map((v, k) => (v - r[k]) / eps)); // 列 = ∂r/∂q_c
    }
    const jjt = [];
    for (let i2 = 0; i2 < n; i2++) {
      jjt.push([]);
      for (let k = 0; k < n; k++) {
        let acc = 0;
        for (let c = 0; c < dof; c++) acc += jac[c][i2] * jac[c][k];
        jjt[i2].push(acc + (i2 === k ? lambda * lambda : 0));
      }
    }
    const dx = solve9(jjt, r);
    if (!dx) break;
    // Δq_c = Σ_i jac[c][i]·dx_i 是**下降**方向的负号项（残差雅可比）
    let worst = 0;
    for (let c = 0; c < dof; c++) {
      let dq = 0;
      for (let i2 = 0; i2 < n; i2++) dq += jac[c][i2] * dx[i2];
      const step = -dq;
      worst = Math.max(worst, Math.abs(step));
      st.q[c] = Math.min(Math.max(st.q[c] + step, j[c].limits[0]), j[c].limits[1]);
    }
    if (worst < 1e-6) break;
  }
  return posErr;
}

// ── 内核状态 ─
const K = {
  sim: null, spec: null, layout: null, dof: 0, arm: null,
  visuals: {}, links: [], targets: [], gripper: 0, lastEE: [0, 0, 0],
  holding: false, _lastHolding: false, goal: [0.28, 0.5, 0],
  tick: 0, steps: 0, obs: null,
  recording: false, episode: null, episodes: [],
  policyMode: false, policyStep: 0, policySource: "js",
  naxis: null, phaseGeom: null,
  iEe: 9, iObj: 12, iGoal: 15,
  redraws: 0, keys: new Set(), hudCb: null, logCb: null, recCb: null,
};

function applySpec(specText) {
  const sim = OrieraSim.from_spec_json(specText); // 校验失败=可读 Error
  K.sim = sim;
  K.spec = JSON.parse(sim.spec_json());
  K.layout = JSON.parse(sim.layout());
  K.dof = K.spec.robot.joints.length;
  K.arm = ikArmFromSpec(K.spec);
  K.phaseGeom = phaseGeom(K.spec);
  const names = K.layout.obs;
  K.iEe = names.indexOf("ee.x");
  K.iObj = names.indexOf("obj_rel.x");
  K.iGoal = names.indexOf("goal_rel.x");
}

function envReset(seed) {
  K.obs = K.sim.reset(seed);
  K.goal = Array.from(K.sim.goal());
  K.tick = K.sim.tick();
  K.steps = 0;
  K.policyMode = false; K.policyStep = 0;
  K.episode = null;
  syncVisuals();
  return K.obs;
}

// ── 三维表现同步：渲染器只搬 mesh，不推演任何物理 ─
function syncVisuals() {
  const p = JSON.parse(K.sim.poses());
  K.tick = p.tick;
  K.steps = p.steps;
  K.holding = p.holding;
  K.lastEE = p.ee.pos.slice();
  K.links.forEach((g, i) => {
    g.position.fromArray(p.links[i].pos);
    g.quaternion.fromArray(p.links[i].rot);
  });
  if (K.visuals.plateL && p.fingers) {
    K.visuals.plateL.position.fromArray(p.fingers.left.pos);
    K.visuals.plateL.quaternion.fromArray(p.fingers.left.rot);
    K.visuals.plateR.position.fromArray(p.fingers.right.pos);
    K.visuals.plateR.quaternion.fromArray(p.fingers.right.rot);
  }
  // 全量 props（M2-WPC 集成：rot 自 WPA 起公开，rot_known=true）
  (K.visuals.props || []).forEach((mesh, i) => {
    const pp = p.props[i];
    if (!pp) return;
    mesh.position.fromArray(pp.pos);
    if (pp.rot_known) mesh.quaternion.fromArray(pp.rot);
  });
  if (K.visuals.cube && p.props.length) {
    K.visuals.cube.position.fromArray(p.props[0].pos);
    if (p.props[0].rot_known) K.visuals.cube.quaternion.fromArray(p.props[0].rot);
    else K.visuals.cube.quaternion.set(0, 0, 0, 1);
  }
  if (K.visuals.goal) K.visuals.goal.position.set(p.goal[0], 0.501, p.goal[2]);
  if (K.visuals.beam) K.visuals.beam.position.set(p.goal[0], 0.66, p.goal[2]);
  K.goal = p.goal.slice();
  return p;
}

// ── 从 spec 搭建 three.js 场景（fixtures / 机器人 / 夹爪 / 方块 / 目标圈）
// ——形状全部来自 spec（改 JSON 换方块形状，浏览器页直接可见）。
function buildSceneFromSpec(scene) {
  // 旧节点退役
  for (const key of ["world", "robot", "goal", "beam"]) {
    const old = K.visuals[key];
    if (old) {
      old.traverse?.((o) => { o.geometry?.dispose?.(); });
      scene.remove(old);
      delete K.visuals[key];
    }
  }
  K.links = [];
  K.visuals.props = []; // 换装重建：prop mesh 数组随 robot 组一并退役

  // fixtures：ground（地板贴图）/ 其他（桌面/箱子——桌面走木纹理）
  const mat = (c, r = 0.8, m = 0.0) => {
    const mt = new THREE.MeshStandardMaterial({ color: c, roughness: r, metalness: m });
    mt.envMapIntensity = 1.1;
    return mt;
  };
  const texLoader = new THREE.TextureLoader();
  const world = new THREE.Group();
  K.visuals.world = world;
  for (const fx of K.spec.scene.fixtures) {
    const geo = new THREE.BoxGeometry(fx.half[0] * 2, fx.half[1] * 2, fx.half[2] * 2);
    let m;
    if (fx.name === "ground") {
      const floorD = texLoader.load("vendor/assets/floor_diff.jpg?v=3");
      floorD.wrapS = floorD.wrapT = THREE.RepeatWrapping;
      floorD.repeat.set(4, 3);
      floorD.colorSpace = THREE.SRGBColorSpace;
      m = new THREE.MeshStandardMaterial({ map: floorD, roughness: 0.92, envMapIntensity: 1.1 });
    } else {
      const woodD = texLoader.load("vendor/assets/wood_orig_diff.jpg?v=6");
      const woodR = texLoader.load("vendor/assets/wood_rough.jpg?v=2");
      for (const t of [woodD, woodR]) { t.wrapS = t.wrapT = THREE.RepeatWrapping; t.repeat.set(1.4, 1.4); t.colorSpace = THREE.SRGBColorSpace; }
      m = new THREE.MeshStandardMaterial({ map: woodD, roughnessMap: woodR, roughness: 0.7, metalness: 0.02, color: 0xd8c8b0, envMapIntensity: 1.1 });
    }
    const mesh = new THREE.Mesh(geo, m);
    mesh.position.set(...fx.center);
    mesh.castShadow = mesh.receiveShadow = true;
    world.add(mesh);
  }
  scene.add(world);

  // 目标圈 + 光柱（半径=goal_xz_tol，来自 spec.task.params）
  const zoneR = K.spec.task.params.goal_xz_tol;
  const goal = new THREE.Mesh(
    new THREE.RingGeometry(zoneR * 0.75, zoneR, 48),
    new THREE.MeshBasicMaterial({ color: 0x7be0a0, side: THREE.DoubleSide, transparent: true, opacity: 0.85 }));
  goal.rotation.x = -Math.PI / 2;
  goal.position.set(0.28, 0.501, 0);
  scene.add(goal);
  const beam = new THREE.Mesh(
    new THREE.CylinderGeometry(zoneR, zoneR, 0.32, 48, 1, true),
    new THREE.MeshBasicMaterial({ color: 0x7be0a0, transparent: true, opacity: 0.055, side: THREE.DoubleSide, depthWrite: false }));
  beam.position.set(0.28, 0.66, 0);
  scene.add(beam);
  K.visuals.goal = goal;
  K.visuals.beam = beam;

  // 机器人：每个 link 一个 Group（位姿由 poses() 每拍写回），
  // 碰撞体形状映射为 mesh（Box→盒；细长盒→胶囊观感；Capsule→胶囊）。
  const armMat = mat(0x5b8def);
  const accent = mat(0x8fa8d8, 0.35, 0.6);
  const robot = new THREE.Group();
  K.visuals.robot = robot;
  const mkShapeMesh = (link) => {
    const s = link.shape;
    let half, mesh;
    if (s && s.Box) {
      half = s.Box.half;
      if (half[0] > half[1] * 2 && half[0] > 0.03) {
        // 细长连杆 → 胶囊观感（对齐旧 look；碰撞仍是盒，渲染层不装懂物理）
        const geo = new THREE.CapsuleGeometry(half[1], Math.max(half[0] * 2 - half[1] * 2, 0.02), 6, 20).rotateZ(-Math.PI / 2);
        mesh = new THREE.Mesh(geo, link.density >= 1500 ? armMat : accent);
      } else {
        mesh = new THREE.Mesh(new THREE.BoxGeometry(half[0] * 2, half[1] * 2, half[2] * 2),
          link.density >= 1500 ? armMat : mat(0x39415a));
      }
    } else if (s && s.Capsule) {
      const geo = new THREE.CapsuleGeometry(s.Capsule.radius, Math.max(s.Capsule.half_len * 2 - s.Capsule.radius * 2, 0.02), 6, 20).rotateZ(-Math.PI / 2);
      mesh = new THREE.Mesh(geo, armMat);
    } else {
      mesh = new THREE.Mesh(new THREE.BoxGeometry(0.03, 0.03, 0.03), accent);
    }
    mesh.position.set(...link.collider_offset);
    return mesh;
  };
  for (const link of K.spec.robot.links) {
    const g = new THREE.Group();
    g.add(mkShapeMesh(link));
    K.links.push(g);
    robot.add(g);
  }
  // 关节轮毂盘（k≥1 的关节，轴随关节轴；渲染信息层）
  const Y = new THREE.Vector3(0, 1, 0);
  K.spec.robot.joints.forEach((j, k) => {
    if (k === 0) return;
    const r = Math.max(0.02, Math.min(0.035, j.anchor[0] * 0.16 + 0.02));
    const geo = new THREE.CylinderGeometry(r, r, 0.012, 24);
    const mesh = new THREE.Mesh(geo, mat(0x8fa8d8, 0.35, 0.7));
    mesh.quaternion.setFromUnitVectors(Y, new THREE.Vector3(...j.axis));
    K.links[j.child_link]?.add(mesh);
  });
  // 夹爪指板
  if (K.spec.robot.gripper) {
    const gh = K.spec.robot.gripper.finger_half;
    const plateGeo = new THREE.BoxGeometry(gh[0] * 2, gh[1] * 2, gh[2] * 2);
    const plateMat = new THREE.MeshStandardMaterial({ color: 0x9fb6e8, roughness: 0.35, metalness: 0.7, transparent: true, opacity: 0.92 });
    K.visuals.plateL = new THREE.Mesh(plateGeo, plateMat);
    K.visuals.plateR = new THREE.Mesh(plateGeo, plateMat);
    robot.add(K.visuals.plateL, K.visuals.plateR);
  } else {
    delete K.visuals.plateL;
    delete K.visuals.plateR;
  }
  // 动态 prop（props[0] = 任务物；其余杂物同为渲染层）
  for (const prop of K.spec.scene.props) {
    let geo;
    const s = prop.shape;
    if (s && s.Cylinder) geo = new THREE.CylinderGeometry(s.Cylinder.radius, s.Cylinder.radius, s.Cylinder.half_h * 2, 32);
    else if (s && s.Sphere) geo = new THREE.SphereGeometry(s.Sphere.r, 24, 16);
    else { const h = (s && s.Cuboid) ? s.Cuboid.half : [0.025, 0.025, 0.025]; geo = new THREE.BoxGeometry(h[0] * 2, h[1] * 2, h[2] * 2); }
    const mesh = new THREE.Mesh(geo, mat(0xffd479));
    mesh.castShadow = mesh.receiveShadow = true;
    if (prop === K.spec.scene.props[0]) K.visuals.cube = mesh;
    (K.visuals.props ||= []).push(mesh); // 与 spec.scene.props 同序（全量渲染）
    robot.add(mesh);
  }
  robot.traverse((o) => { if (o.isMesh) { o.castShadow = true; o.receiveShadow = true; } });
  scene.add(robot);
}

export async function boot(viewport) {
  const stage = (t) => { const el = document.getElementById("load"); if (el) el.firstElementChild.textContent = "启动: " + t; };
  stage("renderer");
  const renderer = new THREE.WebGLRenderer({ antialias: true });
  renderer.setSize(viewport.clientWidth, viewport.clientHeight);
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.25;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  viewport.appendChild(renderer.domElement);
  // PiP 顶视相机（对齐 Rust camera.rs CameraView：正交俯视桌面工作区）
  const pipCam = new THREE.OrthographicCamera(
    -0.46, 0.46, 0.46, -0.46, 0.01, 5);
  pipCam.position.set(0.32, 3.0, 0);
  pipCam.up.set(0, 0, -1); // +x→右, +z→下（与 camera.rs 的 x→右/z→下映射一致）
  pipCam.lookAt(0.32, 0, 0);
  const pipSize = 160;

  // REC 角标（录制时可见）
  const recBadge = document.createElement("div");
  recBadge.style.cssText = "position:absolute;top:10px;left:10px;z-index:4;display:none;align-items:center;gap:6px;background:rgba(0,0,0,0.6);padding:4px 10px;border-radius:4px;font:12px ui-monospace;color:#ff6b81;";
  recBadge.innerHTML = '<span style="width:8px;height:8px;border-radius:50%;background:#ff4444;display:inline-block;animation:blink 1s infinite;"></span> REC';
  viewport.style.position = "relative";
  viewport.appendChild(recBadge);
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x0a0d14);
  scene.fog = new THREE.Fog(0x0a0d14, 3.5, 8);
  // 环境光：HDRI（Poly Haven CC0）经 PMREM；加载失败回退 RoomEnvironment
  //（离线兜底）。URL 相对**文档**（index.html）解析——曾因相对本文件
  // 解析逃到 examples/vendor/ 404、材质全黑（纹理 404 坑 2026-11-01）。
  const pmrem = new THREE.PMREMGenerator(renderer);
  new RGBELoader().load(
    "vendor/assets/env_1k.hdr?v=3",
    (hdr) => {
      const tex = pmrem.fromEquirectangular(hdr).texture;
      scene.environment = tex;
      hdr.dispose();
    },
    undefined,
    () => { scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture; },
  );
  const camera = new THREE.PerspectiveCamera(50, viewport.clientWidth / viewport.clientHeight, 0.05, 50);
  const cam = { theta: 0.9, phi: 1.05, r: 1.9, target: new THREE.Vector3(0.35, 0.45, 0) };
  function placeCam() {
    camera.position.set(
      cam.target.x + cam.r * Math.sin(cam.phi) * Math.cos(cam.theta),
      cam.target.y + cam.r * Math.cos(cam.phi),
      cam.target.z + cam.r * Math.sin(cam.phi) * Math.sin(cam.theta));
    camera.lookAt(cam.target);
  }
  placeCam();
  // 后处理：轻量 bloom + 输出色调
  const rt = new THREE.WebGLRenderTarget(viewport.clientWidth, viewport.clientHeight, { samples: 4, type: THREE.HalfFloatType });
  const composer = new EffectComposer(renderer, rt);
  composer.addPass(new RenderPass(scene, camera));
  composer.addPass(new UnrealBloomPass(new THREE.Vector2(viewport.clientWidth, viewport.clientHeight), 0.32, 0.6, 0.92));
  composer.addPass(new OutputPass());
  K.visuals.composer = composer;

  // 灯光：主光带软阴影 + 半球底光 + 冷色 rim
  const dl = new THREE.DirectionalLight(0xfff2e0, 3.2);
  dl.position.set(1.4, 2.6, 0.9);
  dl.castShadow = true;
  dl.shadow.mapSize.set(2048, 2048);
  dl.shadow.camera.left = -1.2; dl.shadow.camera.right = 1.2;
  dl.shadow.camera.top = 1.2; dl.shadow.camera.bottom = -1.2;
  dl.shadow.camera.far = 8;
  dl.shadow.bias = -0.0006;
  dl.shadow.radius = 4;
  scene.add(dl);
  scene.add(new THREE.HemisphereLight(0x8899cc, 0x20242e, 0.9));
  const rim = new THREE.DirectionalLight(0x88bbff, 1.2);
  rim.position.set(-1.2, 1.8, -1.4);
  scene.add(rim);
  // 地面网格（MuJoCo 式空间锚点）
  const grid = new THREE.GridHelper(2.5, 25, 0x1a2030, 0x1a2030);
  grid.position.set(0, 0.005, 0);
  scene.add(grid);

  // 末端轨迹拖尾（信息层：最近 120 个 EE 位置）
  const TRAIL_N = 120;
  const trailPos = new Float32Array(TRAIL_N * 3);
  const trailGeo = new THREE.BufferGeometry();
  trailGeo.setAttribute("position", new THREE.BufferAttribute(trailPos, 3));
  const trail = new THREE.Line(trailGeo, new THREE.LineBasicMaterial({ color: 0x5b8def, transparent: true, opacity: 0.45 }));
  trail.frustumCulled = false;
  scene.add(trail);

  // ── wasm 物理内核（与 CLI/Python 同一份 TableTopEnv）──
  stage("wasm init");
  await init(new URL("../vendor/oriera/oriera_wasm_bg.wasm", import.meta.url));
  stage("spec");
  const specUrl = new URLSearchParams(location.search).get("spec");
  const specText = specUrl ? await (await fetch(specUrl)).text() : OrieraSim.preset_spec_json();
  try { applySpec(specText); } catch (e) { stage("ERR: " + (e && e.message ? e.message : e)); throw e; }
  K.policySource = new URLSearchParams(location.search).get("policy") === "wasm" ? "wasm" : "js";
  buildSceneFromSpec(scene);
  stage("world");
  try { envReset(1000); } catch (e) { stage("ERR: " + (e && e.message ? e.message : e)); throw e; }
  document.getElementById("load").remove();

  // 相机拖拽
  let drag = null;
  renderer.domElement.addEventListener("pointerdown", (e) => (drag = { x: e.clientX, y: e.clientY }));
  addEventListener("pointerup", () => (drag = null));
  addEventListener("pointermove", (e) => {
    if (!drag) return;
    cam.theta += (e.clientX - drag.x) * 0.008;
    cam.phi = Math.min(Math.max(cam.phi - (e.clientY - drag.y) * 0.008, 0.25), 1.45);
    drag = { x: e.clientX, y: e.clientY };
    placeCam();
  });
  renderer.domElement.addEventListener("wheel", (e) => {
    cam.r = Math.min(Math.max(cam.r + e.deltaY * 0.0015, 0.7), 5);
    placeCam();
  }, { passive: true });

  // 遥操作键盘（Q/A W/S E/D R/F=关节；空格=夹爪）
  const keyMap = {
    q: [0, +0.03], a: [0, -0.03], w: [1, +0.03], s: [1, -0.03],
    e: [2, +0.03], d: [2, -0.03], r: [3, +0.03], f: [3, -0.03],
  };
  addEventListener("keydown", (e) => {
    if (e.key === " ") { K.gripper = K.gripper > 0.5 ? 0 : 1; e.preventDefault(); return; }
    const m = keyMap[e.key.toLowerCase()];
    if (m) { K.keys.add(e.key.toLowerCase()); e.preventDefault(); }
  });
  addEventListener("keyup", (e) => K.keys.delete(e.key.toLowerCase()));

  // UI
  const hud = document.getElementById("hud");
  const log = document.getElementById("log");
  K.hudCb = (t) => (hud.innerHTML = t);
  K.logCb = (t) => (log.textContent = t);
  const jointsDiv = document.getElementById("joints");
  // 滑条从 spec 的关节表生成（名字/限位随 spec；换 spec 可重建）
  const sliders = [];
  function buildSliders() {
    jointsDiv.replaceChildren();
    sliders.length = 0;
    K.targets = K.spec.robot.joints.map(() => 0);
    K.spec.robot.joints.forEach((j, i) => {
      const row = document.createElement("div");
      row.className = "row";
      row.innerHTML = `<label>${j.name}</label><input type="range" min="${j.limits[0]}" max="${j.limits[1]}" step="0.01" value="0"><span class="val">0.00</span>`;
      jointsDiv.appendChild(row);
      const input = row.querySelector("input");
      input.addEventListener("input", () => {
        K.targets[i] = parseFloat(input.value);
        row.querySelector(".val").textContent = K.targets[i].toFixed(2);
      });
      sliders.push({ input, val: row.querySelector(".val") });
    });
  }
  buildSliders();
  const gripInput = document.getElementById("j-grip");
  gripInput.addEventListener("input", () => {
    K.gripper = parseFloat(gripInput.value);
    document.getElementById("v-grip").textContent = K.gripper > 0.5 ? "合" : "开";
  });
  const btn = (id, fn) => document.getElementById(id).addEventListener("click", fn);
  btn("btn-reset", () => { envReset((Math.random() * 1e9) | 0); log.textContent = "已重置（随机种子）"; post({ type: "state", note: "reset" }); });
  // 策略守卫：4 轴走解析 ik（金标路）；其余全转动关节臂（Panda 7 轴等）
  // 走 N 轴 DLS 数值 IK（P1-B2 移植 ik.rs 的简化版）；含 prismatic 关节
  // 的预设（mobile-arm 龙门底盘）置灰——滑轨底盘的「移动后才能抓」需要
  // 两段式规划，数值 IK 单目标 servo 拖不动（Rust 基线同样不给它 DLS，
  // 见 POSITIONING §2 的诚实边界），演示请换预设或用 CLI。
  const hasPrismatic = () => K.spec.robot.joints.some((j) => j.kind === "Prismatic");
  const policyBtn = document.getElementById("btn-policy");
  const refreshPolicyBtn = () => {
    const blocked = hasPrismatic();
    policyBtn.setAttribute("aria-disabled", blocked ? "true" : "false");
    policyBtn.title = blocked
      ? "本预设含 prismatic 关节（移动底盘），脚本策略不支持——请在卡片墙换桌面/Panda 预设"
      : "";
  };
  refreshPolicyBtn();
  const canRunJsPolicy = () => {
    if (hasPrismatic()) {
      log.textContent = "脚本策略不支持 prismatic 预设（mobile-arm）：请在卡片墙换桌面 4 轴或 Panda 7 轴预设；采集/评测用 CLI：cargo run -p oriera-agent --example embodied_sim_baseline --features rigid3d,spec,urdf -- --spec specs/official/<预设>.json";
      return false;
    }
    return true;
  };
  const startPolicy = () => {
    K.policyMode = true; K.policyStep = 0;
    if (K.dof !== 4) {
      // N 轴 DLS 演示：heavy 排程 + 标定/钉住/限幅状态（与 baseline 调参
      // 同形；q 热启动从家位开始，一集一清）。
      K.naxis = {
        phases: heavyPhases(K.phaseGeom),
        sag: [0, 0, 0], pin: null, prevCmd: null, q: K.spec.robot.home.slice(),
        lastErr: 0,
      };
    } else K.naxis = null;
  };
  btn("btn-policy", () => { if (!canRunJsPolicy()) return; startPolicy(); log.textContent = "脚本策略运行中…"; });
  btn("btn-rec", (e) => {
    K.recording = !K.recording;
    e.target.textContent = K.recording ? "■ 停止录制" : "● 开始录制";
    e.target.classList.toggle("rec", K.recording);
    recBadge.style.display = K.recording ? "flex" : "none";
    if (K.recording) {
      K.episode = { task: K.sim.task_phrase(), rows: [], obs: Array.from(K.sim.obs()) };
      K.recCb?.(true);
      log.textContent = "录制中：动关节 / 跑策略，每 tick 记一行 (state, action)。";
    } else if (K.episode) {
      if (K.episode.rows.length) K.episodes.push(K.episode);
      K.episode = null;
      post({ type: "progress", rows: 0, episodes: K.episodes.length });
      log.textContent = `已停。共 ${K.episodes.length} 集 · ${K.episodes.reduce((a, x) => a + x.rows.length, 0)} 行`;
    }
  });
  // G-8：独立打开（未嵌 Studio）时导出按钮置灰 + 悬停/日志区给 CLI 指引。
  // 旧行为是下载演示内核的 JSON，学生会误当作业数据；作业证据一律走
  // CLI（引擎基线口径，见 G-5 HUD 声明）。刻意不用 disabled 属性：
  // 点击仍要在日志区回应（「点了没反应」正是 G-8 原诉）。Studio 内嵌
  // （window.parent !== window）行为不变——Sim.vue 发 cmd:"export" 照常落盘。
  const EMBEDDED = window.parent !== window;
  const EXPORT_CLI_HINT =
    "仅 Studio 内可用；独立采集用 CLI：cargo run -p oriera-agent --example embodied_sim_collect" +
    " --features rigid3d,cam -- --task pick_place --episodes 12 --out out/my-run";
  if (!EMBEDDED) {
    const exportBtn = document.getElementById("btn-export");
    exportBtn.setAttribute("aria-disabled", "true");
    exportBtn.title = EXPORT_CLI_HINT;
    log.textContent = `（尚未录制）\n提示：「导出」${EXPORT_CLI_HINT}（步骤见 docs/LAB-GUIDE.md 第 1 节）`;
  }
  btn("btn-export", () => {
    if (!EMBEDDED) { log.textContent = `「导出」${EXPORT_CLI_HINT}\n（浏览器页只用来看手感；交作业的数据与成功率以 CLI 的 eval_report.json 为准）`; return; }
    if (K.episode?.rows?.length) { K.episodes.push(K.episode); K.episode = null; }
    if (!K.episodes.length) { log.textContent = "没有可导出的 episode——先录一段。"; return; }
    post({ type: "episodes", episodes: K.episodes, obsNames: K.layout.obs, actionNames: K.layout.action, task: K.sim.task_phrase(), fps: Math.round(1 / K.sim.dt()) }); K.episodes = []; log.textContent = "已交给 Studio 工作台。";
  });

  function post(msg) {
    try {
      // targetOrigin 收紧：仅父窗口同源（独立打开时为自身 origin）。
      const parentOrigin = window.parent !== window ? new URL(document.referrer || location.href).origin : location.origin;
      window.parent.postMessage({ source: "oriera-sim", ...msg }, parentOrigin);
    } catch {}
  }
  addEventListener("message", (e) => {
    // 信任边界（安全深检）：仅接受父窗口/自身的控制消息。
    if (e.source !== window.parent && e.source !== window) return;
    const m = e.data;
    if (!m || m.source !== "oriera-sim" || !m.cmd) return;
    if (m.cmd === "reset") envReset(m.seed ?? ((Math.random() * 1e9) | 0));
    else if (m.cmd === "policy") { if (!canRunJsPolicy()) return; startPolicy(); if (m.source === "wasm") K.policySource = "wasm"; }
    else if (m.cmd === "grip") K.gripper = m.value;
    else if (m.cmd === "teleop") { K.policyMode = false; for (let i = 0; i < K.dof; i++) K.targets[i] = m.angles[i]; K.gripper = m.gripper ?? K.gripper; }
    else if (m.cmd === "record") document.getElementById("btn-rec").click();
    else if (m.cmd === "export") document.getElementById("btn-export").click();
    else if (m.cmd === "spec" && m.json) {
      // Studio 三库（M2）入口：整份 spec 换装（场景+机器人随建随换）。
      try {
        applySpec(m.json);
        buildSceneFromSpec(scene);
        buildSliders();
        refreshPolicyBtn();
        envReset(m.seed ?? 1000);
        post({ type: "state", note: "spec-applied", specHash: K.sim.spec_hash() });
      } catch (err) {
        log.textContent = "spec 换装失败：" + (err?.message ?? err);
        post({ type: "state", note: "spec-apply-failed", error: String(err?.message ?? err) });
      }
    }
  });

  let rewardAvg = 0, successCount = 0, episodesRun = 0;
  function loop() {
    let action = null;
    for (const k of K.keys) {
      const m = keyMap[k];
      if (m) { K.targets[m[0]] = Math.min(Math.max(K.targets[m[0]] + m[1], K.spec.robot.joints[m[0]].limits[0]), K.spec.robot.joints[m[0]].limits[1]); }
    }
    // 遥操作闭环：键盘改 K.targets 后回写滑条（UX review：按 Q/A 臂在
    // 动滑条纹丝不动）
    if (!K.policyMode) {
      for (let i = 0; i < K.dof; i++) {
        sliders[i].input.value = K.targets[i];
        sliders[i].val.textContent = K.targets[i].toFixed(2);
      }
    }
    if (K.policyMode) {
      if (K.policySource === "wasm") {
        // 同代码逐位口径（12 集验收对 91.7% 用这条）：
        action = Array.from(K.sim.baseline_action(K.policyStep));
        K.policyStep++;
      } else if (K.naxis) {
        // N 轴 DLS 演示策略（P1-B2）：heavy 排程 + 悬停沉降标定/沉降末
        // 重标定/闭爪目标钉住/关节限幅，与 embodied_sim_baseline 的
        // policy() 同形（f64 演示口径，不逐位对标 Rust）。
        const na = K.naxis;
        const obs = K.sim.obs();
        const ee = [obs[K.iEe], obs[K.iEe + 1], obs[K.iEe + 2]];
        const obj = [ee[0] + obs[K.iObj], ee[1] + obs[K.iObj + 1], ee[2] + obs[K.iObj + 2]];
        const gol = [ee[0] + obs[K.iGoal], ee[1] + obs[K.iGoal + 1], ee[2] + obs[K.iGoal + 2]];
        const ph = policyTick(na.phases, K.policyStep);
        const phase = ph >= 0 ? na.phases[ph] : null;
        let raw = phase ? phase.target(obj, gol) : above(gol, 0.22);
        const demand = phase ? phase.grip : 0;
        let ticksBefore = 0;
        for (let i = 0; i < (ph >= 0 ? ph : na.phases.length); i++) ticksBefore += na.phases[i].ticks;
        // 闭爪/抬升锚：本段首 tick 钉住物体（含沉降段——它先于闭爪），
        // obj 系目标整体平移到钉住的坐标系，随活物体漂移的正反馈回路断开。
        if (phase?.pin) {
          if (!na.pin) na.pin = obj.slice();
          raw = [na.pin[0] + (raw[0] - obj[0]), na.pin[1] + (raw[1] - obj[1]), na.pin[2] + (raw[2] - obj[2])];
        } else na.pin = null;
        const hoverEnd = na.phases[0].ticks - 1;
        const step = K.policyStep;
        if (step === hoverEnd) na.sag = [raw[0] - ee[0], raw[1] - ee[1], raw[2] - ee[2]];
        // 下探段末：臂已稳到抓取航点，把「指令−实测」稳态差折进补偿。
        if (phase && na.phases[1] === phase && step === ticksBefore + phase.ticks - 1) {
          const withSag = [raw[0] + na.sag[0], raw[1] + na.sag[1], raw[2] + na.sag[2]];
          na.sag = [withSag[0] - ee[0], withSag[1] - ee[1], withSag[2] - ee[2]];
        }
        const target = step > hoverEnd
          ? [raw[0] + na.sag[0], raw[1] + na.sag[1], raw[2] + na.sag[2]]
          : raw;
        // 失锁重锚：上一解离目标 >2 cm 时回到实测关节角（真实姿态总是
        // 合法近邻种子）。
        if (step > 0 && na.lastErr > 0.02) na.q = obs.slice(0, K.dof);
        na.lastErr = dlsSolve(K.spec, na, target, [0, -1, 0], [0, 0, 1]);
        const limited = [];
        for (let i = 0; i < K.dof; i++) {
          let v = na.q[i];
          if (na.prevCmd) v = na.prevCmd[i] + Math.min(Math.max(v - na.prevCmd[i], -0.028), 0.028);
          limited.push(v);
        }
        na.prevCmd = limited;
        action = [...limited, demand];
        K.policyStep++;
      } else {
        // 渲染层策略（PHASES+ik 留在 JS，M1 工作包定案）：开环固定节拍，
        // 与 Rust round-4 同形；观测由 wasm 出。
        const obs = K.sim.obs();
        const ee = [obs[K.iEe], obs[K.iEe + 1], obs[K.iEe + 2]];
        const obj = [ee[0] + obs[K.iObj], ee[1] + obs[K.iObj + 1], ee[2] + obs[K.iObj + 2]];
        const gol = [ee[0] + obs[K.iGoal], ee[1] + obs[K.iGoal + 1], ee[2] + obs[K.iGoal + 2]];
        const ph = policyTick(PHASES, K.policyStep);
        const phase = ph >= 0 ? PHASES[ph] : null;
        action = phase ? [...ik(K.arm, phase.target(obj, gol)), phase.grip] : [...ik(K.arm, above(gol, 0.22)), 0];
        K.policyStep++;
      }
      for (let i = 0; i < K.dof; i++) sliders[i].input.value = action[i];
    } else {
      action = [...K.targets, K.gripper];
    }
    const out = K.sim.step(action);
    // wasm-bindgen 返回对象的字段是跨边界 getter：**先全部读出再 free**，
    // 否则 free 后再读 = "null pointer passed to rust"（集成实测：首个
    // 回合结束即炸死渲染循环）。
    const stepObs = out.obs, stepReward = out.reward;
    const terminated = out.terminated, truncated = out.truncated;
    out.free();
    K.obs = stepObs;
    rewardAvg = rewardAvg * 0.98 + stepReward * 0.02;
    // 抓取发光脉冲（UX review：抓住瞬间给视觉反馈）
    if (K.holding !== K._lastHolding) {
      const m = K.visuals.cube.material;
      m.emissive.set(K.holding ? 0x1a4a2a : 0x000000);
      K._lastHolding = K.holding;
    }
    if (K.recording && K.episode) {
      K.episode.rows.push({ state: Array.from(stepObs), action });
      if (K.episode.rows.length % 30 === 0) {
        post({ type: "progress", rows: K.episode.rows.length, episodes: K.episodes.length + 1 });
      }
      sliders.forEach((s, i) => (s.val.textContent = (K.sim.angles()[i] ?? 0).toFixed(2)));
      K.logCb?.(`录制中：${K.episode.rows.length} 行 · r̄=${rewardAvg.toFixed(3)}${K.holding ? " · 🤏已抓住" : ""}`);
    }
    // 回合语义与 Rust env 循环对齐：终止/截断即回合结束——自动重置。
    // 录制卫生：回合结束先切录音带（当前 episode 落列表），再重置。
    if (terminated || truncated) {
      // 退化重抽（对齐 embodied_sim_collect displaced seed band）：
      // <5 步终局 = 生成即入区，不占成功率分母、不进数据集。
      if (terminated && K.steps < 5) {
        K.redraws = (K.redraws || 0) + 1;
        // envReset 会清 policyMode（一次性演示语义）——重抽不是回合
        // 结束，策略要在新抽签上继续跑，否则演示被退化命中无声掐停。
        const keepPolicy = K.policyMode;
        envReset(((Math.random() * 1e9) | 0) + 65536 * K.redraws);
        if (K.recording) K.episode = { task: K.sim.task_phrase(), rows: [], obs: Array.from(K.sim.obs()) };
        if (keepPolicy) startPolicy(); // 重置标定/钉住/限幅状态（新集新状态）
      } else {
        if (K.recording && K.episode && K.episode.rows.length >= 5) K.episodes.push(K.episode);
        episodesRun++;
        if (terminated) {
          successCount++; K.policyMode = false;
          post({ type: "state", note: "success" });
        }
        envReset((Math.random() * 1e9) | 0);
        // 下一集录制对象必须在 envReset **之后**建：envReset 置
        // K.episode=null，此前先建后重置 → 录制+自动重置下只采到第一集。
        K.episode = K.recording ? { task: K.sim.task_phrase(), rows: [], obs: Array.from(K.sim.obs()) } : null;
      }
    }
    syncVisuals();
    // 目标圈呼吸脉冲 + EE 拖尾滚动
    const pulse = 1 + 0.06 * Math.sin(performance.now() * 0.003);
    K.visuals.goal.scale.set(pulse, pulse, 1);
    {
      const attr = trail.geometry.getAttribute("position");
      const arr = attr.array;
      arr.copyWithin(0, 3);
      attr.setXYZ(TRAIL_N - 1, K.lastEE[0], K.lastEE[1], K.lastEE[2]);
      attr.needsUpdate = true;
    }
    const cp = K.visuals.cube.position;
    K.hudCb?.(
      `tick ${K.tick} · r̄ ${rewardAvg.toFixed(3)} · 距目标 ${Math.hypot(cp.x - K.goal[0], cp.z - K.goal[2]).toFixed(3)}m` +
      `<br>成功 ${successCount}/${episodesRun} · ${K.holding ? "🤏 抓住" : "✋ 松开"} · obs ${K.layout.obs_dim} 维 / act ${K.layout.action_dim} 维` +
      `<br><small style="font-size:10px;color:#6f7690">演示内核与引擎基线口径不同，作业证据一律以 CLI eval_report 为准</small>`);
    K.visuals.composer.render();
    // PiP 顶视渲染（右上角小图：与 Rust camera.rs 的俯视映射一致）
    renderer.autoClear = false;
    renderer.setScissorTest(true);
    const pw = viewport.clientWidth;
    const px = pw - pipSize - 10;
    renderer.setViewport(px, 10, pipSize, pipSize);
    renderer.setScissor(px, 10, pipSize, pipSize);
    renderer.setScissorTest(true);
    renderer.render(scene, pipCam);
    renderer.setScissorTest(false);
    renderer.setViewport(0, 0, pw, viewport.clientHeight);
    renderer.autoClear = true;
    requestAnimationFrame(loop);
  }
  loop();
  addEventListener("resize", () => {
    renderer.setSize(viewport.clientWidth, viewport.clientHeight);
    K.visuals.composer.setSize(viewport.clientWidth, viewport.clientHeight);
    camera.aspect = viewport.clientWidth / viewport.clientHeight;
    camera.updateProjectionMatrix();
  });
  post({ type: "state", note: "ready" });

  // CDP/测试钩子（Studio cdp_*.mjs 传统）：确定性地驱动与读态。
  // 数据源已改 wasm（物理退役），键名语义与退役前一致。
  window.__orieraSim = {
    state: () => {
      const p = JSON.parse(K.sim.poses());
      return {
        cube: p.props[0]?.pos ?? [0, 0, 0],
        ee: K.lastEE,
        holding: K.holding,
        gripper: K.sim.gripper(),
        tick: K.tick,
        // 关节角实况 + 目标（跟踪诊断）
        angles: p.angles,
        targets: [...K.targets],
      };
    },
    // 成功率前台验证出口：CDP 轮询读，不干预仿真。
    stats: () => ({
      success: successCount,
      episodes: episodesRun,
      rewardAvg,
      policyMode: K.policyMode,
      holding: K.holding,
      goal: [...K.sim.goal()],
      phase: policyTick(K.naxis ? K.naxis.phases : PHASES, K.policyStep),
      step: K.policyStep,
      redraws: K.redraws || 0,
      policySource: K.policySource,
      specHash: K.sim.spec_hash(),
      fingerprint: K.sim.fingerprint(),
    }),
    // 诊断出口（Studio cdp 传统）。
    debug: {
      get sim() { return K.sim; },
      get spec() { return K.spec; },
      get state() { return K; },
    },
  };
}

// ── 测试导出（node:test + wasm 烟测）──────────────────────────
export const CONSTANTS = Object.freeze({
  PHASES, MAX_STEPS_FALLBACK: 700,
});
export { ik, above, policyTick, ikArmFromSpec, fkPose, dlsSolve, phaseGeom, heavyPhases };
