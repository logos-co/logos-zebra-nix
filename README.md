# logos-zebra-nix

Zebra, the Zcash node, patched to run in-process and built by Nix as a C library,
`libzebrad_c`. It is to Zcash what `logos-monero-nix`'s `libmonerod_c` is to Monero.

```
packages.<system>.default         # libzebrad_c: lib/libzebrad_c.{dylib,so}, include/zebrad_c.h
packages.<system>.zebra-src       # Zebra v7.0.0-rc.0 (by tag, fixed hash) + patches/zebra
packages.<system>.zebrad-c-smoke  # the smoke test binary
checks.<system>.patches           # the patch guard
checks.<system>.smoke             # the smoke test, against the built library
```

Systems: `aarch64-darwin`, `x86_64-darwin`, `aarch64-linux`, `x86_64-linux`. nixpkgs is
logos-nix's (the pin logos-module-builder follows); Rust is 1.91.0, Zebra's own toolchain,
from rust-overlay.

## Patches

| Patch | What | Why |
|---|---|---|
| 0001 | `StartCmd::run_node(config, …)` | Start the node with a caller's config, without Abscissa's launcher. |
| 0002 | End of support returns an error | The halt stops the node, not the host process. |
| 0003 | `zebra_consensus::init_batch_verifiers()` | Verifiers on a runtime that outlives each node, so the node restarts in-process. |
| 0004 | `lightwalletd::server::service()`, and a `run_node` sink for it | The lightwalletd gRPC service, in memory and without a port. Zebra's own docs warn against its TCP port on end users' machines. |

`checks.<system>.patches` runs `ci/check-patches.sh`, which fails when:

- a patch touches a file outside its allowlist: `zebrad/src/commands/start.rs`, the
  end-of-support file, `zebra-rpc/src/lightwalletd/server.rs`, and for 0003 only,
  `zebra-consensus/src/{lib,primitives}.rs`. Nothing in zebra-chain, zebra-state,
  zebra-network, zebra-script or the consensus rules changes;
- `zebrad-c/Cargo.lock` disagrees with upstream's `Cargo.lock` on the version or checksum
  of any package both contain.

## C API

`zebrad-c/include/zebrad_c.h`. Every string returned is the caller's, freed with `ZEBRAD_free`.

| Function | |
|---|---|
| `ZEBRAD_start(options_json)` | Starts the node on its own thread. `{"config": "<zebrad TOML>", "logFile", "logFilter", "exposeRpc"}`. Refuses `debug_stop_at_height`, zcashd compatibility, block notify commands, and TCP servers without `exposeRpc`. |
| `ZEBRAD_stop()` | Stops and joins the node. Idempotent; any thread. |
| `ZEBRAD_state()` | stopped, starting, running (gRPC answers), stopping, failed. |
| `ZEBRAD_status_json()` | `state`, `network`, `uptimeSecs`, `cacheDir`, `height`, `finalizedHeight`, `estimatedHeight`, `peers`, `rpcExposed`, `lastStopMs`, `version`, `lastError`. -1 until known. |
| `ZEBRAD_grpc(path, body, len, &out, &out_len)` | One lightwalletd call, in memory. Returns its grpc-status. |
| `ZEBRAD_free_bytes(out, out_len)` | Frees `ZEBRAD_grpc`'s output. |
| `ZEBRAD_last_error()`, `ZEBRAD_free(s)`, `ZEBRAD_version()` | |

`ZEBRAD_grpc` takes the full gRPC path (`/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlockRange`)
and the request as gRPC frames, exactly as a client would send them. It blocks until the
response ends and returns its frames: all of a server stream's, or those that arrived before
an error, so a long range can resume after the last block it got. It answers UNAVAILABLE (14)
while the node is not running, DEADLINE_EXCEEDED (4) at 120 s, RESOURCE_EXHAUSTED (8) past
256 MiB, and UNIMPLEMENTED (12) for `GetMempoolStream`, which only ends at the next block;
poll `GetMempoolTx` instead. It can be called from any thread, concurrently. Its error
message is per thread: `ZEBRAD_last_error()` on the calling thread, right after the call.

The library exports exactly these nine functions (`zebrad_c.exp`, `zebrad_c.map`). rustc
exports every `#[no_mangle]` symbol in the crate graph, including four from secp256k1-sys,
so the final link swaps rustc's export list for ours, and the build fails if anything else
is exported (`ci/check-exports.sh`).

## Security: no port by default

The wallet reaches the node over Logos IPC: the zebrad module passes the gRPC bytes to
`ZEBRAD_grpc` and returns the response. Nothing listens, so only the module's own process
can query the node or submit transactions through it.

Zebra's TCP servers (`rpc.listen_addr`, `rpc.lightwalletd_listen_addr`,
`rpc.indexer_listen_addr`, `health.listen_addr`) are refused unless the options set
`"exposeRpc": true`, and each start that opens them logs a warning. They serve plaintext,
and the lightwalletd one has no authentication: any local process, and any web page in a
browser on the machine, can reach a loopback port. Use `exposeRpc` for test harnesses, such
as JSON-RPC `generate` on regtest, or for a node someone operates deliberately. The P2P
listener (`network.listen_addr`) is the node's job and is not affected.

## Build and test

```sh
nix build                     # result/lib/libzebrad_c.dylib (.so on Linux)
nix flake check -L            # the patch guard and the smoke test
nix run .#zebrad-c-smoke -- result/lib/libzebrad_c.dylib
```

The smoke test loads the library with `dlopen` and starts a regtest node in a temporary
directory. In memory, with nothing listening, it calls `GetLatestBlock` (Regtest's genesis),
`GetLightdInfo` and `GetBlock`, checks that `GetMempoolStream` is refused, and makes 200
calls from 8 threads at once. It stops the node and checks that a call now answers
UNAVAILABLE. Then it checks that TCP servers are refused without `exposeRpc`. With it, it
mines two blocks over JSON-RPC and streams them back in memory with `GetBlockRange` (1..=3
gives both blocks, then NOT_FOUND), calls `GetLightdInfo` over TCP, and checks that both
ports close on stop. Zebra names Regtest's chain `test`, as its `getblockchaininfo` does.

To work on `zebrad-c` with cargo, link the patched tree where its manifest expects it:

```sh
nix build .#zebra-src -o zebra
cd zebrad-c && cargo build --release
```

A plain cargo build does not apply the export list; the Nix build does.
