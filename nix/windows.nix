# libzebrad_c and its smoke test for Windows: cross builds from x86_64-linux with the family's
# MinGW toolchain (logos-nix: GCC with the mcf thread model, against the UCRT).
{ pkgs, wpkgs, toolchain, zebraSrc, crateSrc, smokeSrc }:

let
  inherit (pkgs) lib;
  target = "x86_64-pc-windows-gnu";
  u = "x86_64_pc_windows_gnu";
  cc = wpkgs.stdenv.cc;
  triple = wpkgs.stdenv.hostPlatform.config;
  tool = name: "${cc.bintools.bintools}/bin/${cc.targetPrefix}${name}";
  inherit (wpkgs.windows) pthreads mcfgthreads mingw_w64_headers;
  libclang = pkgs.llvmPackages.libclang;
  rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };

  # GCC's runtime, which zebrad_c.dll imports; Logos hosts ship these same files.
  runtimeDlls = [
    "${cc.cc.lib}/${triple}/lib/libstdc++-6.dll"
    "${cc.cc.lib}/${triple}/lib/libgcc_s_seh-1.dll"
    "${mcfgthreads}/bin/libmcfgthread-2.dll"
  ];

  # rustc's final link, with our export list for the one rustc writes, and libgcc shared as
  # g++ links it: static libgcc_eh gives the DLL its own emulated TLS, and RocksDB's
  # std::call_once then crashes (rust-rocksdb#665). -lmcfgthread goes last: rustc links
  # with -nodefaultlibs, and C++ under the mcf thread model needs it.
  linker = pkgs.writeShellScript "zebrad-c-link-windows" ''
    args=() swapped=0
    for a in "$@"; do
      case "$a" in
        -Wl,*.def) args+=("-Wl,${crateSrc}/zebrad_c.def"); swapped=1 ;;
        *.def) args+=("${crateSrc}/zebrad_c.def"); swapped=1 ;;
        -lgcc_eh) args+=("-lgcc_s") ;;
        *) args+=("$a") ;;
      esac
    done
    [ "$swapped" = 1 ] || { echo "zebrad-c-link-windows: rustc passed no export list to swap" >&2; exit 1; }
    exec ${cc}/bin/${cc.targetPrefix}cc "''${args[@]}" -lmcfgthread
  '';

  # logos-module-builder's Rust cross environment, plus mcfgthread's headers for C++.
  common = {
    version = (lib.importTOML ../zebrad-c/Cargo.toml).package.version;
    cargoDeps = rustPlatform.importCargoLock { lockFile = ../zebrad-c/Cargo.lock; };
    # zebrad-c/Cargo.toml takes Zebra's crates from ../zebra.
    postUnpack = ''
      ln -s ${zebraSrc} zebra
    '';
    # MinGW's binutils: rustc runs x86_64-w64-mingw32-dlltool for raw-dylib imports (windows-sys).
    nativeBuildInputs = [ rustPlatform.cargoSetupHook toolchain cc.bintools.bintools ];
    env = {
      "CARGO_TARGET_${lib.toUpper u}_LINKER" = "${cc}/bin/${cc.targetPrefix}cc";
      # std links -l:libpthread.a, which mingw-w64 built against mcfgthread does not have;
      # the second path is for the linker's -lmcfgthread.
      "CARGO_TARGET_${lib.toUpper u}_RUSTFLAGS" = "-L native=${pthreads}/lib -L native=${mcfgthreads}/lib";
      "CC_${u}" = "${cc}/bin/${cc.targetPrefix}cc";
      "CXX_${u}" = "${cc}/bin/${cc.targetPrefix}c++";
      "AR_${u}" = tool "ar";
      "CFLAGS_${u}" = "-I${pthreads}/include";
      # <mutex> and <thread> include mcfgthread/gthr.h (RocksDB, zcash_script).
      "CXXFLAGS_${u}" = "-I${pthreads}/include -isystem ${mcfgthreads.dev}/include";
      LIBCLANG_PATH = "${libclang.lib}/lib";
    };
    # bindgen (librocksdb-sys) parses with the build platform's libclang, for the target.
    preBuild = ''
      export BINDGEN_EXTRA_CLANG_ARGS_${u}="--target=${triple} -isystem $(echo ${libclang.lib}/lib/clang/*/include) -isystem ${mingw_w64_headers}/include"
    '';
    # The build platform's strip cannot read PE files; installPhase strips with MinGW's.
    dontStrip = true;
  };

  libzebrad_c = pkgs.stdenv.mkDerivation (common // {
    pname = "zebrad_c";
    src = crateSrc;

    buildPhase = ''
      runHook preBuild
      CARGO_PROFILE_RELEASE_STRIP=false cargo rustc -p zebrad_c --lib --profile release --offline \
        -j "$NIX_BUILD_CORES" --target ${target} -- -C linker=${linker}
      runHook postBuild
    '';

    # The DLL and its import library side by side, as logos-module-builder links and ships them.
    installPhase = ''
      runHook preInstall
      built=target/${target}/release
      install -Dm0755 $built/zebrad_c.dll $out/lib/zebrad_c.dll
      ${tool "strip"} --strip-all $out/lib/zebrad_c.dll
      install -Dm0644 $built/libzebrad_c.dll.a $out/lib/libzebrad_c.dll.a
      install -Dm0644 include/zebrad_c.h $out/include/zebrad_c.h
      install -Dm0644 ${zebraSrc}/LICENSE-MIT $out/lib/LICENSE-MIT.zebra
      install -Dm0644 ${zebraSrc}/LICENSE-APACHE $out/lib/LICENSE-APACHE.zebra
      runHook postInstall
    '';

    doInstallCheck = true;
    installCheckPhase = ''
      runHook preInstallCheck
      pe=$(${tool "objdump"} -p $out/lib/zebrad_c.dll)
      # Exactly the functions the header declares, as ci/check-exports.sh asserts on Unix.
      want=$(grep -o -E 'ZEBRAD_[a-z_]+\(' include/zebrad_c.h | tr -d '(' | sort -u)
      got=$(echo "$pe" | awk '/^\[Ordinal\/Name Pointer\] Table/ { t = 1; next } t && /^\t\[/ { print $NF } t && /^$/ { t = 0 }' | sort -u)
      [ "$got" = "$want" ] || { echo "exports of zebrad_c.dll differ from zebrad_c.h:" >&2; diff <(echo "$want") <(echo "$got") >&2; exit 1; }
      echo "exports: $(echo "$got" | wc -l) ZEBRAD_* functions and nothing else"
      # The libgcc_s link above: emulated TLS must come from libgcc_s_seh-1.dll, as libstdc++'s does.
      echo "$pe" | awk '/DLL Name:/ { d = ($3 == "libgcc_s_seh-1.dll") } d && /__emutls_get_address/ { f = 1 } END { exit !f }' \
        || { echo "zebrad_c.dll does not take __emutls_get_address from libgcc_s_seh-1.dll" >&2; exit 1; }
      if echo "$pe" | grep -qi 'DLL Name: msvcrt.dll'; then
        echo "zebrad_c.dll imports msvcrt.dll beside the UCRT" >&2; exit 1
      fi
      echo "imports: $(echo "$pe" | awk '/DLL Name:/ { print $3 }' | sort -fu | tr '\n' ' ')"
      runHook postInstallCheck
    '';

    meta = {
      description = "Zebra, the Zcash node, run in-process behind a C ABI (Windows)";
      license = with lib.licenses; [ mit asl20 ];
    };
  });

  # A runnable tree: the smoke binary beside zebrad_c.dll and GCC's runtime.
  smoke = pkgs.stdenv.mkDerivation (common // {
    pname = "zebrad-c-smoke";
    src = smokeSrc;

    buildPhase = ''
      runHook preBuild
      cargo build -p zebrad-c-smoke --release --offline -j "$NIX_BUILD_CORES" --target ${target}
      runHook postBuild
    '';

    installPhase = ''
      runHook preInstall
      install -Dm0755 target/${target}/release/zebrad-c-smoke.exe $out/bin/zebrad-c-smoke.exe
      ${tool "strip"} --strip-all $out/bin/zebrad-c-smoke.exe
      install -m0755 ${libzebrad_c}/lib/zebrad_c.dll ${lib.escapeShellArgs runtimeDlls} $out/bin/
      runHook postInstall
    '';

    meta.mainProgram = "zebrad-c-smoke.exe";
  });
in
{
  inherit libzebrad_c smoke;
}
