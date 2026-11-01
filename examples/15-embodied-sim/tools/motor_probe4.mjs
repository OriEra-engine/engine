//! probe4：workpoint 扫描——yaw 在链时肩部马达的 kp/迭代/目标可达性。
import RAPIER from "../vendor/rapier.mjs";
await RAPIER.init(new URL("../vendor/rapier_wasm3d_bg.wasm", import.meta.url));

function build({ iters = null } = {}) {
  const world = new RAPIER.World({ x: 0, y: -9.8, z: 0 });
  world.timestep = 1 / 120;
  if (iters) world.integrationParameters.numSolverIterations = iters;
  const base = world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(0.02, 0.56, 0));
  const sh = 0.62;
  const dyn = (t) => world.createRigidBody(
    RAPIER.RigidBodyDesc.dynamic().setCanSleep(false).setAdditionalMass(0.2).setTranslation(...t));
  const carrier = dyn([0.02, sh, 0]);
  const link1 = dyn([0.02, sh, 0]);
  const link2 = dyn([0.22, sh, 0]);
  const wrist = dyn([0.40, sh, 0]);
  const lc = (b, half, off) => world.createCollider(
    RAPIER.ColliderDesc.cuboid(...half).setDensity(1500).setTranslation(...off), b);
  lc(carrier, [0.015, 0.015, 0.015], [0, 0, 0]);
  lc(link1, [0.10, 0.022, 0.022], [0.10, 0, 0]);
  lc(link2, [0.09, 0.022, 0.022], [0.09, 0, 0]);
  lc(wrist, [0.045, 0.022, 0.022], [0.045, 0, 0]);
  const mk = (p, c, a1, ax) => {
    const jd = RAPIER.JointData.revolute(a1, { x: 0, y: 0, z: 0 }, ax);
    const j = world.createImpulseJoint(jd, p, c, true);
    j.setContactsEnabled(false);
    j.configureMotorModel(RAPIER.MotorModel.ForceBased);
    j.configureMotorPosition(0, 60, 4);
    return j;
  };
  const yaw = mk(base, carrier, { x: 0, y: 0.06, z: 0 }, { x: 0, y: 1, z: 0 });
  const shoulder = mk(carrier, link1, { x: 0, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  mk(link1, link2, { x: 0.20, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  mk(link2, wrist, { x: 0.18, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  const zrot = (b) => { const r = b.rotation(); return 2 * Math.atan2(r.z, r.w); };
  return { world, shoulder, yaw, zrot, link1, carrier };
}

const sweep = [
  ["kp60  iters默认", 60, null],
  ["kp240", 240, null],
  ["kp960", 960, null],
  ["kp3840", 3840, null],
  ["kp60  iters=20", 60, 20],
  ["kp60  iters=100", 60, 100],
  ["kp960 iters=20", 960, 20],
];
for (const [label, kp, iters] of sweep) {
  // 目标 0：settle 下垂
  const a = build({ iters });
  for (let i = 0; i < 720; i++) a.world.step();
  // 目标 -1.0：可达性
  const b = build({ iters });
  b.shoulder.configureMotorPosition(-1.0, kp, 4);
  for (let i = 0; i < 720; i++) b.world.step();
  // kp 用于目标 0 场景（重建）
  const c = build({ iters });
  c.shoulder.configureMotorPosition(0, kp, 4);
  for (let i = 0; i < 720; i++) c.world.step();
  console.log(
    label.padEnd(16),
    "sag0(kp60)=" + a.zrot(a.link1).toFixed(3),
    " hold0(kp设)=" + c.zrot(c.link1).toFixed(3),
    " hold-1(kp设)=" + b.zrot(b.link1).toFixed(3),
  );
}
