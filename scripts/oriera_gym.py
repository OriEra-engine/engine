#!/usr/bin/env python3
"""Gymnasium wrapper over the `oriera_sim` PyO3 bridge — the training
loop's standard-plug entry (PLAN-embodied-sim M1 #1 tail).

    import sys; sys.path.insert(0, "target/debug")
    from oriera_gym import OrieraEnv, OrieraPickPlaceEnv
    env = OrieraPickPlaceEnv(task="pick_place", kp_jitter=0.2, mass_jitter=0.15)
    obs, info = env.reset(seed=1000)
    obs, r, term, trunc, info = env.step(env.action_space.sample())

    # M0a：实验即文件 —— spec 驱动（schema oriera.scene/2，v1 自动迁移）
    env = OrieraEnv(spec="crates/oriera-agent/specs/tabletop-4dof-pick.json")
    obs, info = env.reset(seed=1000)

Action: float32[5] in [-1, 1] → 4 joint-angle targets (× action_scale，
默认 [2,2,2,2] rad，来自 spec 的 robot.action_scale / control) + gripper.
Obs:    float32[21] (see engine embodied_env obs_names / info["names"]).
"""
from __future__ import annotations

import sys
from pathlib import Path

import gymnasium as gym
import numpy as np


TASK_ALIASES = {"pick": "pick_place", "pickplace": "pick_place"}


def _load_oriera_sim() -> "object":
    # release 优先（RL 实测 8956 vs debug 118 steps/s——75×差距），
    # debug 仅作本地调试兜底。
    base = Path(__file__).resolve().parent.parent / "target"
    for profile in ("release", "debug"):
        target = base / profile
        if (target / "oriera_sim.pyd").exists() or (target / "oriera_sim.so").exists():
            break
    if str(target) not in sys.path:
        sys.path.insert(0, str(target))
    import oriera_sim  # the cdylib extension module

    return oriera_sim


class OrieraEnv(gym.Env):
    """M0a spec-driven env：`spec=<json 路径>` 或 `task=<族名>` 二选一。

    spec 路径：实验身份 = spec_hash（引擎自动算，随 receipt 落盘）；
    v1 草案（旧 pick_place.json）自动迁移，行为逐位不变。
    动作尺度从 spec 透传（action_scale），不再硬编码 ×2.0。
    """

    metadata = {"render_modes": []}

    def __init__(self, spec: str | None = None, task: str | None = None,
                 kp_jitter: float = 0.0, mass_jitter: float = 0.0,
                 obs_noise: float = 0.0):
        oriera_sim = _load_oriera_sim()
        if spec is not None:
            if task is not None:
                raise ValueError("spec 与 task 二选一（spec 已带 task.id）")
            self._env = oriera_sim.PickPlaceEnv.from_spec(spec)
        else:
            task = TASK_ALIASES.get(task or "pick_place", task or "pick_place")
            self._env = oriera_sim.PickPlaceEnv(
                task, 0, kp_jitter, mass_jitter, obs_noise)
        self._seed_series = 1000  # reset(seed=None) 时的确定性序列
        n_obs = len(self._env.obs_names)
        n_act = len(self._env.action_names)
        self.observation_space = gym.spaces.Box(
            -np.inf, np.inf, shape=(n_obs,), dtype=np.float32)
        self.action_space = gym.spaces.Box(
            -1.0, 1.0, shape=(n_act,), dtype=np.float32)
        # 动作尺度透传（U-10）：±1 → ±action_scale rad 关节目标
        self._action_scale = np.asarray(self._env.action_scale, dtype=np.float32)
        self._n_joints = n_act - 1

    def reset(self, *, seed: int | None = None, options=None):
        # Gymnasium 惯例：seed=None 延续序列（不复位同一集）；显式 seed
        # 完全决定该集（指纹回执可复现）。
        super().reset(seed=seed)
        if seed is None:
            seed = self._seed_series
        self._seed_series = seed + 1
        obs = self._env.reset(seed)
        return np.asarray(obs, dtype=np.float32), {
            "task": self._env.task_phrase,
            "randomization": self._env.randomization,
        }

    def step(self, action):
        a = np.clip(np.asarray(action, dtype=np.float32), -1.0, 1.0)
        cmd = np.empty(self._n_joints + 1, dtype=np.float32)
        cmd[: self._n_joints] = a[: self._n_joints] * self._action_scale
        cmd[self._n_joints] = (a[self._n_joints] + 1.0) / 2.0  # ±1 → 0..1
        obs, r, term, trunc, succ = self._env.step(cmd.tolist())
        return (np.asarray(obs, dtype=np.float32), float(r),
                bool(term), bool(trunc), {"success": bool(succ)})

    def close(self):
        pass


# 旧类名保留（既有作业/训练脚本零改动）——即默认 pick_place 任务的
# spec 驱动 env 的别名层。
OrieraPickPlaceEnv = OrieraEnv


# gym.make 标准入口（注册表 + max_steps 700 语义已在 env 内）
gym.register(
    id="OrieraPickPlace-v0",
    entry_point="oriera_gym:OrieraPickPlaceEnv",
    kwargs={"task": "pick_place"},
)
