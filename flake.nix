{
  description = "Logos zebra-nix: Zebra, the Zcash node, patched to run in-process and built as libzebrad_c.";

  inputs = {
    # logos-nix roots the family's nixpkgs pin; logos-module-builder follows the same one.
    logos-nix.url = "github:logos-co/logos-nix";
    nixpkgs.follows = "logos-nix/nixpkgs";
    # Zebra 7 needs rustc 1.91, newer than the pinned nixpkgs ships.
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay, logos-nix, ... }:
    let
      lib = nixpkgs.lib;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];

      # The library's build leaves the smoke test's sources out, so editing it rebuilds
      # only the smoke binary.
      crateFiles = [
        ./zebrad-c/Cargo.toml
        ./zebrad-c/Cargo.lock
        ./zebrad-c/src
        ./zebrad-c/include
        ./zebrad-c/zebrad_c.exp
        ./zebrad-c/zebrad_c.map
        ./zebrad-c/smoke/Cargo.toml
      ];
      source = files: lib.fileset.toSource { root = ./zebrad-c; fileset = lib.fileset.unions files; };

      perSystem = system:
        let
          pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
          # Zebra's own rust-toolchain.toml channel.
          toolchain = pkgs.rust-bin.stable."1.91.0".minimal;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
          zebra = import ./nix/zebra-src.nix { inherit pkgs; };

          libzebrad_c = import ./nix/zebrad-c.nix {
            inherit pkgs rustPlatform;
            zebraSrc = zebra.patched;
            crateSrc = source crateFiles;
          };
          smoke = import ./nix/smoke.nix {
            inherit pkgs rustPlatform libzebrad_c;
            zebraSrc = zebra.patched;
            crateSrc = source (crateFiles ++ [ ./zebrad-c/smoke/src ]);
          };
          regtest = import ./nix/regtest-fixture.nix { inherit pkgs libzebrad_c; smoke = smoke.smoke; };
        in
        {
          packages = {
            default = libzebrad_c;
            inherit libzebrad_c;
            zebra-src = zebra.patched;
            zebrad-c-smoke = smoke.smoke;
            # A regtest chain for wallet tests: `zebrad -c <toml> start` on libzebrad_c, and lightwalletd.
            regtest-zebrad = regtest.zebrad;
            inherit (regtest) lightwalletd;
          };
          checks = {
            patches = pkgs.runCommand "zebra-patch-guard" { nativeBuildInputs = [ pkgs.python3 ]; } ''
              set -o pipefail
              bash ${./ci/check-patches.sh} ${zebra.upstream} ${./patches/zebra} ${./zebrad-c/Cargo.lock} | tee "$out"
            '';
            smoke = smoke.check;
            regtest-fixture = regtest.check;
          };
        };

      all = lib.genAttrs systems perSystem;

      # x86_64-windows: cross builds from x86_64-linux, keyed as the family's pseudo-system.
      windows =
        let
          pkgs = import nixpkgs { system = "x86_64-linux"; overlays = [ (import rust-overlay) ]; };
          zebra = import ./nix/zebra-src.nix { inherit pkgs; };
        in
        import ./nix/windows.nix {
          inherit pkgs;
          wpkgs = logos-nix.lib.mkWindowsPkgs { buildSystem = "x86_64-linux"; };
          toolchain = pkgs.rust-bin.stable."1.91.0".minimal.override { targets = [ "x86_64-pc-windows-gnu" ]; };
          zebraSrc = zebra.patched;
          crateSrc = source (crateFiles ++ [ ./zebrad-c/zebrad_c.def ]);
          smokeSrc = source (crateFiles ++ [ ./zebrad-c/zebrad_c.def ./zebrad-c/smoke/src ]);
        };
    in
    {
      packages = lib.mapAttrs (_: s: s.packages) all // {
        x86_64-windows = {
          default = windows.libzebrad_c;
          inherit (windows) libzebrad_c;
          # zebrad-c-smoke.exe beside zebrad_c.dll and GCC's runtime DLLs, ready to run.
          zebrad-c-smoke = windows.smoke;
        };
      };
      checks = lib.mapAttrs (_: s: s.checks) all;
    };
}
