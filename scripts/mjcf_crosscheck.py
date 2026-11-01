#!/usr/bin/env python3
"""MJCF Panda full cross-validation — MuJoCo as the third-party reference frame.

Cross-checks the OriEra URDF import chain (crates/oriera-agent/src/urdf.rs,
read-only) against the official MuJoCo menagerie Franka Panda model:

  Ground truth (MuJoCo side):
    out/mjcf-crosscheck/mujoco_menagerie/franka_emika_panda/panda_nohand.xml
    (auto-fetched by sparse git clone from google-deepmind/mujoco_menagerie,
    Apache-2.0; revision recorded in the report)

  Converted reference (OriEra side):
    out/mjcf-crosscheck/panda_converted.urdf — 7-joint minimal URDF converted
    from the MJCF above (provenance and every number traceable to the MJCF;
    the cross-check itself re-verifies each number). Labeled "converted ref".

  Rust probe:
    cargo run --example urdf_chain_dump -p oriera-agent --features urdf
    (dumps UrdfReport per-joint/per-link + validate_chain report as JSON)

Checks (per task spec): joint type / axis / range / damping / armature /
mass / inertia / body pos (+ effort, rotation), plus a 10-sample
forward-kinematics workspace spot check (<= 5 mm end-effector deviation).

Tolerances: limits 0.02 rad, mass 5%, length 1 mm, damping 10%.
Status rule: PASS <= tol < TOLERANCE <= 2*tol < FAIL.
Missing-on-the-OriEra-side structural values (armature, inertia
off-diagonals) are recorded honestly as FAIL with a root-cause note.

Output: out/mjcf-crosscheck/report.json + a console table, ending with a
single PASS/FAIL summary line.

Dependencies: mujoco + numpy + stdlib only.
"""

from __future__ import annotations

import json
import math
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

try:
    import mujoco
except ImportError:
    print("ERROR: mujoco python package required (pip install mujoco)", file=sys.stderr)
    raise

# ---------------------------------------------------------------- paths ----

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "out" / "mjcf-crosscheck"
VENDOR = OUT / "mujoco_menagerie"
MJCF = VENDOR / "franka_emika_panda" / "panda_nohand.xml"
URDF = OUT / "panda_converted.urdf"
REPORT = OUT / "report.json"

MENAGERIE_URL = "https://github.com/google-deepmind/mujoco_menagerie"
MENAGERIE_DIR = "franka_emika_panda"
MENAGERIE_LICENSE = "Apache-2.0 (menagerie franka_emika_panda LICENSE)"

SEED = 20261101
N_FK_SAMPLES = 10
CHAIN_LEN = 7

TOL_LIMIT_RAD = 0.02
TOL_MASS_REL = 0.05
TOL_LEN_M = 0.001
TOL_DAMP_REL = 0.10
TOL_INERTIA_REL = 0.05
TOL_EFFORT_REL = 0.10
TOL_AXIS_RAD = 0.02
TOL_ROT_RAD = 0.02
TOL_FK_M = 0.005

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.stderr.reconfigure(encoding="utf-8", errors="replace")

# --------------------------------------------- converted-reference URDF ----
# "换算参照": every number below is converted 1:1 from
# mujoco_menagerie/franka_emika_panda/panda_nohand.xml (r32f2…, see report):
#   - joint origin xyz  = MJCF <body pos>          (child frame in parent)
#   - joint origin rpy  = MJCF <body quat>         (single-axis rotations,
#                                                    quat (w x y z) -> rpy)
#   - axis              = MJCF class panda axis "0 0 1"
#   - limits            = MJCF <joint range>       (default class or explicit)
#   - damping           = MJCF default class panda damping="1"
#   - effort            = MJCF <general forcerange> magnitude (87 / 12 N·m)
#   - inertial          = MJCF <inertial mass/pos/fullinertia>
#     (fullinertia order ixx iyy izz ixy ixz iyz == URDF attribute order;
#      note urdf.rs parses only the diagonal — the cross-check reports that)
# armature (MJCF default 0.1) has no URDF equivalent — structural gap, reported.
URDF_TEMPLATE = """<?xml version="1.0"?>
<!--
  panda_converted.urdf — "converted reference" (换算参照), NOT a Franka release.

  Provenance: converted 1:1 from mujoco_menagerie/franka_emika_panda/
  panda_nohand.xml (https://github.com/google-deepmind/mujoco_menagerie,
  Apache-2.0). Source of every number is that MJCF; conversion table in
  scripts/mjcf_crosscheck.py docstring. Consumed by the OriEra URDF import
  chain (crates/oriera-agent/src/urdf.rs) and cross-checked against MuJoCo
  by scripts/mjcf_crosscheck.py. No collision meshes (menagerie meshes are
  not re-distributed here); inertials/joints are the validated payload.
-->
<robot name="panda_converted_ref">

  <link name="link0">
    <inertial>
      <origin xyz="-0.041018 -0.00014 0.049974"/>
      <mass value="0.629769"/>
      <inertia ixx="0.00315" iyy="0.00388" izz="0.004285"
               ixy="8.2904e-7" ixz="0.00015" iyz="8.2299e-6"/>
    </inertial>
  </link>

  <link name="link1">
    <inertial>
      <origin xyz="0.003875 0.002081 -0.04762"/>
      <mass value="4.970684"/>
      <inertia ixx="0.70337" iyy="0.70661" izz="0.0091170"
               ixy="-0.00013900" ixz="0.0067720" iyz="0.019169"/>
    </inertial>
  </link>
  <joint name="joint1" type="revolute">
    <parent link="link0"/> <child link="link1"/>
    <!-- MJCF: <body name="link1" pos="0 0 0.333"> -->
    <origin xyz="0 0 0.333" rpy="0 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-2.8973" upper="2.8973" effort="87"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link2">
    <inertial>
      <origin xyz="-0.003141 -0.02872 0.003495"/>
      <mass value="0.646926"/>
      <inertia ixx="0.0079620" iyy="2.8110e-2" izz="2.5995e-2"
               ixy="-3.925e-3" ixz="1.0254e-2" iyz="7.04e-4"/>
    </inertial>
  </link>
  <joint name="joint2" type="revolute">
    <parent link="link1"/> <child link="link2"/>
    <!-- MJCF: <body name="link2" quat="1 -1 0 0"> = Rx(-pi/2) -->
    <origin xyz="0 0 0" rpy="-1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-1.7628" upper="1.7628" effort="87"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link3">
    <inertial>
      <origin xyz="2.7518e-2 3.9252e-2 -6.6502e-2"/>
      <mass value="3.228604"/>
      <inertia ixx="3.7242e-2" iyy="3.6155e-2" izz="1.083e-2"
               ixy="-4.761e-3" ixz="-1.1396e-2" iyz="-1.2805e-2"/>
    </inertial>
  </link>
  <joint name="joint3" type="revolute">
    <parent link="link2"/> <child link="link3"/>
    <!-- MJCF: <body name="link3" pos="0 -0.316 0" quat="1 1 0 0"> = Rx(+pi/2) -->
    <origin xyz="0 -0.316 0" rpy="1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <!-- range inherited from MJCF default class panda -->
    <limit lower="-2.8973" upper="2.8973" effort="87"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link4">
    <inertial>
      <origin xyz="-5.317e-2 1.04419e-1 2.7454e-2"/>
      <mass value="3.587895"/>
      <inertia ixx="2.5853e-2" iyy="1.9552e-2" izz="2.8323e-2"
               ixy="7.796e-3" ixz="-1.332e-3" iyz="8.641e-3"/>
    </inertial>
  </link>
  <joint name="joint4" type="revolute">
    <parent link="link3"/> <child link="link4"/>
    <!-- MJCF: <body name="link4" pos="0.0825 0 0" quat="1 1 0 0"> -->
    <origin xyz="0.0825 0 0" rpy="1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-3.0718" upper="-0.0698" effort="87"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link5">
    <inertial>
      <origin xyz="-1.1953e-2 4.1065e-2 -3.8437e-2"/>
      <mass value="1.225946"/>
      <inertia ixx="3.5549e-2" iyy="2.9474e-2" izz="8.627e-3"
               ixy="-2.117e-3" ixz="-4.037e-3" iyz="2.29e-4"/>
    </inertial>
  </link>
  <joint name="joint5" type="revolute">
    <parent link="link4"/> <child link="link5"/>
    <!-- MJCF: <body name="link5" pos="-0.0825 0.384 0" quat="1 -1 0 0"> -->
    <origin xyz="-0.0825 0.384 0" rpy="-1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-2.8973" upper="2.8973" effort="12"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link6">
    <inertial>
      <origin xyz="6.0149e-2 -1.4117e-2 -1.0517e-2"/>
      <mass value="1.666555"/>
      <inertia ixx="1.964e-3" iyy="4.354e-3" izz="5.433e-3"
               ixy="1.09e-4" ixz="-1.158e-3" iyz="3.41e-4"/>
    </inertial>
  </link>
  <joint name="joint6" type="revolute">
    <parent link="link5"/> <child link="link6"/>
    <!-- MJCF: <body name="link6" quat="1 1 0 0"> -->
    <origin xyz="0 0 0" rpy="1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-0.0175" upper="3.7525" effort="12"/>
    <dynamics damping="1"/>
  </joint>

  <link name="link7">
    <inertial>
      <origin xyz="1.0517e-2 -4.252e-3 6.1597e-2"/>
      <mass value="0.735522"/>
      <inertia ixx="1.2516e-2" iyy="1.0027e-2" izz="4.815e-3"
               ixy="-4.28e-4" ixz="-1.196e-3" iyz="-7.41e-4"/>
    </inertial>
  </link>
  <joint name="joint7" type="revolute">
    <parent link="link6"/> <child link="link7"/>
    <!-- MJCF: <body name="link7" pos="0.088 0 0" quat="1 1 0 0"> -->
    <origin xyz="0.088 0 0" rpy="1.5707963267948966 0 0"/>
    <axis xyz="0 0 1"/>
    <limit lower="-2.8973" upper="2.8973" effort="12"/>
    <dynamics damping="1"/>
  </joint>

  <link name="attachment"/>

  <joint name="joint8" type="fixed">
    <parent link="link7"/> <child link="attachment"/>
    <!-- MJCF: <body name="attachment" pos="0 0 0.107"
         quat="0.3826834 0 0 0.9238795"> = Rz(135 deg) -->
    <origin xyz="0 0 0.107" rpy="0 0 2.356194490192345"/>
  </joint>

</robot>
"""

# ------------------------------------------------------------- helpers ----


def log(msg: str) -> None:
    print(msg, flush=True)


def ensure_vendor() -> str:
    """Fetch menagerie if absent; return the checked-out revision."""
    if not MJCF.exists():
        log(f"[vendor] fetching {MENAGERIE_URL} (sparse) -> {VENDOR}")
        subprocess.run(
            ["git", "clone", "--depth", "1", "--filter=blob:none", "--sparse",
             MENAGERIE_URL, str(VENDOR)],
            check=True, capture_output=True, text=True,
        )
        subprocess.run(
            ["git", "sparse-checkout", "set", MENAGERIE_DIR],
            cwd=str(VENDOR), check=True, capture_output=True, text=True,
        )
    rev = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"], cwd=str(VENDOR),
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return rev


def quat_to_mat(q) -> np.ndarray:
    """MuJoCo quat (w x y z) -> 3x3 rotation matrix."""
    w, x, y, z = (float(v) for v in q)
    n = math.sqrt(w * w + x * x + y * y + z * z)
    w, x, y, z = w / n, x / n, y / n, z / n
    return np.array([
        [1 - 2 * (y * y + z * z), 2 * (x * y - w * z), 2 * (x * z + w * y)],
        [2 * (x * y + w * z), 1 - 2 * (x * x + z * z), 2 * (y * z - w * x)],
        [2 * (x * z - w * y), 2 * (y * z + w * x), 1 - 2 * (x * x + y * y)],
    ])


def rpy_to_mat(rpy) -> np.ndarray:
    """URDF rpy (extrinsic XYZ) -> 3x3 rotation matrix Rz(y)Ry(p)Rx(r)."""
    r, p, y = (float(v) for v in rpy)
    cr, sr, cp, sp, cy, sy = (math.cos(r), math.sin(r), math.cos(p),
                              math.sin(p), math.cos(y), math.sin(y))
    rx = np.array([[1, 0, 0], [0, cr, -sr], [0, sr, cr]])
    ry = np.array([[cp, 0, sp], [0, 1, 0], [-sp, 0, cp]])
    rz = np.array([[cy, -sy, 0], [sy, cy, 0], [0, 0, 1]])
    return rz @ ry @ rx


def rot_angle(r: np.ndarray) -> float:
    """Rotation angle of a 3x3 matrix (rad), numerically safe."""
    c = (np.trace(r) - 1.0) / 2.0
    return float(math.acos(max(-1.0, min(1.0, c))))


def axis_angle_diff(a: np.ndarray, b: np.ndarray) -> float:
    na, nb = np.linalg.norm(a), np.linalg.norm(b)
    if na < 1e-9 or nb < 1e-9:
        return math.pi if (na < 1e-9) != (nb < 1e-9) else 0.0
    c = float(np.dot(a / na, b / nb))
    return math.acos(max(-1.0, min(1.0, c)))


def status_of(diff: float, tol: float) -> str:
    if diff <= tol:
        return "PASS"
    if diff <= 2.0 * tol:
        return "TOLERANCE"
    return "FAIL"


def fmt(v) -> str:
    if v is None:
        return "-"
    if isinstance(v, str):
        return v
    if isinstance(v, (list, tuple, np.ndarray)):
        return "(" + ", ".join(fmt(x) for x in v) + ")"
    if isinstance(v, float):
        return f"{v:.6g}"
    return str(v)


# ----------------------------------------------------- Rust chain dump ----


def run_rust_dump(urdf_path: Path) -> dict:
    """cargo run --example urdf_chain_dump (retries on build-lock contention)."""
    cmd = ["cargo", "run", "--example", "urdf_chain_dump",
           "-p", "oriera-agent", "--features", "urdf", "--quiet", "--",
           str(urdf_path)]
    for attempt in range(4):
        p = subprocess.run(cmd, cwd=str(REPO), capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=900)
        if p.returncode == 0:
            return json.loads(p.stdout)
        blob = (p.stderr or "") + (p.stdout or "")
        low = blob.lower()
        if ("file lock" in low or "blocking waiting" in low
                or "another cargo" in low):
            log(f"[rust] build lock busy (attempt {attempt + 1}/4), wait 30s")
            time.sleep(30)
            continue
        sys.stderr.write(blob[-4000:])
        raise RuntimeError("urdf_chain_dump failed (see stderr above)")
    raise RuntimeError("urdf_chain_dump: build-lock retry exhausted")


# ------------------------------------------------------- MuJoCo extract ----


def extract_mujoco(mjcf_path: Path):
    model = mujoco.MjModel.from_xml_path(str(mjcf_path))
    data = mujoco.MjData(model)
    mujoco.mj_forward(model, data)

    def bid(name):
        i = mujoco.mj_name2id(model, mujoco.mjtObj.mjOBJ_BODY, name)
        assert i >= 0, f"body {name} missing"
        return i

    def jid(name):
        i = mujoco.mj_name2id(model, mujoco.mjtObj.mjOBJ_JOINT, name)
        assert i >= 0, f"joint {name} missing"
        return i

    bodies = {}
    for name in ["link0", "link1", "link2", "link3", "link4", "link5",
                 "link6", "link7", "attachment"]:
        i = bid(name)
        iquat = model.body_iquat[i]
        r_i = quat_to_mat(iquat)
        full = r_i @ np.diag(model.body_inertia[i]) @ r_i.T
        bodies[name] = {
            "pos": model.body_pos[i].copy(),
            "quat": model.body_quat[i].copy(),
            "mass": float(model.body_mass[i]),
            "ipos": model.body_ipos[i].copy(),
            "inertia_full": full,
        }

    joints = {}
    for k in range(1, CHAIN_LEN + 1):
        name = f"joint{k}"
        j = jid(name)
        d = model.jnt_dofadr[j]
        # effort: matching actuator's forcerange magnitude
        eff = None
        for a in range(model.nu):
            if model.actuator_trnid[a, 0] == j:
                eff = float(abs(model.actuator_forcerange[a, 1]))
                break
        joints[name] = {
            "type": int(model.jnt_type[j]),
            "axis": model.jnt_axis[j].copy(),
            "range": model.jnt_range[j].copy(),
            "damping": float(model.dof_damping[d]),
            "armature": float(model.dof_armature[d]),
            "effort": eff,
            "qposadr": int(model.jnt_qposadr[j]),
            "limited": bool(model.jnt_limited[j]),
        }

    site = mujoco.mj_name2id(model, mujoco.mjtObj.mjOBJ_SITE, "attachment_site")
    assert site >= 0, "site attachment_site missing"
    return model, data, bodies, joints, int(site)


# ------------------------------------------------------------ comparison ----

ROWS: list[dict] = []


def row(item, elem, mjcf_v, oriera_v, diff, tol, status, note=""):
    ROWS.append({
        "item": item, "element": elem,
        "mjcf_value": None if mjcf_v is None else fmt(mjcf_v),
        "oriera_value": None if oriera_v is None else fmt(oriera_v),
        "diff": None if diff is None else (fmt(diff) if isinstance(diff, str) else float(diff)),
        "tol": None if tol is None else float(tol),
        "status": status, "note": note,
    })


def compare_params(bodies_mj, joints_mj, dump) -> None:
    joints_u = {j["name"]: j for j in dump["joints"]}
    links_u = {l["name"]: l for l in dump["links"]}
    child_of = {j["child"]: j for j in dump["joints"]}

    # -- per-joint rows -----------------------------------------------------
    for k in range(1, CHAIN_LEN + 1):
        jn = f"joint{k}"
        jm, ju = joints_mj[jn], joints_u[jn]
        hinge = mujoco.mjtJoint.mjJNT_HINGE.value if hasattr(
            mujoco.mjtJoint, "mjJNT_HINGE") else 3
        ok_type = jm["type"] == hinge and ju["kind"] == "revolute"
        row("type", jn, "hinge", ju["kind"], "-" if ok_type else "mismatch",
            None, "PASS" if ok_type else "FAIL")

        d = axis_angle_diff(np.asarray(jm["axis"], float),
                            np.asarray(ju["axis"], float))
        row("axis", jn, [round(float(x), 6) for x in jm["axis"]],
            [round(float(x), 6) for x in ju["axis"]], d, TOL_AXIS_RAD,
            status_of(d, TOL_AXIS_RAD))

        r_m = np.asarray(jm["range"], float)
        r_u = [ju["limit_lower"], ju["limit_upper"]]
        if None in r_u:
            row("limit", jn, list(r_m), "missing", "missing", TOL_LIMIT_RAD,
                "FAIL", "URDF <limit> absent")
        else:
            r_u = np.asarray(r_u, float)
            d = float(np.max(np.abs(r_m - r_u)))
            row("limit lower", jn, round(float(r_m[0]), 6),
                round(float(r_u[0]), 6), abs(r_m[0] - r_u[0]), TOL_LIMIT_RAD,
                status_of(abs(r_m[0] - r_u[0]), TOL_LIMIT_RAD))
            row("limit upper", jn, round(float(r_m[1]), 6),
                round(float(r_u[1]), 6), abs(r_m[1] - r_u[1]), TOL_LIMIT_RAD,
                status_of(abs(r_m[1] - r_u[1]), TOL_LIMIT_RAD))

        if ju["damping"] is None:
            row("damping", jn, jm["damping"], "missing", "missing",
                TOL_DAMP_REL, "FAIL", "URDF <dynamics damping> absent")
        else:
            dm, du = jm["damping"], float(ju["damping"])
            rel = abs(dm - du) / abs(dm) if dm else abs(dm - du)
            row("damping", jn, dm, du, rel, TOL_DAMP_REL,
                status_of(rel, TOL_DAMP_REL))

        # armature: no URDF concept -> structural FAIL, reported honestly
        row("armature", jn, jm["armature"], "n/a (no URDF concept)",
            "structural", None, "FAIL",
            "URDF 1.0 has no armature semantics; OriEra ArmDescription has "
            "no rotor-inertia field")

        if ju["effort"] is None:
            row("effort", jn, jm["effort"], "missing", "missing",
                TOL_EFFORT_REL, "FAIL", "URDF <limit effort> absent")
        else:
            em, eu = jm["effort"], float(ju["effort"])
            rel = abs(em - eu) / abs(em) if em else abs(em - eu)
            row("effort", jn, em, eu, rel, TOL_EFFORT_REL,
                status_of(rel, TOL_EFFORT_REL))

    # -- per-body rows ------------------------------------------------------
    for name in ["link1", "link2", "link3", "link4", "link5", "link6",
                 "link7", "attachment"]:
        bm = bodies_mj[name]
        ju = child_of.get(name)
        if ju is None:
            row("body pos", name, list(np.round(bm["pos"], 6)), "missing",
                "missing", TOL_LEN_M, "FAIL", "no URDF joint parent found")
        else:
            p_u = np.asarray(ju["origin_xyz"], float)
            d = float(np.max(np.abs(bm["pos"] - p_u)))
            row("body pos", name, list(np.round(bm["pos"], 6)),
                list(np.round(p_u, 6)), d, TOL_LEN_M, status_of(d, TOL_LEN_M))
            r_m = quat_to_mat(bm["quat"])
            r_u = rpy_to_mat(ju["origin_rpy"])
            ang = rot_angle(r_m.T @ r_u)
            row("body rot", name, "quat " + fmt([round(float(x), 7) for x in bm["quat"]]),
                "rpy " + fmt([round(float(x), 7) for x in ju["origin_rpy"]]),
                ang, TOL_ROT_RAD, status_of(ang, TOL_ROT_RAD))

        lu = links_u.get(name)
        in_u = lu.get("inertial") if lu else None

        if name == "attachment":
            same = (bm["mass"] == 0.0 and in_u is None)
            row("mass", name, bm["mass"],
                "none" if in_u is None else in_u["mass"], "-" if same else "mismatch",
                None, "PASS" if same else "FAIL",
                "massless site body on both sides")
            continue

        if in_u is None:
            row("mass", name, bm["mass"], "missing", "missing", TOL_MASS_REL,
                "FAIL", "URDF <inertial> absent")
            continue

        mm, mu = bm["mass"], float(in_u["mass"])
        rel = abs(mm - mu) / mm if mm else abs(mm - mu)
        row("mass", name, mm, mu, rel, TOL_MASS_REL, status_of(rel, TOL_MASS_REL))

        cm, cu = bm["ipos"], np.asarray(in_u["origin_xyz"], float)
        d = float(np.max(np.abs(cm - cu)))
        row("inertial com", name, list(np.round(cm, 6)), list(np.round(cu, 6)),
            d, TOL_LEN_M, status_of(d, TOL_LEN_M))

        full_m = bm["inertia_full"]
        for k, comp in enumerate(("ixx", "iyy", "izz")):
            im, iu = full_m[k, k], float(in_u[comp])
            rel = abs(im - iu) / abs(im) if im else abs(im - iu)
            row(f"inertia {comp}", name, im, iu, rel, TOL_INERTIA_REL,
                status_of(rel, TOL_INERTIA_REL))

    # -- inertia off-diagonals: parsed away by the import chain -------------
    off = []
    for name in ["link1", "link2", "link3", "link4", "link5", "link6", "link7"]:
        f = bodies_mj[name]["inertia_full"]
        off += [f[0, 1], f[0, 2], f[1, 2]]
    off_max = float(np.max(np.abs(off)))
    row("inertia offdiag", "link1..7 (ixy/ixz/iyz)", off_max,
        "n/a (not parsed)", "structural", None, "FAIL",
        "urdf.rs UrdfInertial keeps only ixx/iyy/izz; menagerie fullinertia "
        "has nonzero ixy/ixz/iyz (max |.| = %.3e kg·m²)" % off_max)


# ----------------------------------------------------------------- FK ----


def fk_oriera(dump, qs) -> np.ndarray:
    """URDF FK from the OriEra-parsed chain (origin_xyz/rpy + axis, f32).

    Algorithm: T = prod_k  Trans(origin_xyz_k) · Rz(yaw)Ry(pitch)Rx(roll) ·
    Rot(axis_k, q_k)  (URDF rpy is extrinsic XYZ; rotation about the child
    frame axis). Ends at the fixed `attachment` joint. Recomputed here
    because the Rust side has no FK implementation (noted in the report).
    """
    chain = dump["chain_joints"]
    joints_u = {j["name"]: j for j in dump["joints"]}
    t = np.eye(4)
    for i, jn in enumerate(chain):
        j = joints_u[jn]
        t = t @ _trans(j["origin_xyz"]) @ _rot(rpy_to_mat(j["origin_rpy"])) \
              @ _axis_rot(j["axis"], qs[i])
    j8 = joints_u.get("joint8")
    if j8 is not None and j8["kind"] == "fixed":
        t = t @ _trans(j8["origin_xyz"]) @ _rot(rpy_to_mat(j8["origin_rpy"]))
    return t[:3, 3].copy()


def _trans(xyz) -> np.ndarray:
    t = np.eye(4)
    t[:3, 3] = np.asarray(xyz, float)
    return t


def _rot(r: np.ndarray) -> np.ndarray:
    t = np.eye(4)
    t[:3, :3] = r
    return t


def _axis_rot(axis, q: float) -> np.ndarray:
    a = np.asarray(axis, float)
    n = np.linalg.norm(a)
    if n < 1e-9:
        return np.eye(4)
    a = a / n
    k = np.array([[0, -a[2], a[1]], [a[2], 0, -a[0]], [-a[1], a[0], 0]])
    r = np.eye(3) + math.sin(q) * k + (1 - math.cos(q)) * (k @ k)
    t = np.eye(4)
    t[:3, :3] = r
    return t


def fk_check(model, data, site_id, dump) -> list[dict]:
    rng = np.random.default_rng(SEED)
    out = []
    qs_list = []
    for _ in range(N_FK_SAMPLES):
        qs = []
        for k in range(1, CHAIN_LEN + 1):
            lo, hi = model.jnt_range[mujoco.mj_name2id(
                model, mujoco.mjtObj.mjOBJ_JOINT, f"joint{k}")]
            span = hi - lo
            qs.append(rng.uniform(lo + 0.05 * span, hi - 0.05 * span))
        qs_list.append(qs)

        for k, q in enumerate(qs, start=1):
            adr = model.jnt_qposadr[mujoco.mj_name2id(
                model, mujoco.mjtObj.mjOBJ_JOINT, f"joint{k}")]
            data.qpos[adr] = q
        mujoco.mj_forward(model, data)
        p_m = data.site_xpos[site_id].copy()

        p_u = fk_oriera(dump, qs)
        err = float(np.linalg.norm(p_m - p_u))
        out.append({
            "sample": _,
            "q": [round(float(x), 6) for x in qs],
            "mujoco_xpos": [round(float(x), 6) for x in p_m],
            "oriera_fk": [round(float(x), 6) for x in p_u],
            "err_mm": round(err * 1000.0, 4),
            "status": "PASS" if err <= TOL_FK_M else "FAIL",
        })
    return out


# ---------------------------------------------------------------- main ----


def main() -> int:
    log(f"== MJCF Panda cross-check (mujoco {mujoco.__version__}) ==")
    OUT.mkdir(parents=True, exist_ok=True)
    rev = ensure_vendor()
    log(f"[mjcf] {MJCF} (menagerie rev {rev}, {MENAGERIE_LICENSE})")

    URDF.write_text(URDF_TEMPLATE, encoding="utf-8")
    log(f"[urdf] converted reference written -> {URDF}")

    log("[rust] dumping OriEra URDF chain (cargo run --example urdf_chain_dump)")
    dump = run_rust_dump(URDF)
    log(f"[rust] root={dump['root']} chain={dump['chain_joints']} "
        f"validate_chain={dump['validate_chain']}")

    model, data, bodies_mj, joints_mj, site_id = extract_mujoco(MJCF)
    log(f"[mjcf] model loaded: {model.nbody} bodies, {model.njnt} joints, "
        f"{model.nu} actuators")

    compare_params(bodies_mj, joints_mj, dump)
    fk_rows = fk_check(model, data, site_id, dump)
    for f in fk_rows:
        row("FK end-effector", f"sample {f['sample']}",
            list(f["mujoco_xpos"]), list(f["oriera_fk"]),
            f["err_mm"] / 1000.0, TOL_FK_M, f["status"], "err_mm=%s" % f["err_mm"])

    # OriEra ArmDescription reduction (informational; structural finding)
    arm = dump.get("arm_description")

    n_pass = sum(1 for r in ROWS if r["status"] == "PASS")
    n_tol = sum(1 for r in ROWS if r["status"] == "TOLERANCE")
    n_fail = sum(1 for r in ROWS if r["status"] == "FAIL")
    verdict = "PASS" if n_fail == 0 else "FAIL"

    # ---- console table ----------------------------------------------------
    log("")
    log(f"{'item':<18} {'element':<20} {'MuJoCo':<28} {'OriEra':<28} "
        f"{'diff':>10}  {'status':<9}")
    log("-" * 122)
    for r in ROWS:
        log(f"{r['item']:<18} {r['element']:<20} {str(r['mjcf_value']):<28} "
            f"{str(r['oriera_value']):<28} {fmt(r['diff']):>10}  {r['status']:<9}")
    log("-" * 122)
    fk_errs = [f["err_mm"] for f in fk_rows]
    log(f"FK spot-check: {len(fk_rows)} random configs, err_mm "
        f"min={min(fk_errs)} max={max(fk_errs)} (<= 5.0 mm)")
    if arm:
        log(f"ArmDescription reduction (informational): links={arm['links']} "
            f"limits={arm['limits']} passive_damping={arm['passive_damping']} "
            f"(7 DoF arm mapped onto 4-DoF planar stages — see doc appendix)")
    log("")

    report = {
        "meta": {
            "generated_utc": datetime.now(timezone.utc).isoformat(),
            "mujoco_version": mujoco.__version__,
            "seed": SEED,
            "mjcf_source": {
                "path": str(MJCF),
                "url": f"{MENAGERIE_URL}/tree/main/{MENAGERIE_DIR}",
                "revision": rev,
                "license": MENAGERIE_LICENSE,
            },
            "urdf_source": {
                "path": str(URDF),
                "kind": "converted-ref (换算参照)",
                "provenance": "7-joint minimal URDF converted 1:1 from the "
                              "menagerie panda_nohand.xml listed above",
            },
            "rust_probe": "cargo run --example urdf_chain_dump -p oriera-agent --features urdf",
            "fk_algorithm": "URDF FK from urdf_chain_dump output: T = prod "
                            "Trans(origin_xyz)·Rz(y)Ry(p)Rx(r)·Rot(axis,q); "
                            "rpy extrinsic XYZ; ends at fixed attachment "
                            "joint. Hand-computed in this script because the "
                            "Rust side has no FK implementation.",
            "tolerances": {
                "limit_rad": TOL_LIMIT_RAD, "mass_rel": TOL_MASS_REL,
                "length_m": TOL_LEN_M, "damping_rel": TOL_DAMP_REL,
                "inertia_rel": TOL_INERTIA_REL, "effort_rel": TOL_EFFORT_REL,
                "fk_m": TOL_FK_M,
            },
            "status_rule": "PASS <= tol < TOLERANCE <= 2*tol < FAIL; "
                           "structural absences (armature, inertia "
                           "off-diagonals) are recorded as FAIL",
        },
        "validate_chain_report": dump["validate_chain"],
        "skipped": dump["skipped"],
        "params": ROWS,
        "fk": fk_rows,
        "arm_description_reduction": arm,
        "summary": {"pass": n_pass, "tolerance": n_tol, "fail": n_fail,
                    "total": len(ROWS), "verdict": verdict},
    }
    REPORT.write_text(json.dumps(report, indent=2, ensure_ascii=False),
                      encoding="utf-8")
    log(f"[report] {REPORT}")

    log(f"SUMMARY: PASS={n_pass} TOLERANCE={n_tol} FAIL={n_fail} "
        f"TOTAL={len(ROWS)} -> VERDICT: {verdict}")
    return 0 if n_fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
