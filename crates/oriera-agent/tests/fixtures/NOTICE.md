# tests/fixtures — provenance & licences

Fixtures for `crates/oriera-agent/tests/urdf_robot_spec.rs` (URDF → RobotSpec
full-chain import, PLAN-embodied-general M0a 工作包 D / docs/URDF-IMPORT.md U4).
Nothing in this directory is a vendor release; every file says in its header
exactly what it was derived from.

| File | What it is | Derived from | Licence |
|---|---|---|---|
| `panda7.urdf` | Franka Emika Panda 7-DoF, "converted reference" | `out/mjcf-crosscheck/panda_converted.urdf` — itself the 1:1 conversion (`scripts/mjcf_crosscheck.py`) of `mujoco_menagerie/franka_emika_panda/panda_nohand.xml`, [google-deepmind/mujoco_menagerie](https://github.com/google-deepmind/mujoco_menagerie) rev `b065505` | Apache-2.0 (menagerie); the Franka model data in menagerie is published by DeepMind under Apache-2.0 |
| `gripper2f.urdf` | Generic external two-finger parallel gripper | Hand-written for this repo; dimensions *inspired by* the Franka Hand (finger slide 0..0.04 m, 17×8×17 mm pads) but no menagerie/Franka content is copied | Repository licence |
| `NOTICE.md` | This file | — | — |

## panda7.urdf — deltas against the crosscheck URDF

Both deltas are deliberate and documented in the file header:

1. **`<collision><geometry><mesh filename="package://mujoco_menagerie/franka_emika_panda/assets/…"/>`
   on every link.** The URIs name the menagerie collision assets the MJCF
   references (`link0.stl` … `link7.stl`, `link5_collision_0.obj`). The meshes
   are **not** redistributed here and are **not** loaded by the tests: they
   exist so the importer's honest mesh → chain-segment-envelope degradation is
   exercised on a real 7-link chain (`mesh_uri` stays `None`, the URI lands in
   `RobotSpecOpts.skipped` for M2 to consume).
2. **`<limit velocity>`** on the seven joints — 2.1750 rad/s (joints 1–4),
   2.6100 rad/s (joints 5–7): the Franka datasheet values as carried by
   `franka_description` (Apache-2.0). The menagerie MJCF carries no joint
   velocity limit (MuJoCo has none natively), hence the crosscheck URDF has none
   either; `JointSpec::velocity_limit` needs a real number to be tested.

Everything else (joint origins/rpy, axes, limits, efforts, damping, the full
`<inertial>` tensors incl. off-diagonals, the `attachment` fixed link) is
byte-for-byte the crosscheck payload that `scripts/mjcf_crosscheck.py`
validates against MuJoCo (125/125 PASS).

## Regenerating

`panda_converted.urdf` is rewritten on every `python scripts/mjcf_crosscheck.py`
run from the `URDF_TEMPLATE` in that script. If the template changes, re-copy it
here and re-apply the two deltas above.
