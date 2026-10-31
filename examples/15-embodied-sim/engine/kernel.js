// OriEra 具身仿真台 · JS 镜像内核（example/15-embodied-sim）。
//
// 与 Rust 真源逐常量对照：crates/oriera-subsys/src/rigid3d.rs（场景/机械臂/
// 马达/抓取）× crates/oriera-agent/src/embodied_env.rs（PickPlace3D 任务/
// 观测/奖励）× examples/embodied_sim_collect.rs（IK 基线策略）。浏览器里
// 跑 rapier3d（wasm）——同一物理家族；确定性回执的真源在 Rust 侧
// （with_fixed_dt + sim_fingerprint），本内核是交互镜像，不承诺逐位一致。
//
// postMessage 桥（iframe 内嵌于 Studio Sim 工作台时）：
//   父→子 {source:"oriera-sim", cmd:"reset"|"policy"|"grip"|"record"|"export", ...}
//   子→父 {source:"oriera-sim", type:"state"|"episodes", ...}

import * as THREE from "three";
import RAPIER from "rapier";

// ── 场景与机械臂常量（对照 ArmDescription / Scene3D / PickPlace3D）──
const DT = 1 / 60;
const SUBSTEPS = 2;
const DESC = {
  base: [0.02, 0.5, 0.0],
  shoulderHeight: 0.12,
  links: [0.20, 0.18, 0.09],
  radius: 0.022,
  limits: [
    [-Math.PI, Math.PI],
    [-0.35, 1.45],
    [-2.10, 0.35],
    [-2.00, 2.00],
  ],
  kp: 60,
  kd: 4,
  maxForce: 80,
};
const JOINT_NAMES = ["base_yaw", "shoulder", "elbow", "wrist_pitch", "wrist_roll"];
const PROP_HALF = 0.025;
// 接触抓取夹板（与 Rust rigid3d.rs round-4 同参数）：
// 运动学薄板悬垂于工具中心点之外，摩擦承载，方块全程 Dynamic。
const FINGER_HALF = [0.030, 0.045, 0.006]; // 板顶恒≤尖端，不铲方块顶
const FINGER_X = 0.135; // 板心 = 腕 + 旋转*(0.135,0,0)：TCP 外 4.5cm
const FINGER_GAP_OPEN = 0.075;
const FINGER_GAP_CLOSED = 0.046;
const FINGER_FRICTION = 1.4;
const GRASP_RADIUS = 0.06;
const TABLE = { center: [0.32, 0.25, 0.0], half: [0.28, 0.25, 0.32] };
const OBJ_RANGE = { x: [0.14, 0.22], z: [-0.10, 0.10] };
const GOAL_RANGE = { x: [0.24, 0.31], z: [-0.08, 0.08] };
const ZONE_RADIUS = 0.09;
const SETTLE_TICKS = 45;
const MAX_STEPS = 700;
const TASK_PHRASE = "pick up the cube and place it at the target";
const OBS_NAMES = [
  "j0_yaw","j1_shoulder","j2_elbow","j3_wrist","qd0","qd1","qd2","qd3","gripper",
  "ee_x","ee_y","ee_z","obj_rel_x","obj_rel_y","obj_rel_z",
  "goal_rel_x","goal_rel_y","goal_rel_z","holding","obj_height","dist_ee_obj",
];
const ACTION_NAMES = ["tgt_j0_yaw", "tgt_j1_shoulder", "tgt_j2_elbow", "tgt_j3_wrist", "gripper"];

// ── 确定性 LCG（与 Rust Lcg 同常数）──
function lcgNext(state, lo, hi) {
  state.s = (BigInt.asUintN(64, state.s * 6364136223846793005n) + 1442695040888963407n) & 0xffffffffffffffffn;
  const f = Number(state.s >> 33n) / 4294967295;
  return lo + f * (hi - lo);
}

// ── IK 与阶段策略（对照 embodied_sim_collect.rs）──
function ik(target) {
  const [l1, l2] = [DESC.links[0], DESC.links[1]];
  const shoulderY = DESC.base[1] + DESC.shoulderHeight;
  const dx = target[0] - DESC.base[0];
  const dz = target[2] - DESC.base[2];
  const yaw = Math.atan2(-dz, dx);
  const d = Math.min(Math.max(Math.hypot(dx, dz), 0.06), l1 + l2 - 0.005);
  const h = Math.min(Math.max(target[1] + DESC.links[2] - shoulderY, -0.38), 0.30);
  let cosE = (d * d + h * h - l1 * l1 - l2 * l2) / (2 * l1 * l2);
  cosE = Math.min(Math.max(cosE, -1), 1);
  const elbow = -Math.acos(cosE);
  const shoulder = Math.atan2(h, d) - Math.atan2(l2 * Math.sin(elbow), l1 + l2 * Math.cos(elbow));
  const wrist = -Math.PI / 2 - shoulder - elbow;
  const cl = (v, i) => Math.min(Math.max(v, DESC.limits[i][0]), DESC.limits[i][1]);
  return [cl(yaw, 0), cl(shoulder, 1), cl(elbow, 2), cl(wrist, 3)];
}
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

// ── 内核状态 ─
const K = {
  world: null, joints: [], bodies: {}, visuals: {},
  targets: [0, 0, 0, 0], gripper: 0, lastCmd: [NaN, NaN, NaN, NaN],
  prevAngles: [0, 0, 0, 0], lastVel: [0, 0, 0, 0], lastEE: [0, 0, 0],
  holding: false,
  fingerL: null, fingerR: null, gapNow: FINGER_GAP_OPEN,
  goal: [0.28, 0.5, 0], rng: { s: 1n }, tick: 0, steps: 0, wasHolding: false,
  recording: false, episode: null, episodes: [], policyMode: false, policyStep: 0,
  keys: new Set(), hudCb: null, logCb: null, recCb: null,
};

function makeWorld(seed) {
  const world = new RAPIER.World({ x: 0, y: -9.8, z: 0 });
  world.timestep = DT / SUBSTEPS;
  const rug = (c) => c.setFriction(0.9);
  // ground + table
  const slab = (c, h) => {
    const b = world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(...c));
    world.createCollider(RAPIER.ColliderDesc.cuboid(...h).setFriction(0.9), b);
    return b;
  };
  slab([0, -0.05, 0], [2.0, 0.05, 1.5]);
  slab(TABLE.center, TABLE.half);
  // arm chain bodies（初始 home：链沿 +X 平伸在肩高）
  const sh = DESC.base[1] + DESC.shoulderHeight;
  const dyn = (t) => world.createRigidBody(
    RAPIER.RigidBodyDesc.dynamic().setCanSleep(false)
      .setAdditionalMass(0.2).setTranslation(...t));
  const base = world.createRigidBody(
    RAPIER.RigidBodyDesc.fixed().setTranslation(DESC.base[0], DESC.base[1] + DESC.shoulderHeight / 2, DESC.base[2]));
  world.createCollider(
    RAPIER.ColliderDesc.cuboid(DESC.radius * 1.8, DESC.shoulderHeight / 2, DESC.radius * 1.8), base);
  const carrier = dyn([DESC.base[0], sh, DESC.base[2]]);
  const link1 = dyn([DESC.base[0], sh, DESC.base[2]]);
  const link2 = dyn([DESC.base[0] + DESC.links[0], sh, DESC.base[2]]);
  const wrist = dyn([DESC.base[0] + DESC.links[0] + DESC.links[1], sh, DESC.base[2]]);
  const linkCol = (b, half, off) => {
    const c = RAPIER.ColliderDesc.cuboid(...half).setDensity(1500).setFriction(0.7)
      .setTranslation(...off);
    world.createCollider(c, b);
  };
  linkCol(carrier, [0.015, 0.015, 0.015], [0, 0, 0]);
  linkCol(link1, [DESC.links[0] / 2, DESC.radius, DESC.radius], [DESC.links[0] / 2, 0, 0]);
  linkCol(link2, [DESC.links[1] / 2, DESC.radius, DESC.radius], [DESC.links[1] / 2, 0, 0]);
  linkCol(wrist, [DESC.links[2] / 2, DESC.radius, DESC.radius], [DESC.links[2] / 2, 0, 0]);
  // revolute 关节（impulse joint + ForceBased 位置马达，与 Rust 同参数）
  const anchor = { x: 0, y: DESC.shoulderHeight, z: 0 };
  const stages = [
    { p: base, c: carrier, a1: { x: 0, y: DESC.shoulderHeight / 2, z: 0 }, axis: { x: 0, y: 1, z: 0 }, lim: DESC.limits[0] },
    { p: carrier, c: link1, a1: { x: 0, y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 }, lim: DESC.limits[1] },
    { p: link1, c: link2, a1: { x: DESC.links[0], y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 }, lim: DESC.limits[2] },
    { p: link2, c: wrist, a1: { x: DESC.links[1], y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 }, lim: DESC.limits[3] },
  ];
  const joints = stages.map(({ p, c, a1, axis, lim }) => {
    const jd = RAPIER.JointData.revolute(a1, { x: 0, y: 0, z: 0 }, axis);
    jd.limitsEnabled = true;
    jd.limits = lim;
    const j = world.createImpulseJoint(jd, p, c, true);
    j.setContactsEnabled(false);
    j.configureMotorModel(RAPIER.MotorModel.ForceBased);
    j.configureMotorPosition(0, DESC.kp, DESC.kd);
    return j;
  });
  // cube
  const cube = world.createRigidBody(
    RAPIER.RigidBodyDesc.dynamic().setLinearDamping(0.1).setAngularDamping(0.1)
      .setTranslation(...K.spawn));
  world.createCollider(
    RAPIER.ColliderDesc.cuboid(PROP_HALF, PROP_HALF, PROP_HALF).setDensity(400).setFriction(0.8), cube);
  // 夹板（运动学，随腕架位姿驱动）
  const mkFinger = () => {
    const b = world.createRigidBody(
      RAPIER.RigidBodyDesc.kinematicPositionBased()
        .setTranslation(0, 0, 0));
    world.createCollider(
      RAPIER.ColliderDesc.cuboid(...FINGER_HALF).setFriction(FINGER_FRICTION), b);
    return b;
  };
  const fingerL = mkFinger();
  const fingerR = mkFinger();
  K.bodies = { base, carrier, link1, link2, wrist, cube };
  K.fingerL = fingerL; K.fingerR = fingerR; K.gapNow = FINGER_GAP_OPEN;
  K.joints = joints;
  K.stages = stages.map(({ p, c, axis }) => ({ p: p.handle, c: c.handle, axis }));
  return world;
}

// 关节角读出：几何式 parent_rot⁻¹ ⊗ child_rot 绕轴角（impulse joint
// 无 Rust 侧的 local_to_parent 读出，体姿相除同样精确）。
function readAngles() {
  const byHandle = new Map(Object.entries(K.bodies).map(([k, b]) => [b.handle, b]));
  return K.stages.map(({ p, c, axis }) => {
    const pb = byHandle.get(p), cb = byHandle.get(c);
    const pr = pb.rotation(), cr = cb.rotation();
    const rel = new THREE.Quaternion(pr.x, pr.y, pr.z, pr.w).invert()
      .multiply(new THREE.Quaternion(cr.x, cr.y, cr.z, cr.w));
    return 2 * Math.atan2(
      rel.x * axis.x + rel.y * axis.y + rel.z * axis.z, rel.w);
  });
}

function eePose() {
  const b = K.bodies.wrist;
  const t = b.translation();
  const r = b.rotation();
  const off = new THREE.Vector3(...DESC.links).applyQuaternion(new THREE.Quaternion(r.x, r.y, r.z, r.w));
  return [t.x + off.x, t.y + off.y, t.z + off.z];
}

function observe() {
  const ee = K.lastEE;
  const ct = K.bodies.cube.translation();
  const rel = [ct.x - ee[0], ct.y - ee[1], ct.z - ee[2]];
  const g = [K.goal[0] - ee[0], K.goal[1] - ee[1], K.goal[2] - ee[2]];
  const dist = Math.hypot(...rel);
  return [
    ...readAngles(), ...K.lastVel, K.gripper,
    ...ee, ...rel, ...g,
    K.holding ? 1 : 0, ct.y, dist,
  ];
}

function driveFingers() {
  const gTarget = FINGER_GAP_OPEN + (FINGER_GAP_CLOSED - FINGER_GAP_OPEN) * Math.min(Math.max(K.gripper, 0), 1);
  K.gapNow += Math.min(Math.max(gTarget - K.gapNow, -0.004), 0.004);
  const wrist = K.bodies.wrist;
  const t = wrist.translation();
  const r = wrist.rotation();
  const q = new THREE.Quaternion(r.x, r.y, r.z, r.w);
  const centre = new THREE.Vector3(FINGER_X, 0, 0).applyQuaternion(q).add(new THREE.Vector3(t.x, t.y, t.z));
  const side = new THREE.Vector3(0, 0, K.gapNow / 2).applyQuaternion(q);
  for (const [body, sign] of [[K.fingerL, 1], [K.fingerR, -1]]) {
    body.setNextKinematicRotation(r);
    body.setNextKinematicTranslation(centre.clone().addScaledVector(side, sign));
  }
}

// 报告式握持：合爪 + 方块在握区（真正持取由摩擦接触求解完成）。
function updateHolding() {
  const tip = K.lastEE;
  const ct = K.bodies.cube.translation();
  K.holding = K.gripper > 0.5 &&
    Math.hypot(ct.x - tip[0], ct.y - tip[1], ct.z - tip[2]) < GRASP_RADIUS;
}

function stepOnce(action) {
  // 马达目标（沿用「变了才写」协议）
  for (let i = 0; i < 4; i++) {
    const want = Math.min(Math.max(action[i], DESC.limits[i][0]), DESC.limits[i][1]);
    if (K.lastCmd[i] !== want) {
      K.joints[i].configureMotorPosition(want, DESC.kp, DESC.kd);
      K.lastCmd[i] = want;
    }
  }
  K.gripper = Math.min(Math.max(action[4] ?? 0, 0), 1);
  for (let s = 0; s < SUBSTEPS; s++) K.world.step();
  K.lastEE = eePose();
  driveFingers();
  updateHolding();
  const angles = readAngles();
  K.lastVel = angles.map((a, i) => (a - K.prevAngles[i]) / DT);
  K.prevAngles = angles.slice();
  K.tick++;
  return angles;
}

// 三维表现同步（boot 建好 visuals 之后才生效）
function syncVisuals() {
  if (!K.visuals.cube) return;
  const set = (mesh, b) => {
    const t = b.translation();
    const r = b.rotation();
    mesh.position.set(t.x, t.y, t.z);
    mesh.quaternion.set(r.x, r.y, r.z, r.w);
  };
  set(K.visuals.baseCol, K.bodies.base);
  set(K.visuals.carrier, K.bodies.carrier);
  set(K.visuals.link1, K.bodies.link1);
  set(K.visuals.link2, K.bodies.link2);
  set(K.visuals.wrist, K.bodies.wrist);
  set(K.visuals.cube, K.bodies.cube);
  K.visuals.goal.position.set(K.goal[0], 0.501, K.goal[2]);
}

// 任务回路：reset/step（对照 EmbodiedEnv）
function envReset(seed) {
  K.rng = { s: BigInt.asUintN(64, BigInt(seed ^ 0x5eed00c0ffee0001)) };
  K.spawn = [
    lcgNext(K.rng, OBJ_RANGE.x[0], OBJ_RANGE.x[1]), 0.55,
    lcgNext(K.rng, OBJ_RANGE.z[0], OBJ_RANGE.z[1]),
  ];
  K.goal = [
    lcgNext(K.rng, GOAL_RANGE.x[0], GOAL_RANGE.x[1]), 0.5,
    lcgNext(K.rng, GOAL_RANGE.z[0], GOAL_RANGE.z[1]),
  ];
  // 原地重置（MJ resetData 式）：world 只建一次——free/重建每次
  // 泄漏 wasm 内存，auto-reset 下 ~12s 撑爆一次（WebView2 反复重载
  // 的根因）。复位方块、链位姿、速度与马达目标即可。
  const first = !K.world;
  if (first) { K.world = makeWorld(seed); }
  const B = K.bodies;
  B.cube.setTranslation({ x: K.spawn[0], y: K.spawn[1], z: K.spawn[2] }, true);
  B.cube.setLinvel({ x: 0, y: 0, z: 0 }, true);
  B.cube.setAngvel({ x: 0, y: 0, z: 0 }, true);
  B.cube.setRotation({ x: 0, y: 0, z: 0, w: 1 }, true);
  const sh = DESC.base[1] + DESC.shoulderHeight;
  const homes = {
    base: [DESC.base[0], DESC.base[1] + DESC.shoulderHeight / 2, DESC.base[2]],
    carrier: [DESC.base[0], sh, DESC.base[2]],
    link1: [DESC.base[0], sh, DESC.base[2]],
    link2: [DESC.base[0] + DESC.links[0], sh, DESC.base[2]],
    wrist: [DESC.base[0] + DESC.links[0] + DESC.links[1], sh, DESC.base[2]],
  };
  for (const [name, t] of Object.entries(homes)) {
    const b = B[name];
    b.setTranslation({ x: t[0], y: t[1], z: t[2] }, true);
    b.setRotation({ x: 0, y: 0, z: 0, w: 1 }, true);
    b.setLinvel({ x: 0, y: 0, z: 0 }, true);
    b.setAngvel({ x: 0, y: 0, z: 0 }, true);
  }
  K.targets = [0, 0, 0, 0]; K.gripper = 0; K.lastCmd = [NaN, NaN, NaN, NaN];
  K.prevAngles = [0, 0, 0, 0]; K.lastVel = [0, 0, 0, 0]; K.holding = false;
  K.gapNow = FINGER_GAP_OPEN;
  K.wasHolding = false; K.steps = 0; K.tick = 0; K.policyMode = false; K.policyStep = 0;
  K.episode = null;
  for (let i = 0; i < SETTLE_TICKS; i++) { stepOnce([0, 0, 0, 0, 0]); }
  syncVisuals();
  return observe();
}

function envStep(action) {
  const angles = stepOnce(action);
  K.steps++;
  const tel = { angles, vel: K.lastVel, ee: K.lastEE, holding: K.holding };
  const ct = K.bodies.cube.translation();
  const ee = tel.ee;
  const dObj = Math.hypot(ct.x - ee[0], ct.y - ee[1], ct.z - ee[2]);
  const dGoal = Math.hypot(ct.x - K.goal[0], ct.y - K.goal[1], ct.z - K.goal[2]);
  let reward = -0.002;
  const graspEvent = tel.holding && !K.wasHolding;
  reward += tel.holding
    ? -1.2 * dGoal + 2 * Math.min(Math.max(ct.y - 0.5, 0), 0.2)
    : -0.6 * dObj;
  reward += graspEvent ? 2 : 0;
  K.wasHolding = tel.holding;
  const inZone = dGoal < ZONE_RADIUS && ct.y > 0.5 && ct.y < 0.62;
  const terminated = inZone;
  const truncated = !terminated && K.steps >= MAX_STEPS;
  if (terminated) reward += 10;
  return { obs: observe(), reward, terminated, truncated, success: terminated };
}

export async function boot(viewport) {
  const stage = (t) => { const el = document.getElementById("load"); if (el) el.firstElementChild.textContent = "启动: " + t; };
  stage("renderer");
  const renderer = new THREE.WebGLRenderer({ antialias: true });
  renderer.setSize(viewport.clientWidth, viewport.clientHeight);
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  viewport.appendChild(renderer.domElement);
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x0a0d14);
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
  // 灯光
  scene.add(new THREE.AmbientLight(0x8899bb, 0.7));
  const dl = new THREE.DirectionalLight(0xffffff, 1.4);
  dl.position.set(1.5, 2.5, 1.0);
  scene.add(dl);
  // 地面/桌面/目标圈
  const mat = (c) => new THREE.MeshStandardMaterial({ color: c, roughness: 0.85 });
  const ground = new THREE.Mesh(new THREE.BoxGeometry(4.0, 0.1, 3.0), mat(0x141a26));
  ground.position.set(0, -0.05, 0);
  scene.add(ground);
  const table = new THREE.Mesh(new THREE.BoxGeometry(...TABLE.half.map((v) => v * 2)), mat(0x27313f));
  table.position.set(...TABLE.center);
  scene.add(table);
  const goal = new THREE.Mesh(
    new THREE.RingGeometry(ZONE_RADIUS * 0.75, ZONE_RADIUS, 40),
    new THREE.MeshBasicMaterial({ color: 0x7be0a0, side: THREE.DoubleSide, transparent: true, opacity: 0.8 }));
  goal.rotation.x = -Math.PI / 2;
  goal.position.set(0.28, 0.501, 0);
  scene.add(goal);
  K.visuals.goal = goal;
  // 机械臂与方块
  const armMat = mat(0x5b8def);
  const group = (parent) => { const g = new THREE.Group(); parent.add(g); return g; };
  scene.add((K.visuals.root = group(scene)));
  const mkBox = (hw, c) => new THREE.Mesh(new THREE.BoxGeometry(hw[0] * 2, hw[1] * 2, hw[2] * 2), mat(c));
  K.visuals.baseCol = mkBox([DESC.radius * 1.8, DESC.shoulderHeight / 2, DESC.radius * 1.8], 0x39415a);
  K.visuals.carrier = mkBox([0.015, 0.015, 0.015], 0x8fa8d8);
  // 圆柱长轴是 Y；连杆体坐标沿 +X——把旋转烘进几何体，四元数才不覆盖
  const linkGeo = (len) => new THREE.CylinderGeometry(DESC.radius, DESC.radius, len, 16).rotateZ(-Math.PI / 2);
  K.visuals.link1 = new THREE.Mesh(linkGeo(DESC.links[0]), armMat);
  K.visuals.link2 = new THREE.Mesh(linkGeo(DESC.links[1]), armMat);
  K.visuals.wrist = new THREE.Mesh(linkGeo(DESC.links[2]), mat(0x8fa8d8));
  K.visuals.cube = mkBox([PROP_HALF, PROP_HALF, PROP_HALF], 0xffd479);
  const g2 = new THREE.Group();
  g2.add(K.visuals.link1, K.visuals.link2, K.visuals.wrist, K.visuals.cube,
         K.visuals.baseCol, K.visuals.carrier);
  scene.add(g2);
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

  // 遥操作键盘
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
  const sliders = JOINT_NAMES.slice(0, 4).map((name, i) => {
    const row = document.createElement("div");
    row.className = "row";
    row.innerHTML = `<label>${name}</label><input type="range" min="${DESC.limits[i][0]}" max="${DESC.limits[i][1]}" step="0.01" value="0"><span class="val">0.00</span>`;
    jointsDiv.appendChild(row);
    const input = row.querySelector("input");
    input.addEventListener("input", () => {
      K.targets[i] = parseFloat(input.value);
      row.querySelector(".val").textContent = K.targets[i].toFixed(2);
    });
    return { input, val: row.querySelector(".val") };
  });
  const gripInput = document.getElementById("j-grip");
  gripInput.addEventListener("input", () => {
    K.gripper = parseFloat(gripInput.value);
    document.getElementById("v-grip").textContent = K.gripper > 0.5 ? "合" : "开";
  });
  const btn = (id, fn) => document.getElementById(id).addEventListener("click", fn);
  btn("btn-reset", () => { const o = envReset((Math.random() * 1e9) | 0); log.textContent = "已重置（随机种子）"; post({ type: "state", note: "reset" }); return o; });
  btn("btn-policy", () => { K.policyMode = true; K.policyStep = 0; log.textContent = "脚本策略运行中…"; });
  btn("btn-rec", (e) => {
    K.recording = !K.recording;
    e.target.textContent = K.recording ? "■ 停止录制" : "● 开始录制";
    e.target.classList.toggle("rec", K.recording);
    if (K.recording) {
      K.episode = { task: TASK_PHRASE, rows: [], obs: observe() };
      K.recCb?.(true);
      log.textContent = "录制中：动关节 / 跑策略，每 tick 记一行 (state, action)。";
    } else if (K.episode) {
      if (K.episode.rows.length) K.episodes.push(K.episode);
      K.episode = null;
      post({ type: "progress", rows: 0, episodes: K.episodes.length });
      log.textContent = `已停。共 ${K.episodes.length} 集 · ${K.episodes.reduce((a, x) => a + x.rows.length, 0)} 行`;
    }
  });
  btn("btn-export", () => {
    if (K.episode?.rows?.length) { K.episodes.push(K.episode); K.episode = null; }
    if (!K.episodes.length) { log.textContent = "没有可导出的 episode——先录一段。"; return; }
    if (window.parent !== window) { post({ type: "episodes", episodes: K.episodes, obsNames: OBS_NAMES, actionNames: ACTION_NAMES, task: TASK_PHRASE, fps: 1 / DT }); K.episodes = []; log.textContent = "已交给 Studio 工作台。"; }
    else {
      const blob = new Blob([JSON.stringify({ task: TASK_PHRASE, fps: 1 / DT, obsNames: OBS_NAMES, actionNames: ACTION_NAMES, episodes: K.episodes }, null, 1)], { type: "application/json" });
      const a = document.createElement("a");
      a.href = URL.createObjectURL(blob);
      a.download = "oriera-embodied-episodes.json";
      a.click();
      log.textContent = "已下载 JSON（在 Studio 内导出可一键落盘 LeRobot v2.1）。";
    }
  });

  function post(msg) { try { window.parent.postMessage({ source: "oriera-sim", ...msg }, "*"); } catch {} }
  addEventListener("message", (e) => {
    const m = e.data;
    if (!m || m.source !== "oriera-sim" || !m.cmd) return;
    if (m.cmd === "reset") envReset(m.seed ?? ((Math.random() * 1e9) | 0));
    else if (m.cmd === "policy") { K.policyMode = true; K.policyStep = 0; }
    else if (m.cmd === "grip") K.gripper = m.value;
    else if (m.cmd === "teleop") { K.policyMode = false; for (let i = 0; i < 4; i++) K.targets[i] = m.angles[i]; K.gripper = m.gripper ?? K.gripper; }
    else if (m.cmd === "record") document.getElementById("btn-rec").click();
    else if (m.cmd === "export") document.getElementById("btn-export").click();
  });

  stage("rapier init");
  await RAPIER.init(new URL("../vendor/rapier_wasm3d_bg.wasm", import.meta.url));
  stage("world");
  try { envReset(1000); } catch (e) { stage("ERR: " + (e && e.message ? e.message : e)); throw e; }
  document.getElementById("load").remove();

  let rewardAvg = 0, successCount = 0, episodesRun = 0, lastTerminated = false;
  function loop() {
    let action = null;
    for (const k of K.keys) {
      const m = keyMap[k];
      if (m) { K.targets[m[0]] = Math.min(Math.max(K.targets[m[0]] + m[1], DESC.limits[m[0]][0]), DESC.limits[m[0]][1]); }
    }
    if (K.policyMode) {
      const obs = observe();
      const ee = [obs[9], obs[10], obs[11]];
      const obj = [ee[0] + obs[12], ee[1] + obs[13], ee[2] + obs[14]];
      const gol = [ee[0] + obs[15], ee[1] + obs[16], ee[2] + obs[17]];
      let acc = 0;
      const ph = PHASES.find((p) => (acc += p.ticks) > K.policyStep);
      action = ph ? [...ik(ph.target(obj, gol)), ph.grip] : [...ik(above(gol, 0.22)), 0];
      K.policyStep++;
      for (let i = 0; i < 4; i++) sliders[i].input.value = action[i];
    } else {
      action = [...K.targets, K.gripper];
    }
    const out = envStep(action);
    rewardAvg = rewardAvg * 0.98 + out.reward * 0.02;
    if (K.recording && K.episode) {
      K.episode.rows.push({ state: out.obs, action });
      if (K.episode.rows.length % 30 === 0) {
        post({ type: "progress", rows: K.episode.rows.length, episodes: K.episodes.length + 1 });
      }
      sliders.forEach((s, i) => (s.val.textContent = K.prevAngles[i].toFixed(2)));
      K.logCb?.(`录制中：${K.episode.rows.length} 行 · r̄=${rewardAvg.toFixed(3)}${K.holding ? " · 🤏已抓住" : ""}`);
    }
    // 回合语义与 Rust env 循环对齐：终止/截断即回合结束——自动重置
    // （此前 steps 超限后每帧都计一次 episode，HUD 集数虚高）。
    // 录制卫生：回合结束先切录音带（当前 episode 落列表），再重置。
    if (out.terminated || out.truncated) {
      if (K.recording && K.episode?.rows?.length) K.episodes.push(K.episode);
      K.episode = K.recording ? { task: TASK_PHRASE, rows: [], obs: observe() } : null;
      episodesRun++;
      if (out.terminated) {
        successCount++; K.policyMode = false;
        post({ type: "state", note: "success" });
      }
      envReset((Math.random() * 1e9) | 0);
      if (K.policyMode) K.policyStep = 0;
    }
    syncVisuals();
    const ct = K.bodies.cube.translation();
    K.hudCb?.(
      `tick ${K.tick} · r̄ ${rewardAvg.toFixed(3)} · 距目标 ${Math.hypot(ct.x - K.goal[0], ct.z - K.goal[2]).toFixed(3)}m` +
      `<br>成功 ${successCount}/${episodesRun} · ${K.holding ? "🤏 抓住" : "✋ 松开"} · obs 21 维 / act 5 维`);
    renderer.render(scene, camera);
    requestAnimationFrame(loop);
  }
  loop();
  addEventListener("resize", () => {
    renderer.setSize(viewport.clientWidth, viewport.clientHeight);
    camera.aspect = viewport.clientWidth / viewport.clientHeight;
    camera.updateProjectionMatrix();
  });
  post({ type: "state", note: "ready" });

  // CDP/测试钩子（Studio cdp_*.mjs 传统）：确定性地驱动与读态。
  window.__orieraSim = {
    state: () => {
      const ct = K.bodies.cube.translation();
      return {
        cube: [ct.x, ct.y, ct.z],
        ee: K.lastEE,
        holding: K.holding,
        gripper: K.gripper,
        tick: K.tick,
      };
    },
  };
}
