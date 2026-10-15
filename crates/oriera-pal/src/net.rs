//! Network boundary (L1).
//!
//! Contract: `oriera.pal.net.v1` (Table B-1: RouteRemoteTask — privacy
//! and latency constraints are decided here, upper layers only see the
//! plan). WebTransport/QUIC preferred; WebSocket as fallback.

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
}
