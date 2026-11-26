# lightwalletd for the regtest chain. `goos = "windows"` cross-compiles lightwalletd.exe: pure Go, no cgo.
{ pkgs, goos ? null }:

let
  version = "0.6.3";

  # lightwalletd 0.6 needs Go 1.26.10+, newer than the pinned nixpkgs' 1.25.0. Go 1.27 built from
  # source on nixpkgs' recipe, bootstrapped by that 1.25 (its own bootstrap, 1.22, is too old).
  bootstrap = pkgs.buildPackages.go_1_25;
  go = pkgs.buildPackages.go_1_25.overrideAttrs (old: {
    version = "1.27.4";
    src = pkgs.fetchurl {
      url = "https://go.dev/dl/go1.27.4.src.tar.gz";
      sha256 = "73cfb8571aec0c01177687db87163d46aa20e9779672772891aa17654d9a6099";
    };
    # go_no_vendor_checks doesn't apply past 1.25; nixpkgs uses it nowhere in tree.
    patches = builtins.filter (p: !(pkgs.lib.hasInfix "go_no_vendor_checks" (toString p))) old.patches;
    GOROOT_BOOTSTRAP = "${bootstrap}/share/go";
    disallowedReferences = (old.disallowedReferences or [ ]) ++ [ bootstrap ];
  });

  # As upstream's Makefile stamps a release; GetLightdInfo reports it.
  stamp = "-X github.com/zcash/lightwalletd/common.Version=v${version}";
in
(pkgs.buildGo125Module.override { inherit go; }) ({
  pname = "lightwalletd";
  inherit version;
  src = pkgs.fetchFromGitHub {
    owner = "zcash";
    repo = "lightwalletd";
    rev = "v${version}";
    hash = "sha256-oEfilypspzPLo3Cr5lC2tXxt34Mmmr9NyHNar2292Zs=";
  };
  vendorHash = "sha256-LyWbitsjw6YXxGnxd53SCkD6AL5p8YY6hLQJxgVIIRw=";
  subPackages = [ "." ];
  ldflags = [ stamp ];
  doCheck = false;
  meta.mainProgram = "lightwalletd";
} // pkgs.lib.optionalAttrs (goos == "windows") {
  # buildGoModule pins GOOS to the build platform's, so the .exe is built here directly.
  # `-s -w`: with DWARF in it, the mingw objdump the Windows CI gates with finds no import table.
  buildPhase = ''
    runHook preBuild
    GOOS=windows GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags "-s -w ${stamp}" -o "$GOPATH/bin/lightwalletd.exe" .
    runHook postBuild
  '';
  postInstall = ''
    test -f $out/bin/lightwalletd.exe
  '';
  # The build platform's strip and patchelf would mangle a PE.
  dontFixup = true;
  meta.mainProgram = "lightwalletd.exe";
})
