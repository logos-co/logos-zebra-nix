# A regtest chain for wallet tests, run as processes beside the product, never in it:
# zebrad's command line on libzebrad_c (the smoke binary's serve mode), and lightwalletd.
{ pkgs, smoke, libzebrad_c }:

let
  lib = "${libzebrad_c}/lib/libzebrad_c${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";

  # `zebrad -c <zebrad.toml> start`, the one form the test scripts use.
  zebrad = pkgs.writeShellScriptBin "zebrad" ''
    if [ "$#" -ne 3 ] || [ "$1" != -c ] || [ "$3" != start ]; then
      echo "usage: zebrad -c <zebrad.toml> start" >&2
      exit 2
    fi
    exec ${smoke}/bin/zebrad-c-smoke ${lib} serve "$2"
  '';

  # v0.5.x speaks lightwallet-protocol v0.5.0, the oldest the wallet accepts.
  lightwalletd = pkgs.buildGo125Module {
    pname = "lightwalletd";
    version = "0.5.4";
    src = pkgs.fetchFromGitHub {
      owner = "zcash";
      repo = "lightwalletd";
      rev = "v0.5.4";
      hash = "sha256-vlfC/2yuHx9wiOczyUfBmuI5KdLyonACVlwtUowSuDA=";
    };
    vendorHash = "sha256-DT1R6C6AoXR0FpyVTzw9VcF0DaPbvqvkrVsYg+6bP2g=";
    subPackages = [ "." ];
    # As upstream's Makefile stamps a release; GetLightdInfo reports it.
    ldflags = [ "-X github.com/zcash/lightwalletd/common.Version=v0.5.4" ];
    doCheck = false;
    meta.mainProgram = "lightwalletd";
  };
in
{
  inherit zebrad lightwalletd;

  # Both together: zebrad mines three blocks, lightwalletd serves the third, and SIGTERM
  # stops zebrad cleanly.
  check = pkgs.runCommand "zebra-regtest-fixture-check" {
    __darwinAllowLocalNetworking = true;
    nativeBuildInputs = [ pkgs.curl pkgs.grpcurl ];
  } ''
    set -euo pipefail
    export HOME="$TMPDIR"
    cd "$TMPDIR"
    # Off the usual ports: a Darwin build shares the host's network.
    cat > zebrad.toml <<TOML
    [mining]
    miner_address = "tmLPctKo9j49rtCSKpwEBpLBeykiTGomGQs"
    [network]
    network = "Regtest"
    listen_addr = "127.0.0.1:38233"
    cache_dir = false
    [rpc]
    listen_addr = "127.0.0.1:38232"
    enable_cookie_auth = false
    [state]
    cache_dir = "$TMPDIR/state"
    TOML
    rpc() {
      curl -s --max-time 120 -H 'content-type: application/json' \
        --data-binary "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$1\",\"params\":''${2:-[]}}" http://127.0.0.1:38232/
    }
    fail() { echo "FAIL: $1"; tail -n 30 zebrad.out node.log lwd.out lwd.log 2>/dev/null || true; exit 1; }

    ${zebrad}/bin/zebrad -c zebrad.toml start > zebrad.out 2>&1 &
    z=$!
    for _ in $(seq 1 240); do
      rpc getblockcount | grep -q '"result"' && break
      kill -0 "$z" 2>/dev/null || fail "zebrad exited"
      sleep 0.5
    done
    rpc generate '[3]' | grep -q '"result"' || fail "generate"

    ${lightwalletd}/bin/lightwalletd --no-tls-very-insecure --grpc-bind-addr 127.0.0.1:39067 \
      --http-bind-addr 127.0.0.1:39068 --rpchost 127.0.0.1 --rpcport 38232 --rpcuser x --rpcpassword x \
      --data-dir lwd --log-file lwd.log > lwd.out 2>&1 &
    l=$!
    tip=""
    for _ in $(seq 1 240); do
      tip=$(grpcurl -plaintext -max-time 5 127.0.0.1:39067 cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLatestBlock 2>/dev/null || true)
      echo "$tip" | grep -q '"height": "3"' && break
      kill -0 "$l" 2>/dev/null || fail "lightwalletd exited"
      sleep 0.5
    done
    echo "$tip" | grep -q '"height": "3"' || fail "lightwalletd never served block 3: $tip"
    kill "$l"
    wait "$l" || true

    kill "$z"
    wait "$z" || fail "zebrad exited $? on SIGTERM"
    grep -q "stopped in" zebrad.out || fail "no clean stop"
    { echo "lightwalletd tip: $tip"; grep "stopped in" zebrad.out; } | tee "$out"
  '';
}
