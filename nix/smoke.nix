# The smoke test: a binary that dlopens libzebrad_c, and the check that runs it against
# the built library on a throwaway regtest node.
{ pkgs, rustPlatform, zebraSrc, crateSrc, libzebrad_c }:

let
  inherit (pkgs) stdenv;

  smoke = rustPlatform.buildRustPackage {
    pname = "zebrad-c-smoke";
    version = "0.1.0";
    src = crateSrc;
    cargoLock.lockFile = ../zebrad-c/Cargo.lock;
    # It includes zebra-rpc's generated lightwalletd types from ../zebra.
    postUnpack = ''
      ln -s ${zebraSrc} zebra
    '';
    cargoBuildFlags = [ "-p" "zebrad-c-smoke" ];
    doCheck = false;
    meta.mainProgram = "zebrad-c-smoke";
  };
in
{
  inherit smoke;

  check = pkgs.runCommand "zebrad-c-smoke-check" {
    # Loopback only: the node, lightwalletd and JSON-RPC listen on 127.0.0.1.
    __darwinAllowLocalNetworking = true;
  } ''
    set -o pipefail
    export HOME="$TMPDIR"
    ${smoke}/bin/zebrad-c-smoke ${libzebrad_c}/lib/libzebrad_c${stdenv.hostPlatform.extensions.sharedLibrary} | tee "$out"
  '';
}
