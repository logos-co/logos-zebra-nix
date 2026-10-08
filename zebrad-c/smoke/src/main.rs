//! Smoke test for libzebrad_c: loads the library, runs a regtest node and calls its
//! lightwalletd service in memory, then over TCP with `exposeRpc`.
//!
//! Usage: zebrad-c-smoke <path to libzebrad_c>

use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use prost::Message;

// The client types zebra-rpc generates from the lightwalletd proto, as zebra-rpc builds them.
#[allow(clippy::all, dead_code, missing_docs)]
mod wire {
    include!("../../../zebra/zebra-rpc/proto/__generated__/cash.z.wallet.sdk.rpc.rs");
}

use wire::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, ChainSpec,
    CompactBlock, Empty, LightdInfo,
};

const SERVICE: &str = "/cash.z.wallet.sdk.rpc.CompactTxStreamer";
const STOPPED: c_int = 0;
const RUNNING: c_int = 2;
const FAILED: c_int = 4;
const NOT_FOUND: c_int = 5;
const UNIMPLEMENTED: c_int = 12;
const UNAVAILABLE: c_int = 14;
const STOP_BUDGET: Duration = Duration::from_secs(10);
/// Zebra names every chain but Mainnet "test", as its getblockchaininfo does.
const CHAIN_NAME: &str = "test";
/// Regtest's genesis block hash, in display order.
const REGTEST_GENESIS: &str = "029f11d80ef9765602235e1bc9727e3eb6ba20839319f761fee920d63401e327";
/// Zebra's own deterministic Regtest miner address (secret key 1).
const MINER: &str = "tmLPctKo9j49rtCSKpwEBpLBeykiTGomGQs";

struct Lib {
    start: unsafe extern "C" fn(*const c_char) -> c_int,
    stop: unsafe extern "C" fn(),
    state: unsafe extern "C" fn() -> c_int,
    status_json: unsafe extern "C" fn() -> *const c_char,
    last_error: unsafe extern "C" fn() -> *const c_char,
    free: unsafe extern "C" fn(*const c_char),
    version: unsafe extern "C" fn() -> *const c_char,
    grpc: unsafe extern "C" fn(*const c_char, *const u8, usize, *mut *mut u8, *mut usize) -> c_int,
    free_bytes: unsafe extern "C" fn(*mut u8, usize),
}

fn dlerror() -> String {
    let e = unsafe { libc::dlerror() };
    if e.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
    }
}

/// Looks `name` up in `handle` as a `T`, which must be the function's real pointer type.
unsafe fn symbol<T>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let c_name = CString::new(name).unwrap();
    let s = unsafe { libc::dlsym(handle, c_name.as_ptr()) };
    if s.is_null() {
        return Err(format!("{name} is not exported"));
    }
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&s) })
}

impl Lib {
    fn load(path: &str) -> Result<Lib, String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        let h = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            return Err(format!("dlopen {path}: {}", dlerror()));
        }
        unsafe {
            Ok(Lib {
                start: symbol(h, "ZEBRAD_start")?,
                stop: symbol(h, "ZEBRAD_stop")?,
                state: symbol(h, "ZEBRAD_state")?,
                status_json: symbol(h, "ZEBRAD_status_json")?,
                last_error: symbol(h, "ZEBRAD_last_error")?,
                free: symbol(h, "ZEBRAD_free")?,
                version: symbol(h, "ZEBRAD_version")?,
                grpc: symbol(h, "ZEBRAD_grpc")?,
                free_bytes: symbol(h, "ZEBRAD_free_bytes")?,
            })
        }
    }

    fn take(&self, s: *const c_char) -> String {
        let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
        unsafe { (self.free)(s) };
        text
    }

    fn version(&self) -> String {
        self.take(unsafe { (self.version)() })
    }

    fn last_error(&self) -> String {
        self.take(unsafe { (self.last_error)() })
    }

    fn state(&self) -> c_int {
        unsafe { (self.state)() }
    }

    fn status(&self) -> serde_json::Value {
        serde_json::from_str(&self.take(unsafe { (self.status_json)() })).expect("status is JSON")
    }

    fn start(&self, options: &str) -> Result<(), String> {
        let c = CString::new(options).unwrap();
        match unsafe { (self.start)(c.as_ptr()) } {
            0 => Ok(()),
            rc => Err(format!("start returned {rc}: {}", self.last_error())),
        }
    }

    /// Stops the node and returns how long the stop took.
    fn stop(&self) -> Result<Duration, String> {
        let t0 = Instant::now();
        unsafe { (self.stop)() };
        let took = t0.elapsed();
        ensure(took <= STOP_BUDGET, format!("stop took {took:?}"))?;
        ensure(
            self.state() == STOPPED,
            format!("state {} after stop", self.state()),
        )?;
        Ok(took)
    }

    /// One in-memory call: the grpc-status, the response frames, and the error message.
    fn call(&self, method: &str, request: &impl Message) -> (c_int, Vec<u8>, String) {
        let path = CString::new(format!("{SERVICE}/{method}")).unwrap();
        let body = frame(request);
        let (mut out, mut out_len) = (std::ptr::null_mut(), 0usize);
        let code = unsafe {
            (self.grpc)(
                path.as_ptr(),
                body.as_ptr(),
                body.len(),
                &mut out,
                &mut out_len,
            )
        };
        let bytes = if out.is_null() {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(out, out_len) }.to_vec()
        };
        unsafe { (self.free_bytes)(out, out_len) };
        let message = if code == 0 {
            String::new()
        } else {
            self.last_error()
        };
        (code, bytes, message)
    }

    fn unary<T: Message + Default>(
        &self,
        method: &str,
        request: &impl Message,
    ) -> Result<T, String> {
        let (code, bytes, message) = self.call(method, request);
        ensure(
            code == 0,
            format!("{method}: grpc-status {code}: {message}"),
        )?;
        decode_one(method, &bytes)
    }
}

fn decode_one<T: Message + Default>(method: &str, bytes: &[u8]) -> Result<T, String> {
    match unframe(bytes)?.as_slice() {
        [one] => T::decode(*one).map_err(|e| format!("{method}: {e}")),
        frames => Err(format!("{method}: {} messages, expected 1", frames.len())),
    }
}

/// A gRPC frame: uncompressed, big-endian length, then the message.
fn frame(message: &impl Message) -> Vec<u8> {
    let bytes = message.encode_to_vec();
    let mut out = vec![0];
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&bytes);
    out
}

fn unframe(mut bytes: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut frames = Vec::new();
    while !bytes.is_empty() {
        ensure(bytes.len() >= 5 && bytes[0] == 0, "malformed gRPC frame")?;
        let n = u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize;
        ensure(bytes.len() >= 5 + n, "truncated gRPC frame")?;
        frames.push(&bytes[5..5 + n]);
        bytes = &bytes[5 + n..];
    }
    Ok(frames)
}

fn ensure(ok: bool, why: impl Into<String>) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(why.into())
    }
}

/// Options for a regtest node in `dir`; with `tcp`, it also serves lightwalletd and
/// JSON-RPC on those ports, and mines to a fixed address.
fn options(dir: &Path, tcp: Option<(u16, u16)>, expose_rpc: bool) -> String {
    let quote = |p: &Path| serde_json::to_string(&p.display().to_string()).unwrap();
    let mut config = format!(
        "[network]\nnetwork = \"Regtest\"\nlisten_addr = \"127.0.0.1:0\"\ncache_dir = false\n\n\
         [state]\ncache_dir = {}\n",
        quote(&dir.join("state"))
    );
    if let Some((lightwalletd, json_rpc)) = tcp {
        config += &format!(
            "\n[rpc]\nlightwalletd_listen_addr = \"127.0.0.1:{lightwalletd}\"\nlisten_addr = \"127.0.0.1:{json_rpc}\"\n\
             enable_cookie_auth = false\ncookie_dir = {}\n\n[mining]\nminer_address = \"{MINER}\"\n",
            quote(dir)
        );
    }
    let mut options = serde_json::json!({
        "config": config,
        "logFile": dir.join("zebrad.log").display().to_string(),
        "logFilter": "info",
    });
    if expose_rpc {
        options["exposeRpc"] = true.into();
    }
    options.to_string()
}

/// One JSON-RPC call over HTTP/1.1.
fn json_rpc(
    port: u16,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
        .to_string();
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|e| format!("{method}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;
    write!(
        stream,
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("{method}: {e}"))?;
    let json = response.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    let reply: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("{method}: {e}: {response}"))?;
    match reply.get("error") {
        Some(e) if !e.is_null() => Err(format!("{method}: {e}")),
        _ => Ok(reply["result"].clone()),
    }
}

/// GetBlockRange for `start..=end`: its grpc-status and message, and the heights of the
/// blocks in its frames.
fn block_range(lib: &Lib, start: u64, end: u64) -> Result<(c_int, String, Vec<u64>), String> {
    let id = |height| {
        Some(BlockId {
            height,
            hash: vec![],
        })
    };
    let (code, bytes, message) = lib.call(
        "GetBlockRange",
        &BlockRange {
            start: id(start),
            end: id(end),
        },
    );
    let heights = unframe(&bytes)?
        .into_iter()
        .map(|f| {
            CompactBlock::decode(f)
                .map(|b| b.height)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    Ok((code, message, heights))
}

fn wait_running(lib: &Lib) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match lib.state() {
            RUNNING => return Ok(()),
            FAILED => return Err(format!("node failed: {}", lib.status()["lastError"])),
            _ if Instant::now() > deadline => return Err("node not running after 120 s".into()),
            _ => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The service is handed over before the regtest genesis block is committed.
fn wait_tip(lib: &Lib) -> Result<BlockId, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (code, bytes, message) = lib.call("GetLatestBlock", &ChainSpec {});
        if code != UNAVAILABLE || Instant::now() > deadline {
            ensure(
                code == 0,
                format!("GetLatestBlock: grpc-status {code}: {message}"),
            )?;
            return decode_one("GetLatestBlock", &bytes);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// A block hash as Zebra displays it: byte-reversed hex.
fn display_hash(hash: &[u8]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run(lib: &Lib, dir: &Path) -> Result<(), String> {
    // 1. In memory, with no TCP server.
    lib.start(&options(dir, None, false))?;
    wait_running(lib)?;
    let tip = wait_tip(lib)?;
    let info: LightdInfo = lib.unary("GetLightdInfo", &Empty {})?;
    ensure(
        info.chain_name == CHAIN_NAME,
        format!("chain {:?}, expected {CHAIN_NAME:?}", info.chain_name),
    )?;
    let genesis = display_hash(&tip.hash);
    ensure(
        tip.height == 0 && genesis == REGTEST_GENESIS,
        format!("tip {} {genesis}, expected Regtest's genesis", tip.height),
    )?;
    let block: CompactBlock = lib.unary(
        "GetBlock",
        &BlockId {
            height: 0,
            hash: tip.hash.clone(),
        },
    )?;
    ensure(
        block.height == 0,
        format!("GetBlock by genesis hash gave height {}", block.height),
    )?;
    let status = lib.status();
    ensure(
        status["network"] == "Regtest",
        format!("network {}", status["network"]),
    )?;
    ensure(
        status["rpcExposed"]["lightwalletd"] == "" && status["rpcExposed"]["jsonRpc"] == "",
        format!("ports: {status}"),
    )?;
    println!(
        "in memory: GetLightdInfo chain={} blockHeight={} version={}; GetLatestBlock height={} hash={genesis} \
         (Regtest genesis); GetBlock by that hash OK; no TCP server",
        info.chain_name, info.block_height, info.version, tip.height
    );
    let (code, _, message) = lib.call("GetMempoolStream", &Empty {});
    ensure(
        code == UNIMPLEMENTED,
        format!("GetMempoolStream: grpc-status {code} ({message}), expected 12"),
    )?;
    println!("in memory: GetMempoolStream -> {code} UNIMPLEMENTED ({message})");

    let failures: usize = thread::scope(|s| {
        let callers: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    (0..25)
                        .filter(|_| lib.call("GetLatestBlock", &ChainSpec {}).0 != 0)
                        .count()
                })
            })
            .collect();
        callers.into_iter().map(|c| c.join().unwrap()).sum()
    });
    ensure(
        failures == 0,
        format!("{failures} of 200 concurrent calls failed"),
    )?;
    println!("in memory: 8 threads x 25 concurrent GetLatestBlock, all OK");

    let took = lib.stop()?;
    let (code, _, message) = lib.call("GetLightdInfo", &Empty {});
    ensure(
        code == UNAVAILABLE,
        format!("after stop: grpc-status {code} ({message}), expected 14"),
    )?;
    println!(
        "stop: {} ms; then GetLightdInfo -> {code} UNAVAILABLE ({message})",
        took.as_millis()
    );

    // 2. Zebra's TCP servers, only with exposeRpc.
    let (lwd, rpc) = (free_port(), free_port());
    let refused = lib.start(&options(dir, Some((lwd, rpc)), false));
    ensure(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("exposeRpc") && e.contains("rpc.listen_addr"))
            && lib.state() == STOPPED,
        format!("TCP servers without exposeRpc: {refused:?}"),
    )?;
    println!("without exposeRpc: {}", refused.unwrap_err());

    lib.start(&options(dir, Some((lwd, rpc)), true))?;
    wait_running(lib)?;
    let exposed = &lib.status()["rpcExposed"];
    ensure(
        exposed["lightwalletd"] == format!("127.0.0.1:{lwd}").as_str()
            && exposed["jsonRpc"] == format!("127.0.0.1:{rpc}").as_str(),
        format!("rpcExposed: {exposed}"),
    )?;
    let mined = json_rpc(rpc, "generate", serde_json::json!([2]))?;
    ensure(
        mined.as_array().is_some_and(|a| a.len() == 2),
        format!("generate gave {mined}"),
    )?;
    let (code, message, heights) = block_range(lib, 1, 2)?;
    ensure(
        code == 0 && heights == [1, 2],
        format!("GetBlockRange 1..=2: {code} {message} {heights:?}"),
    )?;
    let (code, message, partial) = block_range(lib, 1, 3)?;
    ensure(
        code == NOT_FOUND && partial == [1, 2],
        format!("GetBlockRange 1..=3: {code} {message} {partial:?}"),
    )?;
    println!(
        "exposeRpc: JSON-RPC generate mined 2 blocks; in memory, GetBlockRange 1..=2 -> frames {heights:?}; \
         1..=3 -> frames {partial:?}, then {code} NOT_FOUND ({message})"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let info = rt.block_on(async {
        let mut client = CompactTxStreamerClient::connect(format!("http://127.0.0.1:{lwd}"))
            .await
            .map_err(|e| e.to_string())?;
        client
            .get_lightd_info(Empty {})
            .await
            .map(|r| r.into_inner())
            .map_err(|e| e.to_string())
    })?;
    ensure(
        info.chain_name == CHAIN_NAME && info.block_height == 2,
        format!("over TCP: {info:?}"),
    )?;
    println!(
        "exposeRpc: GetLightdInfo over TCP 127.0.0.1:{lwd} chain={} blockHeight={}",
        info.chain_name, info.block_height
    );

    let took = lib.stop()?;
    for port in [lwd, rpc] {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        ensure(
            TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_err(),
            format!("port {port} open after stop"),
        )?;
    }
    println!("stop: {} ms; both ports closed", took.as_millis());
    Ok(())
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: zebrad-c-smoke <path to libzebrad_c>");
        return ExitCode::from(2);
    };
    let lib = match Lib::load(&path) {
        Ok(lib) => lib,
        Err(e) => {
            eprintln!("FAIL: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("loaded: {}", lib.version());
    let dir = std::env::temp_dir().join(format!("zebrad-c-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let result = run(&lib, &dir);
    unsafe { (lib.stop)() };
    if let Err(e) = &result {
        eprintln!("FAIL: {e}");
        if let Ok(log) = std::fs::read_to_string(dir.join("zebrad.log")) {
            let lines: Vec<&str> = log.lines().collect();
            eprintln!(
                "--- zebrad.log (last 40 lines)\n{}",
                lines[lines.len().saturating_sub(40)..].join("\n")
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(()) => {
            println!("PASS");
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::FAILURE,
    }
}
