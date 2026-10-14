//! Web entry — Wasm + WebGPU. Called from JS via wasm-bindgen.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn boot() -> String {
    let profile = oriera_pal::detect_platform();
    format!("{:?}", profile)
}
