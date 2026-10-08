# libzebrad_c: Zebra's node, run in-process, as a shared library with a C ABI.
{ pkgs, rustPlatform, zebraSrc, crateSrc }:

let
  inherit (pkgs) lib stdenv;
  isDarwin = stdenv.hostPlatform.isDarwin;
  ext = stdenv.hostPlatform.extensions.sharedLibrary;
  exports = "${crateSrc}/zebrad_c.${if isDarwin then "exp" else "map"}";

  # rustc hands the linker its own export list: every #[no_mangle] symbol in the crate
  # graph, secp256k1-sys's callbacks included. A second list cannot narrow it (ld64 takes
  # the union, GNU ld refuses two), so this swaps it for ours on the final link.
  exportsLinker = pkgs.writeShellScript "zebrad-c-link" ''
    args=() swapped=0 path_next=0
    for a in "$@"; do
      if [ "$path_next" = 1 ]; then args+=("-Wl,${exports}"); path_next=0; swapped=1; continue; fi
      case "$a" in
        -Wl,-exported_symbols_list) args+=("$a"); path_next=1 ;;
        -Wl,--version-script=*) args+=("-Wl,--version-script=${exports}"); swapped=1 ;;
        *) args+=("$a") ;;
      esac
    done
    [ "$swapped" = 1 ] || { echo "zebrad-c-link: rustc passed no export list to swap" >&2; exit 1; }
    exec ${stdenv.cc}/bin/cc "''${args[@]}"
  '';
in
rustPlatform.buildRustPackage {
  pname = "zebrad_c";
  version = (lib.importTOML ../zebrad-c/Cargo.toml).package.version;

  src = crateSrc;
  cargoLock.lockFile = ../zebrad-c/Cargo.lock;

  # zebrad-c/Cargo.toml takes Zebra's crates from ../zebra.
  postUnpack = ''
    ln -s ${zebraSrc} zebra
  '';

  # bindgen, for librocksdb-sys.
  nativeBuildInputs = [ rustPlatform.bindgenHook ];

  # `cargo rustc` reaches only libzebrad_c's own link, so build scripts keep the stock linker.
  buildPhase = ''
    runHook preBuild
    CARGO_PROFILE_RELEASE_STRIP=false cargo rustc -p zebrad_c --lib --profile release --offline \
      -j "$NIX_BUILD_CORES" --target ${stdenv.hostPlatform.rust.rustcTargetSpec} \
      -- -C linker=${exportsLinker} ${if isDarwin
        then "-C link-arg=-Wl,-install_name,$out/lib/libzebrad_c${ext}"
        else "-C link-arg=-Wl,-soname,libzebrad_c${ext}"}
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm0755 target/${stdenv.hostPlatform.rust.rustcTargetSpec}/release/libzebrad_c${ext} $out/lib/libzebrad_c${ext}
    install -Dm0644 include/zebrad_c.h $out/include/zebrad_c.h
    # Zebra is MIT OR Apache-2.0; lib/ is where module staging looks.
    install -Dm0644 ${zebraSrc}/LICENSE-MIT $out/lib/LICENSE-MIT.zebra
    install -Dm0644 ${zebraSrc}/LICENSE-APACHE $out/lib/LICENSE-APACHE.zebra
    runHook postInstall
  '';

  doCheck = false;
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    bash ${../ci/check-exports.sh} $out/lib/libzebrad_c${ext} $out/include/zebrad_c.h
    runHook postInstallCheck
  '';

  # strip would invalidate the linker's ad-hoc signature on macOS.
  dontStrip = isDarwin;

  meta = {
    description = "Zebra, the Zcash node, run in-process behind a C ABI";
    license = with lib.licenses; [ mit asl20 ];
  };
}
