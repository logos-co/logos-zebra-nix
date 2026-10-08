//! The node's lightwalletd service, called in memory by `ZEBRAD_grpc`.

use std::{
    panic::AssertUnwindSafe,
    sync::{mpsc, Mutex, MutexGuard},
    time::Duration,
};

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::{BodyExt, Full};
use tokio::{runtime::Handle, time::Instant};
use tonic::{body::Body, codegen::http, Code, Status};
use tower::ServiceExt;
use zebra_rpc::lightwalletd::server::BoxedService;

/// Bounds each call, the whole response included.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Bounds a response's size, since it is held in memory whole.
const MAX_RESPONSE: usize = 256 << 20;

/// Stays open until the next block, which a blocking call cannot wait for.
const MEMPOOL_STREAM: &str = "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetMempoolStream";

struct Endpoint {
    service: BoxedService,
    runtime: Handle,
}

static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);

pub struct Reply {
    pub code: i32,
    pub message: String,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Reply {
            code: code as i32,
            message: message.into(),
            body: Vec::new(),
        }
    }
}

fn endpoint() -> MutexGuard<'static, Option<Endpoint>> {
    ENDPOINT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serves calls with `service` on the node's `runtime` until [`close`].
pub fn open(service: BoxedService, runtime: Handle) {
    *endpoint() = Some(Endpoint { service, runtime });
}

/// Refuses new calls; calls in flight end when the node's runtime shuts down.
pub fn close() {
    endpoint().take();
}

/// Makes one call on the node's runtime, and blocks until its whole response is in.
pub fn call(path: &str, body: Vec<u8>) -> Reply {
    if path == MEMPOOL_STREAM {
        return Reply::error(
            Code::Unimplemented,
            "GetMempoolStream stays open until the next block; poll GetMempoolTx instead",
        );
    }
    let Some((service, runtime)) = endpoint()
        .as_ref()
        .map(|e| (e.service.clone(), e.runtime.clone()))
    else {
        return Reply::error(Code::Unavailable, "the node is not running");
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let path = path.to_owned();
    runtime.spawn(async move {
        let reply = AssertUnwindSafe(exchange(service, path, body))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Reply::error(Code::Internal, "the call panicked"));
        let _ = tx.send(reply);
    });
    // The task's own timeout answers first unless the runtime is stuck.
    match rx.recv_timeout(TIMEOUT + Duration::from_secs(5)) {
        Ok(reply) => reply,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Reply::error(Code::DeadlineExceeded, "the node did not run the call")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Reply::error(Code::Unavailable, "the node stopped during the call")
        }
    }
}

/// Runs one call and collects its response; one that stops early keeps the whole
/// messages that arrived, so a long range can resume after them.
async fn exchange(service: BoxedService, path: String, body: Vec<u8>) -> Reply {
    let deadline = Instant::now() + TIMEOUT;
    let late = || format!("no complete response within {} s", TIMEOUT.as_secs());
    let request = http::Request::builder()
        .method(http::Method::POST)
        .uri(path)
        .header(http::header::CONTENT_TYPE, "application/grpc")
        .header(http::header::TE, "trailers")
        .body(Body::new(Full::new(Bytes::from(body))));
    let request = match request {
        Ok(request) => request,
        Err(e) => return Reply::error(Code::InvalidArgument, format!("path: {e}")),
    };
    let response = match tokio::time::timeout_at(deadline, service.oneshot(request)).await {
        Ok(Ok(response)) => response,
        Err(_) => return Reply::error(Code::DeadlineExceeded, late()),
    };
    let (head, mut body) = response.into_parts();
    let mut out = Vec::new();
    let mut trailers = None;
    let stopped = |code: Code, message: String, out: Vec<u8>| Reply {
        code: code as i32,
        message,
        body: out,
    };
    loop {
        let frame = match tokio::time::timeout_at(deadline, body.frame()).await {
            Err(_) => return stopped(Code::DeadlineExceeded, late(), out),
            Ok(None) => break,
            Ok(Some(Err(status))) => {
                return stopped(status.code(), status.message().to_owned(), out)
            }
            Ok(Some(Ok(frame))) => frame,
        };
        match frame.into_data() {
            // tonic yields whole messages per chunk, so `out` stays a sequence of frames.
            Ok(data) if out.len() + data.len() > MAX_RESPONSE => {
                let why = format!(
                    "the response passed {} MiB; ask for less",
                    MAX_RESPONSE >> 20
                );
                return stopped(Code::ResourceExhausted, why, out);
            }
            Ok(data) => out.extend_from_slice(&data),
            Err(frame) => trailers = frame.into_trailers().ok(),
        }
    }
    // A call that fails before its first message is trailers-only: the status is a header.
    let status = trailers
        .as_ref()
        .and_then(Status::from_header_map)
        .or_else(|| Status::from_header_map(&head.headers))
        .unwrap_or_else(|| Status::unknown("the response has no grpc-status"));
    stopped(status.code(), status.message().to_owned(), out)
}
