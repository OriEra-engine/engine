//! Network boundary (L1).
//!
//! Contract: `oriera.pal.net.v1` (Table B-1: RouteRemoteTask — privacy
//! and latency constraints are decided here, upper layers only see the
//! plan). WebTransport/QUIC preferred; WebSocket as fallback.

use crate::capability::Capabilities;

/// A remote endpoint identified by URL. Transport selection is PAL's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub url: String,
}

/// Where a task actually ran — reported back for cost accounting (L15 metering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionSite {
    /// Executed on-device (NPU/GPU/CPU).
    Local,
    /// Executed on the edge VPS.
    Edge,
    /// Executed in the cloud.
    Cloud,
}

/// Best-effort datagram/message transport.
pub trait NetTransport {
    /// Send one message; false when the link is down (caller falls back
    /// to local execution — ADR 0001 "端云路由由下层自治").
    fn send(&mut self, endpoint: &Endpoint, payload: &[u8]) -> bool;

    /// Receive pending messages into `out`; returns how many were pushed.
    fn recv(&mut self, out: &mut Vec<Vec<u8>>) -> usize;

    fn is_connected(&self) -> bool;
}

/// Offline transport for tests: everything stays local.
#[derive(Debug, Default)]
pub struct OfflineTransport;

impl NetTransport for OfflineTransport {
    fn send(&mut self, _endpoint: &Endpoint, _payload: &[u8]) -> bool {
        false
    }
    fn recv(&mut self, _out: &mut Vec<Vec<u8>>) -> usize {
        0
    }
    fn is_connected(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Task routing (RouteRemoteTask, decided once at the L1 boundary)
// ---------------------------------------------------------------------------

/// Privacy floor for a task. `OnDeviceOnly` never leaves the device —
/// voice transcripts, camera frames, anything user-identifying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privacy {
    /// May run anywhere the router picks.
    Any,
    /// Must run on-device; the router answers `Local` unconditionally.
    OnDeviceOnly,
}

/// What the caller knows about a task before it runs. Pure data — the
/// router turns it (plus the platform [`Capabilities`]) into an
/// [`ExecutionSite`], so no layer above L1 ever re-derives the
/// edge/cloud/local decision (ADR 0001: 端云路由由下层自治).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteHint {
    /// Wire size of the request (prompt/frame/patch payload).
    pub payload_bytes: u32,
    /// How long the caller can afford to wait, end to end.
    pub latency_budget_ms: u32,
    pub privacy: Privacy,
}

/// Payloads at or below this size are considered NPU-friendly on any
/// device that reports an NPU tier (prompts, small patches — not
/// multi-MB generation requests).
pub const NPU_PAYLOAD_LIMIT_BYTES: u32 = 64 * 1024;

/// Decide where a remote-shaped task should run. Deterministic and pure:
/// same caps + hint → same site, so routing behavior is replayable and
/// testable without a network.
///
/// Policy (v1):
/// 1. `OnDeviceOnly` → [`ExecutionSite::Local`], always.
/// 2. An NPU tier is present and the payload is small → `Local`
///    (0 bytes of wire, lowest latency).
/// 3. Edge VPS capability and a tight latency budget → `Edge`.
/// 4. Otherwise → `Cloud`.
pub fn route(caps: Capabilities, hint: RouteHint) -> ExecutionSite {
    if hint.privacy == Privacy::OnDeviceOnly {
        return ExecutionSite::Local;
    }
    let has_npu = caps.intersects(
        Capabilities::NPU_TOPS_35 | Capabilities::NPU_TOPS_48 | Capabilities::NPU_TOPS_160,
    );
    if has_npu && hint.payload_bytes <= NPU_PAYLOAD_LIMIT_BYTES {
        return ExecutionSite::Local;
    }
    if caps.contains(Capabilities::NET_EDGE_VPS) && hint.latency_budget_ms < 100 {
        return ExecutionSite::Edge;
    }
    ExecutionSite::Cloud
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_transport_reports_disconnected() {
        let mut t = OfflineTransport;
        assert!(!t.is_connected());
        assert!(!t.send(
            &Endpoint {
                url: "wt://edge".into()
            },
            b"x"
        ));
        assert_eq!(t.recv(&mut Vec::new()), 0);
    }

    fn hint(payload: u32, latency: u32, privacy: Privacy) -> RouteHint {
        RouteHint {
            payload_bytes: payload,
            latency_budget_ms: latency,
            privacy,
        }
    }

    #[test]
    fn privacy_pins_tasks_on_device() {
        // Even a huge cloud-shaped payload stays local.
        let ar = Capabilities::NET_EDGE_VPS | Capabilities::NPU_TOPS_48;
        assert_eq!(
            route(ar, hint(10_000_000, 5_000, Privacy::OnDeviceOnly)),
            ExecutionSite::Local
        );
        // A device with nothing still answers Local, never blocks.
        assert_eq!(
            route(Capabilities::empty(), hint(1, 1, Privacy::OnDeviceOnly)),
            ExecutionSite::Local
        );
    }

    #[test]
    fn npu_takes_small_payloads_locally() {
        let mobile = Capabilities::NPU_TOPS_48;
        assert_eq!(
            route(mobile, hint(4_096, 500, Privacy::Any)),
            ExecutionSite::Local
        );
        // Same device, generation-sized payload → not local.
        assert_eq!(
            route(mobile, hint(1024 * 1024, 500, Privacy::Any)),
            ExecutionSite::Cloud
        );
    }

    #[test]
    fn edge_wins_for_tight_budgets_when_present() {
        // No NPU, edge VPS available, sub-100ms budget.
        let edgey = Capabilities::NET_EDGE_VPS;
        assert_eq!(
            route(edgey, hint(200_000, 40, Privacy::Any)),
            ExecutionSite::Edge
        );
        // Loose budget → cloud (bigger models live there).
        assert_eq!(
            route(edgey, hint(200_000, 400, Privacy::Any)),
            ExecutionSite::Cloud
        );
        // Tight budget but no edge capability → cloud.
        assert_eq!(
            route(Capabilities::empty(), hint(200_000, 40, Privacy::Any)),
            ExecutionSite::Cloud
        );
    }

    #[test]
    fn routing_is_deterministic() {
        let caps = Capabilities::GFX_WGPU | Capabilities::NET_EDGE_VPS | Capabilities::NPU_TOPS_35;
        let h = hint(1_024, 30, Privacy::Any);
        assert_eq!(route(caps, h), route(caps, h));
    }
}
