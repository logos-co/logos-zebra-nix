//! Zebra's node, run in-process, behind a small C ABI shaped like `monerod_c`.
//!
//! Every `const char*` returned is owned by the caller and released with `ZEBRAD_free`.
//! The node runs on its own thread and tokio runtime; a stop drops both.

use std::{
    cell::RefCell,
    ffi::{c_char, CStr, CString},
    fs::OpenOptions,
    net::SocketAddr,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicI32, AtomicI64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tokio::sync::oneshot;
use tonic::Code;
use zebrad::{
    commands::{start::LightwalletdServiceSink, StartCmd},
    config::ZebradConfig,
};

mod gauges;
mod grpc;

pub const STOPPED: i32 = 0;
pub const STARTING: i32 = 1;
pub const RUNNING: i32 = 2;
pub const STOPPING: i32 = 3;
pub const FAILED: i32 = 4;

/// How long a stop waits for the node's tasks, the same budget Zebra's launcher uses.
const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);

static STATE: AtomicI32 = AtomicI32::new(STOPPED);
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());
static LAST_STOP_MS: AtomicI64 = AtomicI64::new(-1);
static NODE: Mutex<Option<Node>> = Mutex::new(None);
/// Serializes start and stop, so a start waits for a stop still joining its node.
static LIFECYCLE: Mutex<()> = Mutex::new(());
static GLOBALS: OnceLock<Globals> = OnceLock::new();

thread_local! {
    /// Why this thread's last start or gRPC call failed, until its next call in.
    static CALL_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

struct Node {
    thread: JoinHandle<()>,
    stop_tx: Option<oneshot::Sender<()>>,
    started: Instant,
    network: String,
    cache_dir: String,
    exposed: Exposed,
}

/// Created on the first start and never dropped.
struct Globals {
    // Owns the batch verifiers' worker tasks, so they outlive each node's runtime.
    _verifier_rt: tokio::runtime::Runtime,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Options {
    /// `ZebradConfig` as TOML.
    config: String,
    /// Tracing is installed once per process, so only the first start's file is used.
    log_file: Option<String>,
    log_filter: Option<String>,
    /// Lets the config open Zebra's TCP servers, which are refused otherwise.
    #[serde(default)]
    expose_rpc: bool,
}

/// The TCP servers a config opens besides P2P, each "" when off.
#[derive(Clone, Default)]
struct Exposed {
    json_rpc: String,
    lightwalletd: String,
    indexer: String,
    health: String,
}

impl Exposed {
    fn of(config: &ZebradConfig) -> Self {
        let addr = |a: Option<SocketAddr>| a.map(|a| a.to_string()).unwrap_or_default();
        Exposed {
            json_rpc: addr(config.rpc.listen_addr),
            lightwalletd: addr(config.rpc.lightwalletd_listen_addr),
            indexer: addr(config.rpc.indexer_listen_addr),
            health: addr(config.health.listen_addr),
        }
    }

    /// The settings that open them, by their config names.
    fn settings(&self) -> Vec<&'static str> {
        [
            ("rpc.listen_addr", &self.json_rpc),
            ("rpc.lightwalletd_listen_addr", &self.lightwalletd),
            ("rpc.indexer_listen_addr", &self.indexer),
            ("health.listen_addr", &self.health),
        ]
        .into_iter()
        .filter(|(_, addr)| !addr.is_empty())
        .map(|(name, _)| name)
        .collect()
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "lightwalletd": self.lightwalletd,
            "jsonRpc": self.json_rpc,
            "indexer": self.indexer,
            "health": self.health,
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records why the node failed.
fn set_error(msg: impl Into<String>) {
    *lock(&LAST_ERROR) = msg.into();
}

fn set_call_error(msg: String) {
    CALL_ERROR.with(|e| *e.borrow_mut() = Some(msg));
}

fn clear_call_error() {
    CALL_ERROR.with(|e| e.borrow_mut().take());
}

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with a non-string payload".into()
    }
}

fn to_c(s: String) -> *const c_char {
    CString::new(s.replace('\0', " "))
        .map(|c| c.into_raw() as *const c_char)
        .unwrap_or(ptr::null())
}

fn init_globals(opts: &Options) -> Result<(), String> {
    if GLOBALS.get().is_some() {
        return Ok(());
    }
    if let Some(path) = &opts.log_file {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("log file {path}: {e}"))?;
        let filter = opts.log_filter.clone().unwrap_or_else(|| "info".into());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .try_init();
    }
    let _ = metrics::set_global_recorder(gauges::Recorder);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    // rayon aborts the process on a job panic unless a handler is set.
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("zebra-rayon-{i}"))
        .panic_handler(|p| {
            set_error(format!(
                "panic in a verification job: {}",
                panic_message(&*p)
            ))
        })
        .build_global();
    let verifier_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("zebra-verifiers")
        .enable_all()
        .build()
        .map_err(|e| format!("verifier runtime: {e}"))?;
    verifier_rt.block_on(async { zebra_consensus::init_batch_verifiers() });
    let _ = GLOBALS.set(Globals {
        _verifier_rt: verifier_rt,
    });
    Ok(())
}

/// Refuses settings that exit the process or spawn other processes, and TCP servers
/// the caller did not opt into with `exposeRpc`.
fn validate(config: &ZebradConfig, expose_rpc: bool) -> Result<Exposed, String> {
    if config.state.debug_stop_at_height.is_some() {
        return Err("state.debug_stop_at_height exits the process; refused".into());
    }
    if config.zcashd_compat.enabled {
        return Err("zcashd_compat runs a zcashd process; refused".into());
    }
    if config.notify.block_notify_command.is_some() {
        return Err("notify.block_notify_command runs a shell command; refused".into());
    }
    let exposed = Exposed::of(config);
    let settings = exposed.settings();
    if !settings.is_empty() && !expose_rpc {
        return Err(format!(
            "{}: a TCP server any local process can reach; refused unless the options set \"exposeRpc\": true",
            settings.join(", ")
        ));
    }
    Ok(exposed)
}

fn start(options_json: *const c_char) -> Result<(), String> {
    if options_json.is_null() {
        return Err("options are null".into());
    }
    let raw = unsafe { CStr::from_ptr(options_json) }
        .to_str()
        .map_err(|e| format!("options are not UTF-8: {e}"))?;
    let opts: Options = serde_json::from_str(raw).map_err(|e| format!("options: {e}"))?;
    let config: ZebradConfig = toml::from_str(&opts.config).map_err(|e| format!("config: {e}"))?;
    let exposed = validate(&config, opts.expose_rpc)?;

    let _lifecycle = lock(&LIFECYCLE);
    {
        let mut node = lock(&NODE);
        if node.as_ref().is_some_and(|n| !n.thread.is_finished()) {
            return Err("a node is already running in this process".into());
        }
        if let Some(finished) = node.take() {
            let _ = finished.thread.join();
        }
    }
    // Seconds on the first start, so status() is not held behind it.
    init_globals(&opts)?;
    let settings = exposed.settings();
    if !settings.is_empty() {
        tracing::warn!(
            "exposeRpc: serving {} over TCP, reachable by any local process",
            settings.join(", ")
        );
    }

    zebra_chain::shutdown::IS_SHUTTING_DOWN.store(false, Ordering::SeqCst);
    gauges::reset();
    set_error("");
    STATE.store(STARTING, Ordering::SeqCst);
    let (stop_tx, stop_rx) = oneshot::channel();
    let network = config.network.network.to_string();
    let cache_dir = config.state.cache_dir.display().to_string();
    let thread = std::thread::Builder::new()
        .name("zebrad-node".into())
        .spawn(move || node_main(config, stop_rx))
        .map_err(|e| {
            set_error(format!("node thread: {e}"));
            STATE.store(FAILED, Ordering::SeqCst);
            format!("node thread: {e}")
        })?;
    *lock(&NODE) = Some(Node {
        thread,
        stop_tx: Some(stop_tx),
        started: Instant::now(),
        network,
        cache_dir,
        exposed,
    });
    Ok(())
}

fn node_main(config: ZebradConfig, stop_rx: oneshot::Receiver<()>) {
    let outcome = catch_unwind(AssertUnwindSafe(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("zebrad-node")
            .build()
            .map_err(|e| format!("node runtime: {e}"))?;
        let handle = rt.handle().clone();
        // The node is up once it hands over its gRPC service; a racing stop wins.
        let sink: LightwalletdServiceSink = Box::new(move |service| {
            if STATE
                .compare_exchange(STARTING, RUNNING, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                grpc::open(service, handle);
            }
        });
        let config = Arc::new(config);
        let stopped = rt.block_on(async move {
            tokio::select! {
                biased;
                _ = stop_rx => Ok(true),
                result = StartCmd::run_node(config, false, Some(sink)) => {
                    result.map(|()| false).map_err(|e| format!("{e:#}"))
                }
            }
        });
        // New calls are refused; calls in flight end with the runtime.
        grpc::close();
        rt.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        match stopped {
            Ok(true) => Ok(()),
            Ok(false) => Err("the node exited on its own".into()),
            Err(e) => Err(e),
        }
    }));
    grpc::close();
    let state = match outcome {
        Ok(Ok(())) => STOPPED,
        Ok(Err(e)) => {
            set_error(e);
            FAILED
        }
        Err(p) => {
            set_error(format!("node panicked: {}", panic_message(&*p)));
            FAILED
        }
    };
    STATE.store(state, Ordering::SeqCst);
}

/// Signals the node, joins its thread, then releases it. Idempotent; any thread.
fn stop() {
    let _lifecycle = lock(&LIFECYCLE);
    let Some(mut node) = lock(&NODE).take() else {
        return;
    };
    let t0 = Instant::now();
    let _ = STATE.compare_exchange(RUNNING, STOPPING, Ordering::SeqCst, Ordering::SeqCst);
    let _ = STATE.compare_exchange(STARTING, STOPPING, Ordering::SeqCst, Ordering::SeqCst);
    grpc::close();
    // As zebrad's own signal handler does, so tasks exit quietly instead of panicking.
    zebra_chain::shutdown::set_shutting_down();
    if let Some(tx) = node.stop_tx.take() {
        let _ = tx.send(());
    }
    let _ = node.thread.join();
    LAST_STOP_MS.store(t0.elapsed().as_millis() as i64, Ordering::SeqCst);
}

fn state_name(s: i32) -> &'static str {
    match s {
        STOPPED => "stopped",
        STARTING => "starting",
        RUNNING => "running",
        STOPPING => "stopping",
        FAILED => "failed",
        _ => "unknown",
    }
}

fn version() -> String {
    format!(
        "zebrad {} in-process, zebrad_c {}",
        zebrad::application::release_version(),
        env!("CARGO_PKG_VERSION")
    )
}

fn status() -> String {
    let state = STATE.load(Ordering::SeqCst);
    let live = matches!(state, STARTING | RUNNING | STOPPING);
    let (uptime, network, cache_dir, exposed) = lock(&NODE)
        .as_ref()
        .map(|n| {
            (
                n.started.elapsed().as_secs(),
                n.network.clone(),
                n.cache_dir.clone(),
                n.exposed.clone(),
            )
        })
        .unwrap_or_default();
    let (uptime, exposed) = if live {
        (uptime, exposed)
    } else {
        (0, Exposed::default())
    };
    serde_json::json!({
        "state": state_name(state),
        "network": network,
        "uptimeSecs": uptime,
        "cacheDir": cache_dir,
        "height": gauges::TIP.get(),
        "finalizedHeight": gauges::FINALIZED.get(),
        "estimatedHeight": gauges::NETWORK_TIP.get(),
        "peers": gauges::PEERS.get(),
        "rpcExposed": exposed.json(),
        "lastStopMs": LAST_STOP_MS.load(Ordering::SeqCst),
        "version": version(),
        "lastError": lock(&LAST_ERROR).clone(),
    })
    .to_string()
}

/// Starts the node on its own thread and returns at once; see the header.
#[no_mangle]
pub extern "C" fn ZEBRAD_start(options_json: *const c_char) -> i32 {
    clear_call_error();
    match catch_unwind(AssertUnwindSafe(|| start(options_json))) {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            set_call_error(e);
            1
        }
        Err(p) => {
            let e = format!("start panicked: {}", panic_message(&*p));
            set_error(e.clone());
            set_call_error(e);
            STATE.store(FAILED, Ordering::SeqCst);
            2
        }
    }
}

#[no_mangle]
pub extern "C" fn ZEBRAD_stop() {
    clear_call_error();
    if let Err(p) = catch_unwind(stop) {
        set_error(format!("stop panicked: {}", panic_message(&*p)));
    }
}

#[no_mangle]
pub extern "C" fn ZEBRAD_state() -> i32 {
    clear_call_error();
    STATE.load(Ordering::SeqCst)
}

#[no_mangle]
pub extern "C" fn ZEBRAD_status_json() -> *const c_char {
    clear_call_error();
    to_c(
        catch_unwind(status).unwrap_or_else(|p| {
            format!("{{\"error\":\"status panicked: {}\"}}", panic_message(&*p))
        }),
    )
}

#[no_mangle]
pub extern "C" fn ZEBRAD_last_error() -> *const c_char {
    let call = CALL_ERROR.with(|e| e.borrow().clone());
    to_c(call.unwrap_or_else(|| lock(&LAST_ERROR).clone()))
}

#[no_mangle]
pub extern "C" fn ZEBRAD_version() -> *const c_char {
    clear_call_error();
    to_c(catch_unwind(version).unwrap_or_else(|_| "unknown".into()))
}

/// Releases a string returned by this library.
#[no_mangle]
pub extern "C" fn ZEBRAD_free(s: *const c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s as *mut c_char) });
    }
}

fn grpc_call(
    path: *const c_char,
    body: *const u8,
    body_len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> grpc::Reply {
    if out.is_null() || out_len.is_null() {
        return grpc::Reply::error(Code::InvalidArgument, "out and out_len must not be null");
    }
    unsafe {
        *out = ptr::null_mut();
        *out_len = 0;
    }
    if path.is_null() {
        return grpc::Reply::error(Code::InvalidArgument, "path is null");
    }
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else {
        return grpc::Reply::error(Code::InvalidArgument, "path is not UTF-8");
    };
    let body = match (body.is_null(), body_len) {
        (_, 0) => Vec::new(),
        (true, _) => return grpc::Reply::error(Code::InvalidArgument, "body is null"),
        (false, n) => unsafe { std::slice::from_raw_parts(body, n) }.to_vec(),
    };
    let mut reply = grpc::call(path, body);
    if !reply.body.is_empty() {
        let bytes = std::mem::take(&mut reply.body).into_boxed_slice();
        unsafe {
            *out_len = bytes.len();
            *out = Box::into_raw(bytes).cast::<u8>();
        }
    }
    reply
}

/// Calls the node's lightwalletd service in memory and returns the grpc-status; see the header.
#[no_mangle]
pub extern "C" fn ZEBRAD_grpc(
    path: *const c_char,
    body: *const u8,
    body_len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    clear_call_error();
    let reply = catch_unwind(AssertUnwindSafe(|| {
        grpc_call(path, body, body_len, out, out_len)
    }))
    .unwrap_or_else(|p| {
        grpc::Reply::error(
            Code::Internal,
            format!("call panicked: {}", panic_message(&*p)),
        )
    });
    if reply.code != 0 {
        set_call_error(reply.message);
    }
    reply.code
}

/// Releases the bytes `ZEBRAD_grpc` returned.
#[no_mangle]
pub extern "C" fn ZEBRAD_free_bytes(p: *mut u8, len: usize) {
    if !p.is_null() {
        drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(p, len)) });
    }
}
