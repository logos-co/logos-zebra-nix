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

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      lib = nixpkgs.lib;
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];

      perSystem = system:
        let
          pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
          # Zebra's own rust-toolchain.toml channel.
          toolchain = pkgs.rust-bin.stable."1.91.0".minimal;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
          zebra = import ./nix/zebra-src.nix { inherit pkgs; };

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
        in
        {
          packages = {
            default = libzebrad_c;
            inherit libzebrad_c;
            zebra-src = zebra.patched;
            zebrad-c-smoke = smoke.smoke;
          };
          checks = {
            patches = pkgs.runCommand "zebra-patch-guard" { nativeBuildInputs = [ pkgs.python3 ]; } ''
              set -o pipefail
              bash ${./ci/check-patches.sh} ${zebra.upstream} ${./patches/zebra} ${./zebrad-c/Cargo.lock} | tee "$out"
            '';
            smoke = smoke.check;
          };
        };

      all = lib.genAttrs systems perSystem;
    in
    {
      packages = lib.mapAttrs (_: s: s.packages) all;
      checks = lib.mapAttrs (_: s: s.checks) all;
    };
}
