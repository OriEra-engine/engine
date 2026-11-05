"""recipe_subproc_vecenv.py — SubprocVecEnv×4 并行配方（PLAN-embodied-general M1 吞吐行）。

用 SB3 的 `make_vec_env(..., vec_env_cls=SubprocVecEnv)` 起 4 个进程，各跑
`--steps`（默认 2000）步随机动作，打印聚合吞吐，并顺手跑一遍单进程基线
作对照（LAB 入册单进程参考值：13,458 steps/s，release pyd）。

    python scripts/recipe_subproc_vecenv.py [--n 4] [--steps 2000]
        [--spec crates/oriera-embodied/specs/tabletop-4dof-pick.json]

前置：`target/release/oriera_sim.pyd`（`cargo build --release -p oriera-agent
--features python` + dll→pyd，见 docs/LAB-GUIDE.md）或已 `pip install dist/*.whl`。
Windows 下 SubprocVecEnv 走 spawn，本文件必须有 `if __name__ == "__main__"` 守卫
（子进程会重新 import 本模块）。
"""
from __future__ import annotations

import argparse
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
# 子进程（spawn）重新执行模块级代码：这里把 scripts/ 挂上，OrieraEnv 才能按名 import。
if str(REPO / "scripts") not in sys.path:
    sys.path.insert(0, str(REPO / "scripts"))

import numpy as np  # noqa: E402

from oriera_gym import OrieraEnv  # noqa: E402


def run_vec(spec: str, n: int, steps: int, kind: str = "subproc") -> dict:
    from stable_baselines3.common.env_util import make_vec_env
    from stable_baselines3.common.vec_env import DummyVecEnv, SubprocVecEnv

    if kind == "subproc":
        venv = make_vec_env(
            OrieraEnv,
            n_envs=n,
            seed=1000,
            env_kwargs={"spec": spec},
            vec_env_cls=SubprocVecEnv,
            vec_env_kwargs={"start_method": "spawn"},
        )
    else:
        venv = make_vec_env(OrieraEnv, n_envs=n, seed=1000, env_kwargs={"spec": spec}, vec_env_cls=DummyVecEnv)
    obs = venv.reset()
    assert obs.shape == (n, venv.observation_space.shape[0]), obs.shape
    rng = np.random.default_rng(0)
    t0 = time.perf_counter()
    done_count = 0
    for _ in range(steps):
        actions = rng.uniform(-1.0, 1.0, size=(n, venv.action_space.shape[0])).astype(np.float32)
        obs, rew, dones, infos = venv.step(actions)  # SubprocVecEnv 自动 reset 终止的子环境
        done_count += int(dones.sum())
    dt = time.perf_counter() - t0
    venv.close()
    total = n * steps
    return {"kind": kind, "n": n, "steps_per_env": steps, "total_steps": total, "seconds": dt,
            "steps_per_s": total / dt, "episodes_ended": done_count}


def run_single(spec: str, steps: int) -> dict:
    env = OrieraEnv(spec=spec)
    env.reset(seed=1000)
    rng = np.random.default_rng(0)
    t0 = time.perf_counter()
    for _ in range(steps):
        a = rng.uniform(-1.0, 1.0, size=env.action_space.shape).astype(np.float32)
        _obs, _r, term, trunc, _info = env.step(a)
        if term or trunc:
            env.reset()
    dt = time.perf_counter() - t0
    return {"total_steps": steps, "seconds": dt, "steps_per_s": steps / dt}


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--n", type=int, default=4, help="并行进程数（默认 4）")
    ap.add_argument("--steps", type=int, default=2000, help="每进程步数（默认 2000）")
    ap.add_argument("--spec", default=str(REPO / "crates/oriera-embodied/specs/tabletop-4dof-pick.json"))
    ap.add_argument("--skip-single", action="store_true", help="不跑单进程对照")
    ap.add_argument("--skip-dummy", action="store_true", help="不跑 DummyVecEnv（同进程向量化）对照")
    args = ap.parse_args()
    spec = str(Path(args.spec).resolve())
    print(f"# recipe_subproc_vecenv · spec={Path(spec).name} · n={args.n} · steps/env={args.steps}")

    single = None if args.skip_single else run_single(spec, args.steps)
    if single:
        print(f"# 单进程基线: {single['total_steps']} 步 / {single['seconds']:.2f}s = {single['steps_per_s']:,.0f} steps/s"
              f"（LAB 入册参考 13,458 steps/s，release pyd）")

    vec = run_vec(spec, args.n, args.steps, "subproc")
    print(f"# SubprocVecEnv×{vec['n']}: {vec['total_steps']} 步 / {vec['seconds']:.2f}s = "
          f"{vec['steps_per_s']:,.0f} steps/s 聚合 · 子环境回合结束 {vec['episodes_ended']} 次")
    if single:
        print(f"# 加速比: {vec['steps_per_s'] / single['steps_per_s']:.2f}×（{vec['n']} 进程；"
              f"进程间 IPC 每步一次 pickle 往返，是小步长环境的主开销）")
    if not args.skip_dummy:
        dummy = run_vec(spec, args.n, args.steps, "dummy")
        print(f"# DummyVecEnv×{dummy['n']}（同进程向量化对照）: {dummy['total_steps']} 步 / {dummy['seconds']:.2f}s = "
              f"{dummy['steps_per_s']:,.0f} steps/s 聚合")


if __name__ == "__main__":
    main()
