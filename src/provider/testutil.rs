//! 测试用本地 mock server（不依赖真 API key，也不出 127.0.0.1）。
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{body::Body, extract::Request, http::StatusCode, response::Response};
use serde_json::Value;

#[derive(Clone, Default)]
pub struct Recorder {
    reqs: Arc<Mutex<Vec<Recorded>>>,
}

#[derive(Clone, Debug, Default)]
pub struct Recorded {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|s| s.as_str())
    }

    pub fn json(&self, pointer: &str) -> &Value {
        self.body.pointer(pointer).unwrap_or(&Value::Null)
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn record(&self, req: Request) -> Recorded {
        let path = req.uri().path().to_string();
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let bytes = axum::body::to_bytes(req.into_body(), 4 * 1024 * 1024)
            .await
            .expect("read mock request body");
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let rec = Recorded {
            path,
            headers,
            body,
        };
        self.reqs.lock().expect("recorder lock").push(rec.clone());
        rec
    }

    pub fn last(&self) -> Recorded {
        self.reqs
            .lock()
            .expect("recorder lock")
            .last()
            .cloned()
            .expect("mock server should have captured a request")
    }

    pub fn count(&self) -> usize {
        self.reqs.lock().expect("recorder lock").len()
    }
}

/// 返回一个 `text/event-stream` 响应；额外头用于 429/`retry-after` 之类的场景。
pub fn sse(body: &str, status: StatusCode, extra_headers: &[(&str, &str)]) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header("content-type", "text/event-stream");
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    builder
        .body(Body::from(body.to_string()))
        .expect("build mock sse response")
}

pub fn json(body: &str, status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("build mock json response")
}

/// 在随机端口起 mock server，返回 base url（`http://127.0.0.1:PORT`）。
pub async fn spawn(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let addr: SocketAddr = listener.local_addr().expect("mock server addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}
