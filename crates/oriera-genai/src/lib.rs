//! L8 Generative AI — native asset generation behind one engine boundary.
//!
//! This layer extends the L7 philosophy (book T10, AI-NATIVE.md) from
//! *completions* to *generated assets*: the engine — not the application —
//! owns generation. Upper layers (L9 agents, L11 apps) talk to
//! [`GenerationProvider`]; they never see HTTP, keys or vendor URLs.
//! [`MockProvider`] is the deterministic offline backend that keeps tests
//! hermetic and lets the engine run without a key — same role as L7's
//! `RuleProvider`.
//!
//! Generation is **task-shaped**: submit ([`GenerationProvider::request_generation`]),
//! poll ([`GenerationProvider::poll_task`]), land. Both first-party adapters
//! (Tripo, Seedance) and any future vendor are asynchronous create-and-poll
//! services, so the boundary is designed once around that shape.
//!
//! "Native, not bolted on" is defined by ADR 0003 and audited by five
//! criteria: assets land through L4 named hot swap, L9 can invoke it,
//! absence degrades to a placeholder instead of a crash, the contract is
//! in `contracts/oriera/genai/v1`, and the engine core vendors zero SDKs.

#[cfg(feature = "providers")]
pub mod cogview;
pub mod config;
pub mod hot_swap;
pub mod mock;
#[cfg(feature = "providers")]
pub mod seedance;
pub mod system;
#[cfg(feature = "providers")]
pub mod transport;
#[cfg(feature = "providers")]
pub mod tripo;

#[cfg(feature = "providers")]
pub use config::create_generation_provider_with_transport;
pub use config::{create_generation_provider, ProviderConfig, ProviderKind};
pub use hot_swap::{apply_pending, GenAsset};
pub use mock::MockProvider;
pub use system::{
    AssetFailed, AssetLanded, AssetSwap, GenAiLimits, GenAiMeter, GenAiSystem, PendingSwaps,
};

use std::fmt;

/// Opaque provider task handle, stable across polls. Never reused after a
/// terminal status.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

/// What the creator asked for. Providers declare support via
/// [`ProviderCapabilities`]; the engine rejects unsupported requests
/// before any transport work happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationKind {
    /// prompt → GLB/USDZ mesh (e.g. Tripo).
    TextTo3d,
    /// reference image → GLB/USDZ mesh.
    ImageTo3d,
    /// prompt → PNG/JPEG still (e.g. CogView, any OpenAI-images-dialect
    /// endpoint). The single highest-leverage kind for small 3D games:
    /// skydomes, ground albedo, posters, sprite sheets are all stills.
    TextToImage,
    /// prompt → video container (e.g. Seedance). Seedance is a *video*
    /// model; in this layer it also feeds future image-to-3D pipelines.
    TextToVideo,
    /// first-frame image → video.
    ImageToVideo,
}

impl GenerationKind {
    /// Stable id for metrics (`oriera_genai_request{kind=...}`).
    pub fn id(self) -> &'static str {
        match self {
            GenerationKind::TextTo3d => "text_to_3d",
            GenerationKind::ImageTo3d => "image_to_3d",
            GenerationKind::TextToImage => "text_to_image",
            GenerationKind::TextToVideo => "text_to_video",
            GenerationKind::ImageToVideo => "image_to_video",
        }
    }

    /// The degrade payload format for this kind: a placeholder landing
    /// after a failure must be parseable by whatever consumes the slot
    /// (ADR 0003 criterion 3 — an outage costs fidelity, never a crash;
    /// a still slot must not receive an empty mesh).
    pub fn placeholder_format(self) -> AssetFormat {
        match self {
            GenerationKind::TextTo3d | GenerationKind::ImageTo3d => AssetFormat::Glb,
            GenerationKind::TextToImage => AssetFormat::Image,
            GenerationKind::TextToVideo | GenerationKind::ImageToVideo => AssetFormat::Video,
        }
    }
}

/// Container format of a generated asset. Drives validation magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetFormat {
    /// Binary glTF, magic `glTF`.
    Glb,
    /// USDZ zip container, magic `PK`.
    Usdz,
    /// MP4 (`ftyp`) or WebM (EBML) container.
    Video,
    /// PNG or JPEG still.
    Image,
}

impl AssetFormat {
    /// Stable id for metrics.
    pub fn id(self) -> &'static str {
        match self {
            AssetFormat::Glb => "glb",
            AssetFormat::Usdz => "usdz",
            AssetFormat::Video => "video",
            AssetFormat::Image => "image",
        }
    }
}

/// One generation request. `name` is the L4 `insert_named` landing key —
/// the whole point of the pipeline is that the result swaps in behind
/// that name without touching entities (ADR 0003 criterion 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRequest {
    pub kind: GenerationKind,
    pub prompt: String,
    /// Empty means "no negative prompt".
    pub negative_prompt: String,
    /// Logical asset name the result will land under.
    pub name: String,
    /// Client-side validation ceiling. Providers must not bypass it.
    pub max_bytes: u64,
    /// Optional deterministic seed where the provider supports one.
    pub seed: Option<u64>,
}

impl GenerationRequest {
    /// A text-to-3D request landing under `name`, with default limits.
    pub fn text_to_3d(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            kind: GenerationKind::TextTo3d,
            prompt: prompt.into(),
            negative_prompt: String::new(),
            name: name.into(),
            max_bytes: ValidationLimits::default().max_bytes,
            seed: None,
        }
    }

    /// A text-to-image request landing under `name`, with default limits.
    pub fn text_to_image(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            kind: GenerationKind::TextToImage,
            prompt: prompt.into(),
            negative_prompt: String::new(),
            name: name.into(),
            max_bytes: ValidationLimits::default().max_bytes,
            seed: None,
        }
    }

    /// A text-to-video request landing under `name`, with default limits.
    pub fn text_to_video(name: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self {
            kind: GenerationKind::TextToVideo,
            prompt: prompt.into(),
            negative_prompt: String::new(),
            name: name.into(),
            max_bytes: ValidationLimits::default().max_bytes,
            seed: None,
        }
    }
}

/// A validated generated asset. `payload` has passed
/// [`validate_asset`] before this value is produced anywhere outside a
/// provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedAsset {
    pub format: AssetFormat,
    pub payload: Vec<u8>,
    /// Which adapter produced it (metrics, `oriera_genai_asset{provider=...}`).
    pub provider_id: String,
    /// Provider-reported model id, when the vendor exposes one.
    pub model: String,
}

/// Task lifecycle as seen through [`GenerationProvider::poll_task`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded(GeneratedAsset),
    Failed(String),
}

impl TaskStatus {
    /// Terminal statuses carry their outcome; a `true` return means
    /// further polls would be wasted work.
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Succeeded(_) | TaskStatus::Failed(_))
    }
}

/// Which generation kinds a provider serves. A request for an
/// unsupported kind fails fast with [`GenAiError::Unsupported`] — no
/// transport, no billing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub text_to_3d: bool,
    pub image_to_3d: bool,
    pub text_to_image: bool,
    pub text_to_video: bool,
    pub image_to_video: bool,
}

impl ProviderCapabilities {
    /// All five kinds — the [`MockProvider`] default.
    pub const ALL: ProviderCapabilities = ProviderCapabilities {
        text_to_3d: true,
        image_to_3d: true,
        text_to_image: true,
        text_to_video: true,
        image_to_video: true,
    };

    /// Does this provider serve `kind`?
    pub fn supports(self, kind: GenerationKind) -> bool {
        match kind {
            GenerationKind::TextTo3d => self.text_to_3d,
            GenerationKind::ImageTo3d => self.image_to_3d,
            GenerationKind::TextToImage => self.text_to_image,
            GenerationKind::TextToVideo => self.text_to_video,
            GenerationKind::ImageToVideo => self.image_to_video,
        }
    }
}

/// Why a generation failed. Vendor details stay inside the adapter —
/// they never leak into layer-9 prompt code (mirrors L7 `InferError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenAiError {
    /// The provider does not serve this [`GenerationKind`] / request.
    Unsupported(String),
    /// The provider refused / errored / timed out.
    Provider(String),
    /// The produced asset failed [`validate_asset`].
    Validation(String),
}

/// Mirrors L7 `oriera_infer::ErrorClass` **semantics** (same stable ids)
/// without depending on oriera-infer — ADR 0003 keeps generation and
/// inference decoupled, so the vocabulary is copied, not imported
/// (#106, review E8: CogView/Tripo/Seedance hit 429/402 too, and slot
/// degradation needs the same retry/fall-through policy words).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenAiErrorClass {
    RateLimited,
    Auth,
    Quota,
    Server,
    Timeout,
    Provider,
}

impl GenAiErrorClass {
    /// Stable id for metrics labels — identical strings to L7's ids so
    /// a shared dashboard can chart both layers under one axis.
    pub fn id(self) -> &'static str {
        match self {
            GenAiErrorClass::RateLimited => "rate_limited",
            GenAiErrorClass::Auth => "auth",
            GenAiErrorClass::Quota => "quota",
            GenAiErrorClass::Server => "server",
            GenAiErrorClass::Timeout => "timeout",
            GenAiErrorClass::Provider => "provider",
        }
    }

    /// Same policy as L7: transport-transient classes are worth backing
    /// off (or falling through the chain); capability/config failures
    /// are not — retrying cannot change the outcome.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            GenAiErrorClass::RateLimited | GenAiErrorClass::Server | GenAiErrorClass::Timeout
        )
    }
}

impl GenAiError {
    /// Fold into the public class vocabulary. `Unsupported` is a
    /// capability fact (never retryable); `Validation` is a produced-
    /// asset fact (the request reached and answered — provider bucket);
    /// `Provider` messages are keyword-sniffed for the transport facts
    /// adapters embed (429/401/402/5xx/timeout).
    pub fn class(&self) -> GenAiErrorClass {
        match self {
            GenAiError::Unsupported(_) => GenAiErrorClass::Provider,
            GenAiError::Validation(_) => GenAiErrorClass::Provider,
            GenAiError::Provider(msg) => {
                let m = msg.to_ascii_lowercase();
                if m.contains("429") || m.contains("rate limit") || m.contains("too many requests")
                {
                    GenAiErrorClass::RateLimited
                } else if m.contains("401") || m.contains("403") {
                    GenAiErrorClass::Auth
                } else if m.contains("402") {
                    GenAiErrorClass::Quota
                } else if m.contains("timeout") || m.contains("timed out") {
                    GenAiErrorClass::Timeout
                } else if m.contains("500") || m.contains("502") || m.contains("503") {
                    GenAiErrorClass::Server
                } else {
                    GenAiErrorClass::Provider
                }
            }
        }
    }

    /// Retry/fall-through policy (delegates to the class).
    pub fn retryable(&self) -> bool {
        self.class().retryable()
    }
}

impl fmt::Display for GenAiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenAiError::Unsupported(m) => write!(f, "unsupported generation: {m}"),
            GenAiError::Provider(m) => write!(f, "provider error: {m}"),
            GenAiError::Validation(m) => write!(f, "asset validation failed: {m}"),
        }
    }
}

impl std::error::Error for GenAiError {}

/// The generation boundary every consumer depends on (L8 contract,
/// `oriera.genai.provider.v1`, ADR 0003). Implementations are
/// protocol adapters; transport is injected, keys live only in
/// platform shells.
pub trait GenerationProvider: Send + Sync {
    /// Stable provider id for metrics.
    fn id(&self) -> &str;

    /// Which kinds this provider serves.
    fn capabilities(&self) -> ProviderCapabilities;

    /// Submit a task; returns its poll handle. Unsupported kinds must
    /// fail here with [`GenAiError::Unsupported`].
    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError>;

    /// Poll one task. Terminal statuses carry the asset or the error.
    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError>;
}

// ---------------------------------------------------------------------------
// Validation (appendix-B `ValidateAsset`)
// ---------------------------------------------------------------------------

/// Default per-asset size ceiling: 64 MiB. Generous for v0.1 payloads
/// (meshes, short clips) and small enough to stop a runaway download
/// before it matters.
pub const DEFAULT_MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceiling [`ValidationLimits::effective`] clamps to — a
/// hallucinated `u64::MAX` from a model must not become policy (same
/// philosophy as L6 `TimeScale`).
pub const HARD_MAX_ASSET_BYTES: u64 = 1024 * 1024 * 1024;

/// Client-side validation limits. One clamp entry point
/// ([`ValidationLimits::effective`]) so every consumer sees the same
/// policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationLimits {
    pub max_bytes: u64,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_ASSET_BYTES,
        }
    }
}

impl ValidationLimits {
    /// The limits every validator must use: `max_bytes` clamped to
    /// `[1, HARD_MAX_ASSET_BYTES]` (zero or absurd → default).
    pub fn effective(self) -> Self {
        if self.max_bytes == 0 || self.max_bytes > HARD_MAX_ASSET_BYTES {
            Self::default()
        } else {
            self
        }
    }
}

/// Validate a generated asset before it may approach the asset store:
/// size ceiling plus format magic (GLB header/chunk consistency, USDZ
/// zip prefix, MP4/WebM/PNG/JPEG signatures).
///
/// Depth promise (ADR 0003): in the skeleton stage this checks magic +
/// size + container self-consistency. Deep checks — mesh legality, zip
/// nesting depth (decompression-bomb guard) — land with the v0.2 asset
/// decoder, because a decoder that never runs cannot be lied to by
/// bytes it never reads.
pub fn validate_asset(asset: &GeneratedAsset, limits: &ValidationLimits) -> Result<(), GenAiError> {
    let limits = limits.effective();
    let bytes = &asset.payload;
    if bytes.len() as u64 > limits.max_bytes {
        return Err(GenAiError::Validation(format!(
            "{} bytes exceeds the {} byte ceiling",
            bytes.len(),
            limits.max_bytes
        )));
    }
    let ok = match asset.format {
        AssetFormat::Glb => glb_header_is_sane(bytes),
        AssetFormat::Usdz => bytes.starts_with(b"PK"),
        AssetFormat::Video => {
            bytes.len() >= 8
                && (&bytes[4..8] == b"ftyp"            // MP4
                    || bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])) // WebM EBML
        }
        AssetFormat::Image => {
            bytes.starts_with(&[0x89, b'P', b'N', b'G']) // PNG
                || bytes.starts_with(&[0xFF, 0xD8, 0xFF]) // JPEG
        }
    };
    if ok {
        Ok(())
    } else {
        Err(GenAiError::Validation(format!(
            "payload does not match the {} format signature",
            asset.format.id()
        )))
    }
}

/// GLB container sanity: magic `glTF`, version 2, declared total length
/// equals the actual length, and the first chunk is a JSON chunk whose
/// declared length fits inside the file.
fn glb_header_is_sane(b: &[u8]) -> bool {
    if b.len() < 20 || !b.starts_with(b"glTF") {
        return false;
    }
    let u32_at = |off: usize| -> u64 {
        u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]) as u64
    };
    if u32_at(4) != 2 {
        return false; // version
    }
    if u32_at(8) != b.len() as u64 {
        return false; // declared total length
    }
    let chunk_len = u32_at(12);
    &b[16..20] == b"JSON" && 20 + chunk_len <= b.len() as u64
}

/// The smallest **visible** GLB: one triangle (ADR 0003 criterion 3,
/// upgraded — an outage lands a placeholder you can actually see, not
/// an empty scene). Doubles as the MockProvider fixture and as the
/// L8→L4→L5 chain proof: `oriera_asset::glb::decode_glb_mesh` parses
/// it into 3 vertices / 3 indices.
pub(crate) fn tiny_glb() -> Vec<u8> {
    let json = br#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[{"mesh":0}]}],"meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2,"material":0}]}],"materials":[{"pbrMetallicRoughness":{"baseColorFactor":[0.85,0.16,0.12,1.0]}}],"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","max":[0.8,0.6,0.0],"min":[-0.8,-0.6,0.0]},{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},{"bufferView":2,"componentType":5125,"count":3,"type":"SCALAR"}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36},{"buffer":0,"byteOffset":72,"byteLength":12}],"buffers":[{"byteLength":84}]}"#;
    let json_len = json.len().div_ceil(4) * 4; // JSON chunk pads with spaces
                                               // BIN: 3 × pos(36B) + 3 × normal(36B,朝镜头) + 3 × u32(12B) = 84B。
    let mut bin = Vec::with_capacity(84);
    for v in [[0.0f32, -0.6, 0.0], [0.8, 0.6, 0.0], [-0.8, 0.6, 0.0]] {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for _ in 0..3 {
        for c in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    for i in [0u32, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    let total = 12 + 8 + json_len + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_len as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(json);
    out.extend(std::iter::repeat_n(b' ', json_len - json.len()));
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\x00");
    out.extend_from_slice(&bin);
    out
}

/// Header-valid MP4 stub the MockProvider returns for video kinds.
/// Deliberately not playable — it exists so the pipeline can be exercised
/// end to end offline; real video comes from real providers.
pub(crate) fn tiny_mp4_stub() -> Vec<u8> {
    let mut out = vec![0u8; 8]; // size placeholder + "ftyp" slot
    out[4..8].copy_from_slice(b"ftyp");
    out.extend_from_slice(b"isom");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"isomiso2avc1mp41");
    out
}

/// The smallest fully-valid PNG: one 1×1 RGBA pixel (stored, zlib level
/// 0 stream). Doubles as the **placeholder still** for image degrade
/// paths — one payload, provably parseable by any PNG decoder, small
/// enough to inline. Chunk CRCs are computed at construction, so the
/// container is self-consistent by construction, not by transcription.
pub(crate) fn tiny_png() -> Vec<u8> {
    /// Reference CRC-32 (ISO 3309, as PNG requires).
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(ty);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(4 + data.len());
        crc_input.extend_from_slice(ty);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    }
    // IHDR: 1×1, 8-bit RGBA, no interlace.
    let ihdr: [u8; 13] = [
        0, 0, 0, 1, // width
        0, 0, 0, 1, // height
        8, 6, 0, 0, 0, // bit depth, color type, flags
    ];
    // IDAT: zlib stream with one stored (uncompressed) deflate block —
    // filter byte 0 + one transparent RGBA pixel. Adler-32 of five zero
    // bytes is 5 (`00 00 00 05` big-endian).
    let idat: [u8; 16] = [
        0x78, 0x9C, // zlib header (CMF/FLG, checksum valid)
        0x01, 0x05, 0x00, 0xFA, 0xFF, // BFINAL stored block, LEN=5, NLEN
        0x00, // scanline filter: none
        0x00, 0x00, 0x00, 0x00, // one transparent pixel
        0x00, 0x05, 0x00, 0x01, // Adler-32: b=5, a=1
    ];
    let mut out = Vec::with_capacity(8 + 25 + 28 + 12);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn genai_error_classes_mirror_l7_semantics() {
        use super::*;
        // 能力缺失：不可重试
        assert_eq!(
            GenAiError::Unsupported("no mesh kind".into()).class(),
            GenAiErrorClass::Provider
        );
        assert!(!GenAiError::Unsupported("x".into()).retryable());
        // 传输事实关键词嗅探
        assert_eq!(
            GenAiError::Provider("HTTP 429: slow down".into()).class(),
            GenAiErrorClass::RateLimited
        );
        assert!(GenAiError::Provider("HTTP 429".into()).retryable());
        assert_eq!(
            GenAiError::Provider("HTTP 401: bad key".into()).class(),
            GenAiErrorClass::Auth
        );
        assert_eq!(
            GenAiError::Provider("HTTP 402: out of balance".into()).class(),
            GenAiErrorClass::Quota
        );
        assert_eq!(
            GenAiError::Provider("request timed out".into()).class(),
            GenAiErrorClass::Timeout
        );
        assert!(GenAiError::Provider("gateway 502".into()).retryable());
        assert_eq!(
            GenAiError::Provider("weird garbage".into()).class(),
            GenAiErrorClass::Provider
        );
        // 稳定 id 与 L7 同词表
        assert_eq!(GenAiErrorClass::RateLimited.id(), "rate_limited");
        assert_eq!(GenAiErrorClass::Auth.id(), "auth");
    }

    use super::*;

    fn asset(format: AssetFormat, payload: Vec<u8>) -> GeneratedAsset {
        GeneratedAsset {
            format,
            payload,
            provider_id: "test".into(),
            model: String::new(),
        }
    }

    #[test]
    fn tiny_glb_is_a_valid_asset() {
        let glb = asset(AssetFormat::Glb, tiny_glb());
        assert!(validate_asset(&glb, &ValidationLimits::default()).is_ok());
        // Declared total length matches the buffer exactly.
        assert_eq!(
            u32::from_le_bytes(tiny_glb()[8..12].try_into().unwrap()) as usize,
            tiny_glb().len()
        );
    }

    #[test]
    fn stubs_pass_their_format_signatures() {
        // PNG signature inline: no production kind emits images yet, so
        // there is no stub constructor — validation is what is tested.
        let png: Vec<u8> = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'].into();
        for (format, payload) in [
            (AssetFormat::Video, tiny_mp4_stub()),
            (AssetFormat::Image, png),
        ] {
            assert!(validate_asset(&asset(format, payload), &ValidationLimits::default()).is_ok());
        }
    }

    #[test]
    fn tiny_png_is_a_structurally_valid_png() {
        let png = asset(AssetFormat::Image, tiny_png());
        assert!(validate_asset(&png, &ValidationLimits::default()).is_ok());
        // Verify every chunk's declared length and CRC — the placeholder
        // must be parseable by a real decoder, not just sniffable.
        let b = &png.payload[..];
        let mut off = 8;
        let mut chunks = 0;
        while off < b.len() {
            let len = u32::from_be_bytes(b[off..off + 4].try_into().unwrap()) as usize;
            let ty = &b[off + 4..off + 8];
            let crc_stored =
                u32::from_be_bytes(b[off + 8 + len..off + 12 + len].try_into().unwrap());
            assert_eq!(crc32(&b[off + 4..off + 8 + len]), crc_stored, "{ty:?}");
            chunks += 1;
            off += 12 + len;
        }
        assert_eq!(chunks, 3); // IHDR + IDAT + IEND
        assert_eq!(off, b.len()); // no trailing bytes
    }

    /// Reference CRC-32 (ISO 3309, as PNG requires) used only by the
    /// chunk-integrity test above.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn placeholder_format_matches_the_requested_kind() {
        assert_eq!(
            GenerationKind::TextToImage.placeholder_format(),
            AssetFormat::Image
        );
        assert_eq!(
            GenerationKind::TextTo3d.placeholder_format(),
            AssetFormat::Glb
        );
        assert_eq!(
            GenerationKind::TextToVideo.placeholder_format(),
            AssetFormat::Video
        );
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let bad = asset(AssetFormat::Glb, b"not a glb at all".to_vec());
        assert!(validate_asset(&bad, &ValidationLimits::default()).is_err());
        let usdz_bytes = asset(AssetFormat::Usdz, b"PK\x03\x04junk".to_vec());
        assert!(validate_asset(&usdz_bytes, &ValidationLimits::default()).is_ok());
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let glb = asset(AssetFormat::Glb, tiny_glb());
        let tight = ValidationLimits { max_bytes: 8 };
        assert!(validate_asset(&glb, &tight).is_err());
    }

    #[test]
    fn limits_clamp_through_one_entry_point() {
        assert_eq!(
            ValidationLimits { max_bytes: 0 }.effective(),
            ValidationLimits::default()
        );
        assert_eq!(
            ValidationLimits {
                max_bytes: u64::MAX
            }
            .effective(),
            ValidationLimits::default()
        );
        assert_eq!(
            ValidationLimits { max_bytes: 1024 }.effective().max_bytes,
            1024
        );
    }

    #[test]
    fn task_status_terminality() {
        assert!(!TaskStatus::Queued.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        assert!(TaskStatus::Failed("x".into()).is_terminal());
        assert!(TaskStatus::Succeeded(asset(AssetFormat::Glb, tiny_glb())).is_terminal());
    }

    #[test]
    fn capabilities_gate_kinds() {
        assert!(ProviderCapabilities::ALL.supports(GenerationKind::TextTo3d));
        assert!(ProviderCapabilities::ALL.supports(GenerationKind::TextToImage));
        let none = ProviderCapabilities::default();
        assert!(!none.supports(GenerationKind::ImageToVideo));
        assert!(!none.supports(GenerationKind::TextToImage));
    }
}
