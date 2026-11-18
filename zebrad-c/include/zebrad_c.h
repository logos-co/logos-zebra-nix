// C ABI over Zebra's node: the Zcash node, in-process, as a shared library.
// Every returned `const char*` is owned by the caller; release it with ZEBRAD_free().
#ifndef LOGOS_ZEBRAD_C_H
#define LOGOS_ZEBRAD_C_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum ZEBRAD_State {
  ZEBRAD_STOPPED  = 0,
  ZEBRAD_STARTING = 1,
  ZEBRAD_RUNNING  = 2,  // set once ZEBRAD_grpc can answer
  ZEBRAD_STOPPING = 3,
  ZEBRAD_FAILED   = 4
};

// Starts the node on its own thread and returns at once. `options_json` is
// {"config": "<zebrad TOML>", "logFile": "...", "logFilter": "info", "exposeRpc": false}.
// A config that opens a TCP server (rpc.listen_addr, rpc.lightwalletd_listen_addr,
// rpc.indexer_listen_addr, health.listen_addr) is refused unless "exposeRpc" is true.
// Non-zero means refused; see ZEBRAD_last_error().
int ZEBRAD_start(const char* options_json);
// Signals the node, joins its thread, then releases it. Idempotent; any thread.
void ZEBRAD_stop(void);
int ZEBRAD_state(void);
// {"state", "network", "uptimeSecs", "cacheDir", "height", "finalizedHeight",
//  "estimatedHeight", "peers", "rpcExposed": {"lightwalletd", "jsonRpc", "indexer", "health"},
//  "lastStopMs", "version", "lastError"}. Numbers are -1 until known; addresses "" when off.
const char* ZEBRAD_status_json(void);
// Why this thread's last ZEBRAD_start or ZEBRAD_grpc failed, until its next call into the
// library (other than this one and the free functions); else why the node last failed.
const char* ZEBRAD_last_error(void);
void ZEBRAD_free(const char* s);
const char* ZEBRAD_version(void);

// Calls the node's lightwalletd CompactTxStreamer service in memory; no socket is involved.
// `path` is the gRPC path, e.g. "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlockRange";
// `body` is the request as uncompressed gRPC frames (flag 0, 4-byte big-endian length,
// message). Blocks until the response ends, then sets `*out` to its frames: all of a stream's,
// or those that arrived before an error. Returns the grpc-status: 0 is OK; 14 UNAVAILABLE
// while the node is not running; 4 DEADLINE_EXCEEDED at 120 s; 8 RESOURCE_EXHAUSTED past
// 256 MiB; 12 UNIMPLEMENTED for GetMempoolStream, which waits for the next block (poll
// GetMempoolTx). Any thread, concurrently.
int ZEBRAD_grpc(const char* path, const uint8_t* body, size_t body_len,
                uint8_t** out, size_t* out_len);
// Releases ZEBRAD_grpc's `*out`, given its `*out_len`. Null is fine.
void ZEBRAD_free_bytes(uint8_t* p, size_t len);

// Calls the node's JSON-RPC in memory; no socket or cookie is involved. `params_json` is a JSON
// array or object, or null or "" for none. Blocks until the reply, always
// {"result":...,"error":null} or {"result":null,"error":{"code":n,"message":s}}: -9 while the
// node is not running, -28 while it starts, -32601 for a method it may not call, -32600 for
// params of another shape, -32603 at 120 s. Mining and chain-rewriting methods answer on regtest
// only; `stop` and `rpc.discover` never do. Any thread, concurrently.
const char* ZEBRAD_rpc(const char* method, const char* params_json);

#ifdef __cplusplus
}
#endif

#endif  // LOGOS_ZEBRAD_C_H
