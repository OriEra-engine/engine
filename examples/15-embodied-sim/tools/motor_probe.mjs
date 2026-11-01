//! 最小复现：vendored rapier wasm 的 ForceBased 位置马达在双摆链上的
//! 真实 authority（前台验证发现的肩部保持力稀释问题的隔离试验台）。
//! 跑法：node examples/15-embodied-sim/tools/motor_probe.mjs
import RAPIER from "../vendor/rapier.mjs";

await RAPIER.init(new URL("../vendor/rapier_wasm3d_bg.wasm", import.meta.url));

const world = new RAPIER.World({ x: 0, y: -9.8, z: 0 });
world.timestep = 1 / 120;

const base = world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(0, 0.6, 0));
const link1 = world.createRigidBody(
  RAPIER.RigidBodyDesc.dynamic().setCanSleep(false).setAdditionalMass(0.2).setTranslation(0, 0.6, 0));
const link2 = world.createRigidBody(
  RAPIER.RigidBodyDesc.dynamic().setCanSleep(false).setAdditionalMass(0.2).setTranslation(0.2, 0.6, 0));
world.createCollider(RAPIER.ColliderDesc.cuboid(0.015, 0.015, 0.015), link1);
world.createCollider(
  RAPIER.ColliderDesc.cuboid(0.1, 0.022, 0.022).setDensity(1500).setTranslation(0.1, 0, 0), link1);
world.createCollider(
  RAPIER.ColliderDesc.cuboid(0.09, 0.022, 0.022).setDensity(1500).setTranslation(0.09, 0, 0), link2);

const mk = (p, c, ax, anchor) => {
  const jd = RAPIER.JointData.revolute(anchor, { x: 0, y: 0, z: 0 }, ax);
  const j = world.createImpulseJoint(jd, p, c, true);
  j.setContactsEnabled(false);
  j.configureMotorModel(RAPIER.MotorModel.ForceBased);
  j.configureMotorPosition(0, 60, 4);
  return j;
};
const shoulder = mk(base, link1, { x: 0, y: 0, z: 1 }, { x: 0, y: 0, z: 0 });
const elbow = mk(link1, link2, { x: 0, y: 0, z: 1 }, { x: 0.2, y: 0, z: 0 });

// 无接触版本（碰撞组全关，排除接触干扰）
function angle(b) {
  const r = b.rotation();
  return 2 * Math.atan2(r.z, r.w);
}

// 试验 1：肩部目标 -1.0（抬臂），kp60——稳态误差多少？
shoulder.configureMotorPosition(-1.0, 60, 4);
elbow.configureMotorPosition(0, 60, 4);
for (let i = 0; i < 720; i++) world.step(); // 6 s
console.log("A kp60  shoulder target=-1.0 →", angle(link1).toFixed(3), " elbow:", angle(link2).toFixed(3));

// 试验 2：kp×8
shoulder.configureMotorPosition(-1.0, 480, 16);
for (let i = 0; i < 360; i++) world.step();
console.log("B kp480 shoulder target=-1.0 →", angle(link1).toFixed(3));

// 试验 3：setMotorMaxForce 大数
shoulder.setMotorMaxForce(1e6);
for (let i = 0; i < 360; i++) world.step();
console.log("C +maxForce1e6          →", angle(link1).toFixed(3));

// 试验 4：单摆（去掉 link2 的关节——看是否链式负载所致）
elbow.configureMotorPosition(0, 1e5, 1e3);
shoulder.configureMotorPosition(-1.0, 60, 4);
for (let i = 0; i < 720; i++) world.step();
console.log("D kp60 +肘部超硬锁     →", angle(link1).toFixed(3));

// 试验 5：无重力对照
world.gravity = { x: 0, y: 0, z: 0 };
for (let i = 0; i < 360; i++) world.step();
console.log("E 无重力                →", angle(link1).toFixed(3));
