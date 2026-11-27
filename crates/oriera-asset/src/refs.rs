//! L4 资产引用图(issue #206)——真实场景批次 WP9:可机读的资产需求表 +
//! 预检完整性报告。
//!
//! 职责:回答"这个世界需要哪些资产、缺不缺"。分享/发行一条世界时,
//! 把世界的资产需求([`AssetManifest`])随包携带,接收方对自己的"磁盘"
//! 做预检([`preflight`]),得到确定性顺序的缺失/重名报告([`RefReport`]);
//! 分享方在打包前也可以用同一函数自查。JSON 往返([`AssetManifest::to_json`]
//! / [`AssetManifest::from_json`])让需求表成为可 diff、可校验和的随包
//! 文件,风格对齐 memory-json.v1。
//!
//! # 契约
//! - 纯 std + serde,wasm32 可编译:本模块**没有任何** `std::fs`/环境
//!   访问——"这个 uri 在不在"由调用方注入的 `resolver(uri) -> bool`
//!   回答:主机端传真盘探测,wasm 传内核 VFS,测试传 `HashMap`/`Vec`
//!   假盘;
//! - 确定性:报告顺序只取决于输入——`missing` 按引用输入序(每条解析
//!   失败的引用各记一次,同一 uri 可能重复出现),`duplicate_names` 按
//!   名字首现序(每个重名只记一次);同一 manifest 的 `to_json` 恒输出
//!   逐位相同的字符串(字段序固定),`preflight` 双跑逐位一致;
//! - `from_json` Result-based:任何坏输入都是 `Err`,绝不 panic,调用方
//!   保留旧表继续跑(自愈哲学,同 memory-json);
//! - 纯加法:不动 [`crate::AssetStore`],既有公共签名零变化;不带需求
//!   表的世界行为与现版本逐位一致。
//!
//! # 诚实边界
//! - 本模块**不计算哈希**:[`AssetRef::sha256`] 只是随表登记的期望值
//!   (十六进制小写约定),[`sha256_matches`] 把它与调用方自行对真实
//!   字节算出的摘要做大小写不敏感比对——本模块不接触任何资产字节,
//!   也不校验字符串是否合法十六进制;
//! - 本模块**不解析任何资产格式**(glb/png/ogg…),解码归兄弟模块
//!   ([`crate::glb`] 等);
//! - `resolver` 只回答"存在与否",不校验内容;字节级完整性由
//!   [`sha256_matches`] 补足——两者都通过才算真正"齐了"。

use serde::{Deserialize, Serialize};

/// 需求表的 JSON 格式版本(对齐 memory-json.v1 的 `version` 槽位约定)。
const SCHEMA_VERSION: u32 = 1;

/// serde `default` 钩子:`from_json` 容忍 `version` 缺省,视为当前版本。
fn schema_version() -> u32 {
    SCHEMA_VERSION
}

/// 资产大类。JSON 里序列化为小写单词(`"mesh"`、`"texture"`…),未知
/// 类别落到 [`AssetKind::Other`],保证需求表永远装得下新类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// 几何网格(glb/gltf/ply…)。
    Mesh,
    /// 贴图(png/jpg/ktx…)。
    Texture,
    /// 音频(ogg/wav…)。
    Audio,
    /// 动画/音频片段(clip)。
    Clip,
    /// 地形数据(高度场/tiling…)。
    Terrain,
    /// 其它/未分类——兜底,让新格式不必先改枚举也能登记。
    Other,
}

/// 一条资产需求(引用)。字段是记录本体,公开可直构;[`AssetRef::new`]
/// / [`AssetRef::with_sha256`] 是便捷构造。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    /// 逻辑名(世界内的稳定称呼,如 `hero_mesh`)。重名不报错——是否
    /// 属于冗余由 [`RefReport::duplicate_names`] 报告,由人裁决。
    pub name: String,
    /// 资产大类。
    pub kind: AssetKind,
    /// 资源标识(相对包内路径或 URI),原样交给 resolver,本模块不解释。
    pub uri: String,
    /// 登记的期望 SHA-256,十六进制小写约定;`None` 表示"未承诺摘要"。
    /// 本模块不计算哈希,只在 [`sha256_matches`] 与调用方自算值比对。
    pub sha256: Option<String>,
}

impl AssetRef {
    /// 无摘要登记的便捷构造(`sha256 = None`)。
    pub fn new(name: impl Into<String>, kind: AssetKind, uri: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind,
            uri: uri.into(),
            sha256: None,
        }
    }

    /// 链式登记期望摘要(十六进制小写约定,不做字符集校验)。
    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.sha256 = Some(sha256.into());
        self
    }
}

/// 可机读的资产需求表——"这个世界需要哪些资产"的完整答案。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetManifest {
    /// 格式版本,恒为 1(`new`/`from_json` 归一化;反序列化时其它值
    /// 被 [`AssetManifest::from_json`] 拒绝)。JSON 输出的首个字段。
    #[serde(default = "schema_version")]
    version: u32,
    /// 引用条目,保持 `add` 的输入序——顺序即语义,不排序。
    refs: Vec<AssetRef>,
}

impl Default for AssetManifest {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            refs: Vec::new(),
        }
    }
}

impl AssetManifest {
    /// 空需求表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一条引用。同名追加**不 panic、不去重**——重名只是数据,
    /// 是否冗余由 [`preflight`] 报告,由人/工具裁决。
    pub fn add(&mut self, r: AssetRef) {
        self.refs.push(r);
    }

    /// 全部引用,顺序与 `add` 输入序一致。
    pub fn refs(&self) -> &[AssetRef] {
        &self.refs
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// 紧凑 JSON 快照,字段序固定、同表恒逐位同串——可 diff、可校验和
    /// (风格对齐 memory-json.v1)。形如
    /// `{"version":1,"refs":[{"name":…,"kind":…,"uri":…,"sha256":…}]}`。
    pub fn to_json(&self) -> String {
        // serde_json 对"纯字符串/数字/null"载荷不可能失败;仍走
        // unwrap_or_default 而不是 unwrap,遵守模块"绝不 panic"契约。
        serde_json::to_string(self).unwrap_or_default()
    }

    /// 从 [`AssetManifest::to_json`] 输出重建。任何坏输入(非法 JSON、
    /// 结构不符、未知 kind、字段类型错、`version` 非 1)都是
    /// `Err`([`JsonError`]),绝不 panic;未知字段忽略,向前兼容。
    /// `version` 缺省视为当前版本;重建出的表恒归一化为当前版本。
    pub fn from_json(s: &str) -> Result<Self, JsonError> {
        let parsed: AssetManifest = serde_json::from_str(s).map_err(|e| JsonError {
            message: format!("oriera.asset.refs.v1: parse error: {e}"),
        })?;
        if parsed.version != SCHEMA_VERSION {
            return Err(JsonError {
                message: format!(
                    "oriera.asset.refs.v1: unsupported version {} (expected {SCHEMA_VERSION})",
                    parsed.version
                ),
            });
        }
        Ok(parsed)
    }
}

/// 预检完整性报告——"缺不缺"的确定性答案。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefReport {
    /// 解析失败的 uri,按引用输入序,每条失败的引用各记一次(同一
    /// uri 被多条引用指到且缺失时会重复出现,条数与失败引用数一致)。
    pub missing: Vec<String>,
    /// 出现多于一次的逻辑名,每个名字只记一次,按首现序。
    pub duplicate_names: Vec<String>,
    /// 实际检查的引用条数(= [`AssetManifest::refs`] 长度)。
    pub checked: usize,
}

/// 对 `manifest` 做完整性预检。"文件在不在"由调用方注入的
/// `resolver(uri) -> bool` 回答(主机传真盘探测、wasm 传内核 VFS、
/// 测试传假盘)。报告顺序只取决于输入,双跑逐位一致。
pub fn preflight(manifest: &AssetManifest, resolver: impl Fn(&str) -> bool) -> RefReport {
    let mut missing = Vec::new();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in manifest.refs() {
        if !resolver(&r.uri) {
            missing.push(r.uri.clone());
        }
        *counts.entry(r.name.as_str()).or_insert(0) += 1;
    }
    // 重名输出按名字在 refs 里的首现序,每个重名只记一次。计数表只作
    // 判据、不参与输出顺序——报告顺序仍只取决于输入,双跑逐位一致。
    let mut pushed = std::collections::HashSet::new();
    let mut duplicate_names = Vec::new();
    for r in manifest.refs() {
        if counts[r.name.as_str()] > 1 && pushed.insert(r.name.as_str()) {
            duplicate_names.push(r.name.clone());
        }
    }
    RefReport {
        missing,
        duplicate_names,
        checked: manifest.len(),
    }
}

/// 比对登记的期望摘要与调用方对真实字节自算的十六进制摘要。
/// - `r.sha256` 为 `None` → `false`:未登记期望值即未承诺内容,不得
///   白给通过(诚实语义,不是"缺失即通过");
/// - 大小写不敏感(登记约定是小写,但实测摘要常为大写输出,按内容
///   而非书写风格裁决);
/// - 本函数**不计算哈希、不校验十六进制字符集**——它只是两条十六进制
///   字符串的大小写不敏感等值比对,字节正确性责任在调用方。
pub fn sha256_matches(r: &AssetRef, actual_hex: &str) -> bool {
    match &r.sha256 {
        None => false,
        Some(expected) => expected.eq_ignore_ascii_case(actual_hex),
    }
}

/// JSON 往返错误——只携带一条人读信息。具体类型而非 `String`,便于
/// 调用方 `?` 传播与将来扩展;语义与 memory-json 的 String 错误等价。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// 人读的错误说明(含 `oriera.asset.refs.v1:` 前缀)。
    pub message: String,
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsonError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 假盘:uri → 是否存在。resolver 注入的典型测试形态。
    fn fake_disk(present: &[&str]) -> impl Fn(&str) -> bool {
        let disk: HashMap<String, bool> = present.iter().map(|u| (u.to_string(), true)).collect();
        move |uri| *disk.get(uri).unwrap_or(&false)
    }

    #[test]
    fn missing_and_duplicates_reported_in_input_order() {
        let mut m = AssetManifest::new();
        m.add(AssetRef::new(
            "hero_mesh",
            AssetKind::Mesh,
            "assets/hero.glb",
        ));
        m.add(AssetRef::new(
            "hero_tex",
            AssetKind::Texture,
            "assets/hero.png",
        ));
        m.add(AssetRef::new(
            "bgm_rain",
            AssetKind::Audio,
            "assets/rain.ogg",
        ));
        m.add(AssetRef::new(
            "hero_mesh",
            AssetKind::Mesh,
            "assets/hero_v2.glb",
        )); // 同名追加:合法,由报告披露

        let report = preflight(&m, fake_disk(&["assets/hero.glb", "assets/hero_v2.glb"]));
        // missing 按输入序,只含解析失败的条目
        assert_eq!(report.missing, vec!["assets/hero.png", "assets/rain.ogg"]);
        // 重名按首现序,每个名字一次
        assert_eq!(report.duplicate_names, vec!["hero_mesh"]);
        assert_eq!(report.checked, 4);
    }

    #[test]
    fn duplicate_names_follow_first_occurrence_order() {
        let mut m = AssetManifest::new();
        // 三个重名,交错出现,首现序 = b, a, c
        m.add(AssetRef::new("b", AssetKind::Mesh, "u/b.glb"));
        m.add(AssetRef::new("a", AssetKind::Texture, "u/a.png"));
        m.add(AssetRef::new("c", AssetKind::Audio, "u/c.ogg"));
        m.add(AssetRef::new("a", AssetKind::Texture, "u/a2.png"));
        m.add(AssetRef::new("b", AssetKind::Mesh, "u/b2.glb"));
        m.add(AssetRef::new("c", AssetKind::Audio, "u/c2.ogg"));
        m.add(AssetRef::new("b", AssetKind::Mesh, "u/b3.glb")); // 三现,仍只记一次

        let report = preflight(
            &m,
            fake_disk(&[
                "u/b.glb", "u/a.png", "u/c.ogg", "u/a2.png", "u/b2.glb", "u/c2.ogg", "u/b3.glb",
            ]),
        );
        assert_eq!(report.duplicate_names, vec!["b", "a", "c"]);
        assert_eq!(report.missing, Vec::<String>::new());
        assert_eq!(report.checked, 7);
    }

    #[test]
    fn resolver_injection_controls_the_verdict() {
        let mut m = AssetManifest::new();
        m.add(AssetRef::new("t", AssetKind::Terrain, "w/terrain.bin"));

        // 全有 → 全齐
        let ok = preflight(&m, fake_disk(&["w/terrain.bin"]));
        assert!(ok.missing.is_empty());
        // 全无 → 全缺,顺序保持输入序
        let mut m2 = AssetManifest::new();
        m2.add(AssetRef::new("a", AssetKind::Mesh, "x/1.glb"));
        m2.add(AssetRef::new("b", AssetKind::Texture, "x/2.png"));
        let none = preflight(&m2, |_| false);
        assert_eq!(none.missing, vec!["x/1.glb", "x/2.png"]);
        assert_eq!(none.checked, 2);
    }

    #[test]
    fn json_round_trip_preserves_kinds_and_sha256() {
        let mut m = AssetManifest::new();
        m.add(
            AssetRef::new("hero_mesh", AssetKind::Mesh, "assets/hero.glb")
                .with_sha256("ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12"),
        );
        m.add(AssetRef::new(
            "hero_tex",
            AssetKind::Texture,
            "assets/hero.png",
        ));
        m.add(AssetRef::new(
            "bgm_rain",
            AssetKind::Audio,
            "assets/rain.ogg",
        ));
        m.add(AssetRef::new(
            "intro",
            AssetKind::Clip,
            "assets/intern.clip",
        ));
        m.add(AssetRef::new(
            "ground",
            AssetKind::Terrain,
            "assets/ground.bin",
        ));
        m.add(AssetRef::new("misc", AssetKind::Other, "assets/misc.dat"));

        let restored = AssetManifest::from_json(&m.to_json()).unwrap();
        assert_eq!(restored, m);
        // kind 枚举逐个对拍
        let kinds: Vec<AssetKind> = restored.refs().iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                AssetKind::Mesh,
                AssetKind::Texture,
                AssetKind::Audio,
                AssetKind::Clip,
                AssetKind::Terrain,
                AssetKind::Other,
            ]
        );
        // sha256 Some/None 都在
        assert!(restored.refs()[0].sha256.is_some());
        assert_eq!(restored.refs()[1].sha256, None);
    }

    #[test]
    fn json_output_is_stable_and_diffable() {
        let mut m = AssetManifest::new();
        m.add(AssetRef::new("hero_mesh", AssetKind::Mesh, "assets/hero.glb").with_sha256("ab12"));
        m.add(AssetRef::new(
            "hero_tex",
            AssetKind::Texture,
            "assets/hero.png",
        ));
        // 字段序固定、双跑逐位一致
        let once = m.to_json();
        assert_eq!(m.to_json(), once);
        assert_eq!(
            once,
            "{\"version\":1,\"refs\":[{\"name\":\"hero_mesh\",\"kind\":\"mesh\",\"uri\":\"assets/hero.glb\",\"sha256\":\"ab12\"},{\"name\":\"hero_tex\",\"kind\":\"texture\",\"uri\":\"assets/hero.png\",\"sha256\":null}]}"
        );
    }

    #[test]
    fn from_json_bad_input_is_err_never_panic() {
        // 顶层坏 JSON
        assert!(AssetManifest::from_json("").is_err());
        assert!(AssetManifest::from_json("{not json").is_err());
        assert!(AssetManifest::from_json("[]").is_err());
        assert!(AssetManifest::from_json("\"str\"").is_err());
        // 结构不符
        assert!(AssetManifest::from_json("{\"version\":1,\"refs\":{}}").is_err()); // refs 非数组
        assert!(AssetManifest::from_json("{\"version\":1,\"refs\":[\"x\"]}").is_err()); // 条目非对象
        assert!(AssetManifest::from_json("{\"version\":1,\"refs\":[{}]}").is_err()); // 缺字段
        assert!(AssetManifest::from_json(
            "{\"version\":1,\"refs\":[{\"name\":\"a\",\"kind\":\"meshh\",\"uri\":\"u\"}]}"
        )
        .is_err()); // 未知 kind
        assert!(AssetManifest::from_json(
            "{\"version\":1,\"refs\":[{\"name\":\"a\",\"kind\":\"mesh\",\"uri\":\"u\",\"sha256\":3}]}"
        )
        .is_err()); // sha256 类型错
                    // 版本守卫
        assert!(AssetManifest::from_json("{\"version\":2,\"refs\":[]}").is_err());
        // version 缺省容忍,视为当前版本
        let no_version = AssetManifest::from_json("{\"refs\":[]}").unwrap();
        assert!(no_version.is_empty());
    }

    #[test]
    fn empty_manifest_round_trips_and_preflights_clean() {
        let m = AssetManifest::new();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.to_json(), "{\"version\":1,\"refs\":[]}");
        let restored = AssetManifest::from_json(&m.to_json()).unwrap();
        assert_eq!(restored, m);
        // 即使 resolver 全拒,空表也不缺任何东西
        let report = preflight(&m, |_| false);
        assert_eq!(report, RefReport::default());
        assert_eq!(report.checked, 0);
    }

    #[test]
    fn sha256_matches_case_insensitive_and_none_is_false() {
        let with_hex = AssetRef::new("n", AssetKind::Mesh, "u").with_sha256("ab12cd");
        let no_hex = AssetRef::new("n", AssetKind::Mesh, "u");

        assert!(sha256_matches(&with_hex, "ab12cd")); // 精确相等
        assert!(sha256_matches(&with_hex, "AB12CD")); // 大小写不敏感
        assert!(sha256_matches(&with_hex, "Ab12Cd")); // 混合大小写
        assert!(!sha256_matches(&with_hex, "ab12ce")); // 内容不同
        assert!(!sha256_matches(&with_hex, "ab12")); // 长度不同
                                                     // None → false:未登记期望值即未承诺,不得白给通过(文档语义)
        assert!(!sha256_matches(&no_hex, ""));
        assert!(!sha256_matches(&no_hex, "ab12cd"));
    }

    #[test]
    fn double_run_reports_are_bit_identical() {
        let mut m = AssetManifest::new();
        m.add(AssetRef::new("a", AssetKind::Mesh, "x/1.glb"));
        m.add(AssetRef::new("b", AssetKind::Texture, "x/2.png"));
        m.add(AssetRef::new("a", AssetKind::Audio, "x/3.ogg"));

        let r1 = preflight(&m, fake_disk(&["x/2.png"]));
        let r2 = preflight(&m, fake_disk(&["x/2.png"]));
        assert_eq!(r1, r2); // 逐位一致(含顺序)
        assert_eq!(m.to_json(), m.to_json());
    }

    #[test]
    fn add_appends_same_name_without_panic() {
        let mut m = AssetManifest::new();
        m.add(AssetRef::new("hero", AssetKind::Mesh, "u/1.glb"));
        m.add(AssetRef::new("hero", AssetKind::Texture, "u/2.png"));
        assert_eq!(m.len(), 2); // 同名追加:不去重、不 panic
        assert_eq!(m.refs()[0].kind, AssetKind::Mesh);
        assert_eq!(m.refs()[1].kind, AssetKind::Texture);
    }

    #[test]
    fn json_error_displays_and_impls_std_error() {
        let e = AssetManifest::from_json("{bad").unwrap_err();
        assert!(e.message.starts_with("oriera.asset.refs.v1:"));
        assert!(e.to_string().contains("parse error"));
        let _box: Box<dyn std::error::Error> = Box::new(e); // std::error::Error 可用
    }
}
