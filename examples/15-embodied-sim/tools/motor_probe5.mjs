//! probe5：抬臂（重力反向）场景的 (kp,kd,iters) 工作点扫描。
//! 判据：终态误差 + 末 2 s 振荡幅度。
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
  mk(base, carrier, { x: 0, y: 0.06, z: 0 }, { x: 0, y: 1, z: 0 });
  const shoulder = mk(carrier, link1, { x: 0, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  const elbow = mk(link1, link2, { x: 0.20, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  const wristJ = mk(link2, wrist, { x: 0.18, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
  const zrot = (b) => { const r = b.rotation(); return 2 * Math.atan2(r.z, r.w); };
  return { world, shoulder, elbow, wristJ, zrot, link1, link2 };
}

const TARGET = 1.086;
const combos = [
  [60, 4, null], [60, 4, 20], [60, 4, 100],
  [240, 8, null], [240, 16, 20], [240, 16, 100],
  [960, 30, 20], [960, 30, 100], [960, 60, 20], [960, 60, 100],
  [3840, 120, 20], [3840, 120, 100],
];
for (const [kp, kd, iters] of combos) {
  const { world, shoulder, elbow, wristJ, zrot, link1, link2 } = build({ iters });
  shoulder.configureMotorPosition(TARGET, kp, kd);
  elbow.configureMotorPosition(-2.1, kp, kd);
  wristJ.configureMotorPosition(-0.5, kp, kd);
  const samples = [];
  for (let i = 0; i < 720; i++) {
    world.step();
    if (i >= 480) samples.push(zrot(link1)); // 末 2 s 采样
  }
  const fin = samples[samples.length - 1];
  const osc = Math.max(...samples) - Math.min(...samples);
  console.log(
    `kp=${String(kp).padStart(4)} kd=${String(kd).padStart(3)} iters=${String(iters ?? 4).padStart(3)}`,
    " err=" + (TARGET - fin >= 0 ? "+" : "") + (TARGET - fin).toFixed(3),
    " osc=" + osc.toFixed(3),
  );
}
