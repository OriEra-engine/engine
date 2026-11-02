/* tslint:disable */
/* eslint-disable */

/**
 * The simulator handle. Build with [`OrieraSim::from_spec_json`].
 */
export class OrieraSim {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Measured joint angles (rad).
     */
    angles(): Float32Array;
    /**
     * The collector's pick_place scripted baseline (f32-exact mirror
     * of `examples/embodied_sim_collect.rs`) for control step `step`,
     * read off the last observation. Empty array when not applicable
     * (non-4-DoF arm / other task) — the caller keeps its own policy.
     */
    baseline_action(step: number): Float32Array;
    cube_pos(): Float32Array;
    dof(): number;
    dt(): number;
    ee_pos(): Float32Array;
    /**
     * Per-episode state fingerprint (#67), as a decimal `u64` string
     * (JS numbers cannot hold it exactly).
     */
    fingerprint(): string;
    /**
     * Same receipt, 16 hex digits.
     */
    fingerprint_hex(): string;
    /**
     * Build from `oriera.scene/2` JSON text (a v1 draft migrates on the
     * fly). Validation failures surface as a JS `Error` carrying the
     * engine's field-level message.
     */
    static from_spec_json(json: string): OrieraSim;
    /**
     * Current gripper plate gap (m) — the ramped value, bit-exact with
     * the engine's `MultibodySystem::gap_now` (M1 leftover #2 closed;
     * read-only).
     */
    gap(): number;
    goal(): Float32Array;
    gripper(): number;
    holding(): boolean;
    /**
     * Observation/action layout JSON (names, slices, dims, dof, dt,
     * max_steps).
     */
    layout(): string;
    max_steps(): number;
    /**
     * Whether the input was a v1 draft migrated on load.
     */
    migrated(): boolean;
    /**
     * The last observation (after `reset` or `step`).
     */
    obs(): Float32Array;
    /**
     * Every pose the renderer places, as JSON (links, TCP, plates,
     * prop, goal, flags) — see `SimCore::poses_json`.
     */
    poses(): string;
    /**
     * The engine's default experiment (`tabletop-4dof-pick`, pick_place)
     * as spec JSON — the demo's preset; `from_spec_json(preset_spec_json())`
     * is the same env as `TableTopEnv::new()`.
     */
    static preset_spec_json(): string;
    randomization(): string;
    /**
     * Reset to a fresh episode. `seed` is a JS number (exact to 2^53;
     * the collector's seeds are small integers).
     */
    reset(seed: number): Float32Array;
    /**
     * Canonical spec hash (16 hex digits) — the experiment's identity.
     */
    spec_hash(): string;
    /**
     * The resolved spec as JSON — the renderer builds its scene from it.
     */
    spec_json(): string;
    /**
     * Advance one control step with `[tgt_j0..tgt_jN-1, gripper]`.
     */
    step(action: Float32Array): StepResult;
    /**
     * Control steps since the last reset.
     */
    steps(): number;
    task_id(): string;
    task_phrase(): string;
    /**
     * Engine ticks since the last reset (includes the settle ticks).
     */
    tick(): number;
}

/**
 * One `step()` outcome — the Gymnasium tuple plus `success`.
 */
export class StepResult {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    readonly obs: Float32Array;
    readonly reward: number;
    readonly success: boolean;
    readonly terminated: boolean;
    readonly truncated: boolean;
}

/**
 * Route Rust panics to `console.error` with the message (otherwise the
 * browser only sees `RuntimeError: unreachable`). No extra crate — a
 * plain wasm-bindgen import.
 */
export function __oriera_wasm_init(): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __oriera_wasm_init: () => void;
    readonly __wbg_orierasim_free: (a: number, b: number) => void;
    readonly __wbg_stepresult_free: (a: number, b: number) => void;
    readonly orierasim_angles: (a: number, b: number) => void;
    readonly orierasim_baseline_action: (a: number, b: number, c: number) => void;
    readonly orierasim_cube_pos: (a: number, b: number) => void;
    readonly orierasim_dof: (a: number) => number;
    readonly orierasim_dt: (a: number) => number;
    readonly orierasim_ee_pos: (a: number, b: number) => void;
    readonly orierasim_fingerprint: (a: number, b: number) => void;
    readonly orierasim_fingerprint_hex: (a: number, b: number) => void;
    readonly orierasim_from_spec_json: (a: number, b: number, c: number) => void;
    readonly orierasim_gap: (a: number) => number;
    readonly orierasim_goal: (a: number, b: number) => void;
    readonly orierasim_gripper: (a: number) => number;
    readonly orierasim_holding: (a: number) => number;
    readonly orierasim_layout: (a: number, b: number) => void;
    readonly orierasim_max_steps: (a: number) => number;
    readonly orierasim_migrated: (a: number) => number;
    readonly orierasim_obs: (a: number, b: number) => void;
    readonly orierasim_poses: (a: number, b: number) => void;
    readonly orierasim_preset_spec_json: (a: number) => void;
    readonly orierasim_randomization: (a: number, b: number) => void;
    readonly orierasim_reset: (a: number, b: number, c: number) => void;
    readonly orierasim_spec_hash: (a: number, b: number) => void;
    readonly orierasim_spec_json: (a: number, b: number) => void;
    readonly orierasim_step: (a: number, b: number, c: number) => number;
    readonly orierasim_steps: (a: number) => number;
    readonly orierasim_task_id: (a: number, b: number) => void;
    readonly orierasim_task_phrase: (a: number, b: number) => void;
    readonly orierasim_tick: (a: number) => number;
    readonly stepresult_obs: (a: number, b: number) => void;
    readonly stepresult_reward: (a: number) => number;
    readonly stepresult_success: (a: number) => number;
    readonly stepresult_terminated: (a: number) => number;
    readonly stepresult_truncated: (a: number) => number;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export2: (a: number, b: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
