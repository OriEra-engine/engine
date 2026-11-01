//! probe3：4 关节全链全零目标——读每级绝对姿态，验证「载具俯仰假摔」
//! 假设（脉冲链锁定自由度软化 → carrier 倾斜 → 肩角读数假性下垂）。
import RAPIER from "../vendor/rapier.mjs";

await RAPIER.init(new URL("../vendor/rapier_wasm3d_bg.wasm", import.meta.url));

const world = new RAPIER.World({ x: 0, y: -9.8, z: 0 });
world.timestep = 1 / 120;
const G_WORLD = 0x00010003, G_ARM = 0x00020001;

world.createCollider(RAPIER.ColliderDesc.cuboid(2, 0.05, 1.5).setCollisionGroups(G_WORLD),
  world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(0, -0.05, 0)));

const base = world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(0.02, 0.56, 0));
world.createCollider(RAPIER.ColliderDesc.cuboid(0.0396, 0.06, 0.0396).setCollisionGroups(G_ARM), base);

const sh = 0.62;
const dyn = (t) => world.createRigidBody(
  RAPIER.RigidBodyDesc.dynamic().setCanSleep(false).setAdditionalMass(0.2).setTranslation(...t));
const carrier = dyn([0.02, sh, 0]);
const link1 = dyn([0.02, sh, 0]);
const link2 = dyn([0.22, sh, 0]);
const wrist = dyn([0.40, sh, 0]);
const lc = (b, half, off) => world.createCollider(
  RAPIER.ColliderDesc.cuboid(...half).setDensity(1500).setFriction(0.7)
    .setCollisionGroups(G_ARM).setTranslation(...off), b);
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
mk(carrier, link1, { x: 0, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
mk(link1, link2, { x: 0.20, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });
mk(link2, wrist, { x: 0.18, y: 0, z: 0 }, { x: 0, y: 0, z: 1 });

for (let i = 0; i < 720; i++) world.step(); // 6 s settle

const zrot = (b) => { const r = b.rotation(); return 2 * Math.atan2(r.z, r.w); };
const yrot = (b) => { const r = b.rotation(); return 2 * Math.atan2(r.y, r.w); };
console.log("carrier: zPitch=" + zrot(carrier).toFixed(4) + "  yYaw=" + yrot(carrier).toFixed(4));
console.log("link1  : zPitch=" + zrot(link1).toFixed(4));
console.log("link2  : zPitch=" + zrot(link2).toFixed(4));
console.log("wrist  : zPitch=" + zrot(wrist).toFixed(4));
console.log("→ 肩角读数(carrier→link1 相对) ≈ " + (zrot(link1) - zrot(carrier)).toFixed(4));
