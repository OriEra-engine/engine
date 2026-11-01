//! 全量复刻 kernel 世界（4 关节链+基座立柱+方块+指板+碰撞组），二分
//! 定位肩部保持力稀释源。跑法：node examples/15-embodied-sim/tools/motor_probe2.mjs
import RAPIER from "../vendor/rapier.mjs";

await RAPIER.init(new URL("../vendor/rapier_wasm3d_bg.wasm", import.meta.url));

const DT = 1 / 60, SUBSTEPS = 2;
const DESC = {
  base: [0.02, 0.5, 0.0], shoulderHeight: 0.12,
  links: [0.20, 0.18, 0.09], radius: 0.022,
  limits: [[-Math.PI, Math.PI], [-0.35, 1.45], [-2.5, 0.35], [-2.0, 2.0]],
  kp: 60, kd: 4,
};
const G_WORLD = 0x00010003, G_ARM = 0x00020001;

function build({ withGroups = true, withCube = true, withFingers = true } = {}) {
  const world = new RAPIER.World({ x: 0, y: -9.8, z: 0 });
  world.timestep = DT / SUBSTEPS;
  const grp = (c, g) => (withGroups ? c.setCollisionGroups(g) : c);
  const slab = (c, h) => {
    const b = world.createRigidBody(RAPIER.RigidBodyDesc.fixed().setTranslation(...c));
    world.createCollider(grp(RAPIER.ColliderDesc.cuboid(...h).setFriction(0.9), G_WORLD), b);
  };
  slab([0, -0.05, 0], [2.0, 0.05, 1.5]);
  slab([0.19, 0.25, 0], [0.31, 0.25, 0.31]); // 桌面（top 0.5）
  const sh = DESC.base[1] + DESC.shoulderHeight;
  const base = world.createRigidBody(
    RAPIER.RigidBodyDesc.fixed().setTranslation(DESC.base[0], DESC.base[1] + DESC.shoulderHeight / 2, DESC.base[2]));
  world.createCollider(grp(
    RAPIER.ColliderDesc.cuboid(DESC.radius * 1.8, DESC.shoulderHeight / 2, DESC.radius * 1.8), G_ARM), base);
  const dyn = (t) => world.createRigidBody(
    RAPIER.RigidBodyDesc.dynamic().setCanSleep(false).setAdditionalMass(0.2).setTranslation(...t));
  const carrier = dyn([DESC.base[0], sh, DESC.base[2]]);
  const link1 = dyn([DESC.base[0], sh, DESC.base[2]]);
  const link2 = dyn([DESC.base[0] + DESC.links[0], sh, DESC.base[2]]);
  const wrist = dyn([DESC.base[0] + DESC.links[0] + DESC.links[1], sh, DESC.base[2]]);
  const linkCol = (b, half, off) => world.createCollider(grp(
    RAPIER.ColliderDesc.cuboid(...half).setDensity(1500).setFriction(0.7).setTranslation(...off), G_ARM), b);
  linkCol(carrier, [0.015, 0.015, 0.015], [0, 0, 0]);
  linkCol(link1, [DESC.links[0] / 2, DESC.radius, DESC.radius], [DESC.links[0] / 2, 0, 0]);
  linkCol(link2, [DESC.links[1] / 2, DESC.radius, DESC.radius], [DESC.links[1] / 2, 0, 0]);
  linkCol(wrist, [DESC.links[2] / 2, DESC.radius, DESC.radius], [DESC.links[2] / 2, 0, 0]);

  const stages = [
    { p: base, c: carrier, a1: { x: 0, y: DESC.shoulderHeight / 2, z: 0 }, axis: { x: 0, y: 1, z: 0 } },
    { p: carrier, c: link1, a1: { x: 0, y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 } },
    { p: link1, c: link2, a1: { x: DESC.links[0], y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 } },
    { p: link2, c: wrist, a1: { x: DESC.links[1], y: 0, z: 0 }, axis: { x: 0, y: 0, z: 1 } },
  ];
  const joints = stages.map(({ p, c, a1, axis }) => {
    const jd = RAPIER.JointData.revolute(a1, { x: 0, y: 0, z: 0 }, axis);
    const j = world.createImpulseJoint(jd, p, c, true);
    j.setContactsEnabled(false);
    j.configureMotorModel(RAPIER.MotorModel.ForceBased);
    j.configureMotorPosition(0, DESC.kp, DESC.kd);
    return j;
  });

  let cube = null, fingers = [];
  if (withCube) {
    cube = world.createRigidBody(RAPIER.RigidBodyDesc.dynamic()
      .setLinearDamping(0.1).setAngularDamping(0.1).setTranslation(0.176, 0.55, -0.092));
    world.createCollider(grp(
      RAPIER.ColliderDesc.cuboid(0.025, 0.025, 0.025).setDensity(400).setFriction(0.8), G_WORLD), cube);
  }
  if (withFingers) {
    const mk = () => {
      const b = world.createRigidBody(RAPIER.RigidBodyDesc.kinematicPositionBased().setTranslation(0, 0, 0));
      world.createCollider(grp(
        RAPIER.ColliderDesc.cuboid(0.030, 0.045, 0.006).setFriction(1.4), G_ARM), b);
      return b;
    };
    fingers = [mk(), mk()];
    // home 指板位姿（TCP=FINGER_X 越过腕心）
    const wt = wrist.translation();
    for (const [i, sgn] of [[0, 1], [1, -1]]) {
      fingers[i].setNextKinematicTranslation(wt.x + 0.135, wt.y, wt.z + sgn * 0.075 / 2);
    }
  }
  const angles = () => {
    const rel = (pb, cb) => {
      const pr = pb.rotation(), cr = cb.rotation();
      const q = { // pr⁻¹ * cr
        w: pr.w * cr.w + pr.x * cr.x + pr.y * cr.y + pr.z * cr.z,
        x: pr.w * cr.x - pr.x * cr.w - pr.y * cr.z + pr.z * cr.y,
        y: pr.w * cr.y + pr.x * cr.z - pr.y * cr.w + pr.z * cr.x,
        z: pr.w * cr.z - pr.x * cr.y + pr.y * cr.x - pr.z * cr.w,
      };
      return q;
    };
    const pairs = [[base, carrier, { x: 0, y: 1, z: 0 }], [carrier, link1, { x: 0, y: 0, z: 1 }],
      [link1, link2, { x: 0, y: 0, z: 1 }], [link2, wrist, { x: 0, y: 0, z: 1 }]];
    return pairs.map(([p, c, ax]) => {
      const r = rel(p, c);
      return 2 * Math.atan2(r.x * ax.x + r.y * ax.y + r.z * ax.z, r.w);
    });
  };
  return { world, joints, angles, cube, fingers, bodies: { base, carrier, link1, link2, wrist } };
}

function run(label, opts, steps = 720) {
  const { world, joints, angles } = build(opts);
  for (let i = 0; i < steps; i++) world.step();
  console.log(label.padEnd(46), "angles:", angles().map((a) => a.toFixed(3)).join(", "));
}

console.log("── settle 全零目标，6 s 后 ──");
run("A 全量复刻（组+方块+指板）", {});
run("B 无碰撞组", { withGroups: false });
run("C 无方块", { withCube: false });
run("D 无指板", { withFingers: false });
run("E 无方块无指板", { withCube: false, withFingers: false });
run("F 无组无方块无指板", { withGroups: false, withCube: false, withFingers: false });
