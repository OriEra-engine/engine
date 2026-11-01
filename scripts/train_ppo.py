#!/usr/bin/env python3
"""SB3 PPO 训练器——OriEra 具身仿真台 RL 路线（LAB-GUIDE §5 的可复跑版）。

reach 100k 的闭环验证（a1797c5）用的是内联脚本，未入库不可复跑；本
脚本把训练-评测协议固化：固定评测 seed 1000-1009、按期评测、产出
eval 曲线 JSON + SB3 模型，双跑同 seed 结果可对账。

    python scripts/train_ppo.py --task pick_place --steps 100000
    python scripts/train_ppo.py --task reach --steps 100000 --dr

依赖：pip install stable-baselines3 numpy（cdylib 见 oriera_gym 头注）。
Windows 注意：python feature 的 cdylib 测试要 --test-threads=1，本脚本
单进程无涉。
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

import numpy as np

OUT_DEFAULT = Path("out/rl-training")


def make_env(args):
    from oriera_gym import OrieraPickPlaceEnv
    return OrieraPickPlaceEnv(
        task=args.task,
        kp_jitter=0.2 if args.dr else 0.0,
        mass_jitter=0.15 if args.dr else 0.0,
        obs_noise=0.01 if args.dr else 0.0,
    )


def evaluate(model, args, n_episodes=12) -> dict:
    """固定 seed 1000-1011 评测——与批量 CLI 验收带/LAB-GUIDE 对齐，可跨期对账。"""
    env = make_env(args)
    rows = []
    for seed in range(1000, 1000 + n_episodes):
        obs, _ = env.reset(seed=seed)
        total, done = 0.0, False
        while not done:
            action, _ = model.predict(obs, deterministic=True)
            obs, r, term, trunc, info = env.step(action)
            total += float(r)
            done = term or trunc
        rows.append({"seed": seed, "return": total, "success": bool(info.get("success", False))})
    env.close()
    rets = [r["return"] for r in rows]
    succ = sum(r["success"] for r in rows)
    return {
        "mean_return": float(np.mean(rets)),
        "success_rate": succ / len(rows),
        "episodes": rows,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--task", default="pick_place", choices=["reach", "pick_place", "push"])
    ap.add_argument("--steps", type=int, default=100_000)
    ap.add_argument("--dr", action="store_true", help="域随机化三旋钮（kp/mass/obs_noise）")
    ap.add_argument("--eval-every", type=int, default=20_000)
    ap.add_argument("--out", type=Path, default=None)
    args = ap.parse_args()
    args.out = args.out or (OUT_DEFAULT / f"{args.task}{'-dr' if args.dr else ''}-{args.steps}")

    from stable_baselines3 import PPO

    args.out.mkdir(parents=True, exist_ok=True)
    t0 = time.time()

    curve = []
    env = make_env(args)
    model = PPO("MlpPolicy", env, verbose=0, seed=7)
    curve.append({"step": 0, **evaluate(model, args)})

    for checkpoint in range(args.eval_every, args.steps + 1, args.eval_every):
        model.learn(total_timesteps=args.eval_every, reset_num_timesteps=False)
        row = {"step": checkpoint, "wall_s": round(time.time() - t0, 1),
               **evaluate(model, args)}
        curve.append(row)
        print(f"[{checkpoint:>7} steps] r̄={row['mean_return']:.1f} "
              f"succ={row['success_rate']:.0%} wall={row['wall_s']}s", flush=True)

    model.save(args.out / "ppo_model")
    (args.out / "eval_curve.json").write_text(
        json.dumps({"task": args.task, "steps": args.steps, "dr": args.dr,
                    "eval_seeds": "1000-1009", "curve": curve}, indent=1))
    env.close()
    print(f"done → {args.out} (wall {time.time() - t0:.0f}s)")


if __name__ == "__main__":
    main()
