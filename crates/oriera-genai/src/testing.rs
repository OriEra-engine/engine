//! 共享测试脚手架（评审 R2 架构师 P1：四个适配器各一份逐字复制，约
//! 100 行收编归一）。仅供本 crate 各 `#[cfg(test)]` 模块使用——
//! `pub` 只为子模块可见，不构成 semver 承诺。

use crate::transport::{HttpRequest, HttpResponse, Transport};
use std::sync::{Arc, Mutex};

/// Scripted transport: pops queued responses per request.
pub struct ScriptedTransport {
    pub requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<Vec<HttpResponse>>,
}

impl Transport for ScriptedTransport {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, String> {
        self.requests.lock().unwrap().push(req.clone());
        self.responses
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| "no scripted response".to_string())
    }
}

/// 响应按 LIFO 出队（调用方按「倒序」排队：取件、末次轮询、……、创建）。
pub fn fixture_transport(responses: Vec<HttpResponse>) -> Arc<ScriptedTransport> {
    Arc::new(ScriptedTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(responses),
    })
}
