#!/usr/bin/env python3
"""Gymnasium wrapper over the `oriera_sim` PyO3 bridge — the training
loop's standard-plug entry (PLAN-embodied-sim M1 #1 tail).

    import sys; sys.path.insert(0, "target/debug")
    from oriera_gym import OrieraPickPlaceEnv
    env = OrieraPickPlaceEnv(task="pick_place", kp_jitter=0.2, mass_jitter=0.15)
    obs, info = env.reset(seed=1000)
    obs, r, term, trunc, info = env.step(env.action_space.sample())

Action: float32[5] in [-1, 1] → 4 joint-angle targets (×2 rad) + gripper.
Obs:    float32[21] (see engine embodied_env OBS_NAMES / info["names"]).
"""
from __future__ import annotations

import sys
from pathlib import Path

import gymnasium as gym
import numpy as np


TASK_ALIASES = {"pick": "pick_place", "pickplace": "pick_place"}


class OrieraPickPlaceEnv(gym.Env):
    metadata = {"render_modes": []}

    def __init__(self, task: str = "pick_place", kp_jitter: float = 0.0,
                 mass_jitter: float = 0.0, obs_noise: float = 0.0):
        target = Path(__file__).resolve().parent.parent / "target/debug"
        if str(target) not in sys.path:
            sys.path.insert(0, str(target))
        import oriera_sim  # the cdylib extension module

        task = TASK_ALIASES.get(task, task)  # "pick" → "pick_place"
        self._env = oriera_sim.PickPlaceEnv(
            task, 0, kp_jitter, mass_jitter, obs_noise)
        self.observation_space = gym.spaces.Box(
            -np.inf, np.inf, shape=(21,), dtype=np.float32)
        self.action_space = gym.spaces.Box(
            -1.0, 1.0, shape=(5,), dtype=np.float32)

    def reset(self, *, seed: int | None = None, options=None):
        super().reset(seed=seed)
        obs = self._env.reset(seed if seed is not None else 1000)
        return np.asarray(obs, dtype=np.float32), {
            "task": self._env.task_phrase,
            "randomization": self._env.randomization,
        }

    def step(self, action):
        a = np.clip(np.asarray(action, dtype=np.float32), -1.0, 1.0)
        cmd = np.empty(5, dtype=np.float32)
        cmd[:4] = a[:4] * 2.0          # ±1 → ±2 rad joint targets
        cmd[4] = (a[4] + 1.0) / 2.0    # ±1 → gripper 0..1
        obs, r, term, trunc, succ = self._env.step(cmd.tolist())
        return (np.asarray(obs, dtype=np.float32), float(r),
                bool(term), bool(trunc), {"success": bool(succ)})

    def close(self):
        pass


# gym.make 标准入口（注册表 + max_steps 700 语义已在 env 内）
gym.register(
    id="OrieraPickPlace-v0",
    entry_point="oriera_gym:OrieraPickPlaceEnv",
    kwargs={"task": "pick_place"},
)
