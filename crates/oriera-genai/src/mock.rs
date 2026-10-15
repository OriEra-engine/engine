//! Deterministic offline generation backend — the L8 twin of L7's
//! `RuleProvider`. Same input → same task sequence → same bytes, so
//! engine tests stay hermetic and every demo runs without a key.
//! Remote adapters (Tripo, Seedance) live behind the `providers`
//! feature and are opt-in at boot, never at build time.

use crate::{
    AssetFormat, GenAiError, GeneratedAsset, GenerationKind, GenerationProvider, GenerationRequest,
    ProviderCapabilities, TaskId, TaskStatus,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// How many polls a task stays non-terminal before completing. `2`
/// gives demos a visible Queued → Running → Succeeded progression.
const DEFAULT_POLLS_TO_COMPLETE: u32 = 2;

/// Offline backend: tasks complete after a fixed number of polls and
/// return stub assets ([`crate::tiny_glb`] for 3D kinds — a fully
/// valid, parseable container; signature-valid stubs otherwise).
pub struct MockProvider {
    id: &'static str,
    caps: ProviderCapabilities,
    polls_to_complete: u32,
    state: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    next: u64,
    /// task id → (polls served, requested kind).
    tasks: HashMap<String, (u32, GenerationKind)>,
}

impl MockProvider {
    /// All capabilities, default pacing.
    pub fn new() -> Self {
        Self {
            id: "mock-genai-v1",
            caps: ProviderCapabilities::ALL,
            polls_to_complete: DEFAULT_POLLS_TO_COMPLETE,
            state: Mutex::new(MockState::default()),
        }
    }

    /// Restrict capabilities — for demos and tests of the
    /// [`GenAiError::Unsupported`] path.
    pub fn with_capabilities(caps: ProviderCapabilities) -> Self {
        Self {
            caps,
            ..Self::new()
        }
    }

    fn stub_for(kind: GenerationKind) -> GeneratedAsset {
        let (format, payload) = match kind {
            GenerationKind::TextTo3d | GenerationKind::ImageTo3d => {
                (AssetFormat::Glb, crate::tiny_glb())
            }
            GenerationKind::TextToVideo | GenerationKind::ImageToVideo => {
                (AssetFormat::Video, crate::tiny_mp4_stub())
            }
        };
        GeneratedAsset {
            format,
            payload,
            provider_id: "mock-genai-v1".into(),
            model: "mock-stub".into(),
        }
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl GenerationProvider for MockProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.caps
    }

    fn request_generation(&self, req: &GenerationRequest) -> Result<TaskId, GenAiError> {
        if !self.caps.supports(req.kind) {
            return Err(GenAiError::Unsupported(format!(
                "mock provider does not serve {}",
                req.kind.id()
            )));
        }
        if req.name.trim().is_empty() {
            return Err(GenAiError::Unsupported(
                "generation needs a non-empty landing name".into(),
            ));
        }
        let n = {
            let Ok(mut st) = self.state.lock() else {
                return Err(GenAiError::Provider("mock state poisoned".into()));
            };
            st.next += 1;
            let key = format!("mock-{}", st.next);
            st.tasks.insert(key, (0, req.kind));
            st.next
        };
        Ok(TaskId(format!("mock-{n}")))
    }

    fn poll_task(&self, task: &TaskId) -> Result<TaskStatus, GenAiError> {
        let Ok(mut st) = self.state.lock() else {
            return Err(GenAiError::Provider("mock state poisoned".into()));
        };
        let Some((served, kind)) = st.tasks.get_mut(&task.0) else {
            return Err(GenAiError::Provider(format!("unknown task {}", task.0)));
        };
        *served += 1;
        // First poll reports the queue wait, later polls run, then done.
        if *served == 1 {
            Ok(TaskStatus::Queued)
        } else if *served <= self.polls_to_complete {
            Ok(TaskStatus::Running)
        } else {
            Ok(TaskStatus::Succeeded(Self::stub_for(*kind)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_is_queued_running_succeeded() {
        let p = MockProvider::new();
        let id = p
            .request_generation(&GenerationRequest::text_to_3d("npc_prop", "一把老木吉他"))
            .unwrap();
        assert_eq!(p.poll_task(&id).unwrap(), TaskStatus::Queued);
        assert_eq!(p.poll_task(&id).unwrap(), TaskStatus::Running);
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Glb);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn video_requests_land_video_stubs() {
        let p = MockProvider::new();
        let id = p
            .request_generation(&GenerationRequest::text_to_video("intro", "雨夜的咖啡馆"))
            .unwrap();
        p.poll_task(&id).unwrap(); // Queued
        p.poll_task(&id).unwrap(); // Running
        match p.poll_task(&id).unwrap() {
            TaskStatus::Succeeded(a) => {
                assert_eq!(a.format, AssetFormat::Video);
                assert!(crate::validate_asset(&a, &Default::default()).is_ok());
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn polls_after_terminal_keep_returning_the_asset() {
        let p = MockProvider::new();
        let id = p
            .request_generation(&GenerationRequest::text_to_3d("x", "y"))
            .unwrap();
        for _ in 0..6 {
            assert!(p.poll_task(&id).is_ok());
        }
        assert!(p.poll_task(&id).unwrap().is_terminal());
    }

    #[test]
    fn unknown_task_is_a_provider_error() {
        let p = MockProvider::new();
        assert!(matches!(
            p.poll_task(&TaskId("nope".into())),
            Err(GenAiError::Provider(_))
        ));
    }

    #[test]
    fn unsupported_kind_fails_fast() {
        let p = MockProvider::with_capabilities(ProviderCapabilities {
            text_to_3d: true,
            ..ProviderCapabilities::default()
        });
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_video("v", "clip")),
            Err(GenAiError::Unsupported(_))
        ));
    }

    #[test]
    fn empty_name_is_rejected() {
        let p = MockProvider::new();
        assert!(matches!(
            p.request_generation(&GenerationRequest::text_to_3d("  ", "x")),
            Err(GenAiError::Unsupported(_))
        ));
    }

    #[test]
    fn output_is_deterministic_across_providers() {
        let run = || {
            let p = MockProvider::new();
            let id = p
                .request_generation(&GenerationRequest::text_to_3d("a", "b"))
                .unwrap();
            let first = p.poll_task(&id).unwrap();
            let second = p.poll_task(&id).unwrap();
            (id, first, second)
        };
        assert_eq!(run(), run());
    }
}
