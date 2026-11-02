#!/bin/bash
# M0b 批量扫参示例（PLAN-embodied-general §9.1 P-5：给 slurm array 示例，不自建调度器）。
#
# 这是一份文档件：在没有 slurm 的环境（Windows 工作站 / 单机）它只是说明
# "一个 array task = 一个 (spec, seed) 组合 = 一个 runs/<hash8>/<method>/<seed>/ 目录"
# 的映射；有 slurm 时直接 `sbatch scripts/batch_example.slurm`。
#
# 断点续跑：引擎看到目标目录已有 eval_report.json 就打印「↷ 断点续跑」退出 0，
# 所以 array 任务被抢占/重排后原样重提交即可，不会重复算也不会互相覆盖
# （目录由 spec_hash8 + 协议 hash8 + seed 唯一决定）。
#
# 300 配置扫参 = 例如 30 个 lab spec × 10 个 seed：把 SPECS 列表和 SEEDS 改成
# 你的，--array=0-299 即可（%8 限制同时跑 8 个）。
#
#SBATCH --job-name=oriera-sweep
#SBATCH --array=0-5%4
#SBATCH --cpus-per-task=1
#SBATCH --mem=2G
#SBATCH --time=00:30:00
#SBATCH --output=runs/_logs/slurm-%A_%a.out

set -euo pipefail
cd "${SLURM_SUBMIT_DIR:-$(dirname "$0")/..}"

# ---- 扫参网格（改这里）----------------------------------------------------
SPECS=(
  crates/oriera-agent/specs/official/tabletop-4dof-pick.json
  crates/oriera-agent/specs/lab/example-cylinder.json
)
SEEDS=(1000 2000 3000)
PROTOCOL="${PROTOCOL:-paper@v1}"      # paper@v1 只记录不拦；课程口径用 course@v1
EPISODES="${EPISODES:-100}"            # paper@v1 下限 100
RUNS_ROOT="${RUNS_ROOT:-runs}"
# -------------------------------------------------------------------------

N_SEEDS=${#SEEDS[@]}
IDX=${SLURM_ARRAY_TASK_ID:-0}
SPEC=${SPECS[$((IDX / N_SEEDS))]}
SEED=${SEEDS[$((IDX % N_SEEDS))]}

# 预编译一次（登录节点 `cargo build --release ...`），array 任务只跑二进制——
# 不要让 300 个任务各自 cargo build。
EXE="${EXE:-target/release/examples/embodied_sim_collect}"
if [[ ! -x "$EXE" ]]; then
  echo "缺 $EXE —— 先在登录节点：cargo build --release -p oriera-agent --example embodied_sim_collect --features rigid3d,cam,spec,urdf" >&2
  exit 3
fi

mkdir -p "$RUNS_ROOT/_logs"
echo "[array $IDX] spec=$SPEC seed=$SEED protocol=$PROTOCOL episodes=$EPISODES → $RUNS_ROOT/<hash8>/scripted-p<proto8>/$SEED/"
exec "$EXE" \
  --spec "$SPEC" --seed "$SEED" --runs-root "$RUNS_ROOT" \
  --protocol "$PROTOCOL" --episodes "$EPISODES" \
  --verify-determinism off --gate report --no-cam
