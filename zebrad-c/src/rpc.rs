//! The node's JSON-RPC, called in memory by `ZEBRAD_rpc`.

use std::{
    panic::AssertUnwindSafe,
    sync::{mpsc, Mutex, MutexGuard},
    time::Duration,
};

use futures::FutureExt;
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::runtime::Handle;
use tower::ServiceExt;
use zebra_rpc::server::InMemoryService;

/// Bounds each call.
const TIMEOUT: Duration = Duration::from_secs(120);

const NOT_RUNNING: i32 = -9;
const STARTING: i32 = -28;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INTERNAL_ERROR: i32 = -32603;

/// Read the chain, the mempool and the node's peers, or relay a signed transaction.
const EVERY_NETWORK: &[&str] = &[
    "getinfo",
    "getdeprecationinfo",
    "getblockchaininfo",
    "getaddressbalance",
    "sendrawtransaction",
    "getblock",
    "getblockheader",
    "getbestblockhash",
    "getbestblockheightandhash",
    "getmempoolinfo",
    "getrawmempool",
    "z_gettreestate",
    "z_getsubtreesbyindex",
    "getrawtransaction",
    "getaddresstxids",
    "getaddressutxos",
    "getblockcount",
    "getblockhash",
    "getmininginfo",
    "getnetworksolps",
    "getnetworkhashps",
    "getnetworkinfo",
    "getpeerinfo",
    "ping",
    "validateaddress",
    "z_validateaddress",
    "z_converttex",
    "getstandardfee",
    "getblocksubsidy",
    "getdifficulty",
    "z_listunifiedreceivers",
    "gettxout",
];

/// Mine, rewrite the chain or change the node's peers: for a chain of the caller's own.
const REGTEST_ONLY: &[&str] = &[
    "generate",
    "generatetoaddress",
    "submitblock",
    "getblocktemplate",
    "invalidateblock",
    "reconsiderblock",
    "addnode",
];

struct Endpoint {
    service: InMemoryService,
    runtime: Handle,
    regtest: bool,
}

static ENDPOINT: Mutex<Option<Endpoint>> = Mutex::new(None);

fn endpoint() -> MutexGuard<'static, Option<Endpoint>> {
    ENDPOINT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serves calls with `service` on the node's `runtime` until [`close`].
pub fn open(service: InMemoryService, runtime: Handle, regtest: bool) {
    *endpoint() = Some(Endpoint {
        service,
        runtime,
        regtest,
    });
}

/// Refuses new calls; calls in flight end when the node's runtime shuts down.
pub fn close() {
    endpoint().take();
}

/// The reply to a call that never reached the node.
pub fn refusal(code: i32, message: &str) -> String {
    serde_json::json!({ "result": null, "error": { "code": code, "message": message } })
        .to_string()
}

/// Makes one call on the node's runtime and blocks until its reply is in. The method is checked
/// here, against the chain this node runs, so a restart cannot come between the two.
pub fn call(method: &str, params: Option<&str>, starting: bool) -> String {
    let regtest_only = REGTEST_ONLY.contains(&method);
    if !regtest_only && !EVERY_NETWORK.contains(&method) {
        return refusal(METHOD_NOT_FOUND, &format!("method not allowed: {method}"));
    }
    let params = params.filter(|p| !p.trim().is_empty()).unwrap_or("[]");
    match serde_json::from_str::<serde_json::Value>(params) {
        Ok(p) if p.is_array() || p.is_object() => {}
        _ => return refusal(INVALID_REQUEST, "params must be a JSON array or object"),
    }
    let Some((service, runtime, regtest)) = endpoint()
        .as_ref()
        .map(|e| (e.service.clone(), e.runtime.clone(), e.regtest))
    else {
        return if starting {
            refusal(STARTING, "the node is starting")
        } else {
            refusal(NOT_RUNNING, "the node is not running")
        };
    };
    if regtest_only && !regtest {
        return refusal(METHOD_NOT_FOUND, &format!("{method} is allowed on regtest only"));
    }
    // `method` is on a list above, and `params` just parsed as one JSON value.
    let request = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let (tx, rx) = mpsc::sync_channel(1);
    runtime.spawn(async move {
        let reply = AssertUnwindSafe(exchange(service, request))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| refusal(INTERNAL_ERROR, "the call panicked"));
        let _ = tx.send(reply);
    });
    // The task's own timeout answers first unless the runtime is stuck.
    match rx.recv_timeout(TIMEOUT + Duration::from_secs(5)) {
        Ok(reply) => reply,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            refusal(INTERNAL_ERROR, "the node did not run the call")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            refusal(NOT_RUNNING, "the node stopped during the call")
        }
    }
}

#[derive(Deserialize)]
struct Response<'a> {
    #[serde(borrow, default)]
    result: Option<&'a RawValue>,
    #[serde(borrow, default)]
    error: Option<&'a RawValue>,
}

/// Runs one call and keeps the node's `result` and `error` as it wrote them.
async fn exchange(service: InMemoryService, request: String) -> String {
    let response = match tokio::time::timeout(TIMEOUT, service.oneshot(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => return refusal(INTERNAL_ERROR, &e.to_string()),
        Err(_) => {
            let late = format!("no response within {} s", TIMEOUT.as_secs());
            return refusal(INTERNAL_ERROR, &late);
        }
    };
    match serde_json::from_str::<Response>(&response) {
        Ok(reply) => format!(
            r#"{{"result":{},"error":{}}}"#,
            reply.result.map_or("null", RawValue::get),
            reply.error.map_or("null", RawValue::get)
        ),
        Err(_) => {
            let text: String = response.chars().take(200).collect();
            refusal(INTERNAL_ERROR, &format!("unreadable response: {text}"))
        }
    }
}
