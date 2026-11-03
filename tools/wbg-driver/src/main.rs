//! wbg-driver — wasm-bindgen glue 生成器（CLI 装不上时的等价路径，
//! 见 docs/WASM-RENDERER.md §2）。产物=官方 wasm-bindgen web glue。
use anyhow::{bail, Context, Result};
use wasm_bindgen_cli_support::Bindgen;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().context("用法: wbg-driver <input.wasm> <out-dir> [--out-name N]")?;
    let out_dir = args.next().context("缺 <out-dir>")?;
    let mut out_name = String::from("oriera_wasm");
    while let Some(a) = args.next() {
        if a == "--out-name" {
            out_name = args.next().context("--out-name 缺值")?;
        } else if a == "web" || a == "--typescript" || a == "--target" {
            // 兼容官方 CLI 风格的摆设参数（本驱动只产 web ESM）
            if a == "--target" {
                args.next();
            }
        } else {
            bail!("未知参数 {a}");
        }
    }

    // builder 方法可失败（返回 Result），链式不可用——逐句 ？。
    let mut b = Bindgen::new();
    b.typescript(true);
    b.omit_default_module_path(false);
    b.input_path(&input);
    b.out_name(&out_name);
    b.remove_name_section(true);
    b.remove_producers_section(true);
    b.web(true).context("web target")?;
    b.generate(&out_dir).context("bindgen generate 失败")?;
    println!("glue 写入 {out_dir}/（web ESM + typescript，out-name={out_name}）");
    Ok(())
}
