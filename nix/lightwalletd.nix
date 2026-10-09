# lightwalletd for the regtest chain. v0.5.x speaks lightwallet-protocol v0.5.0, the oldest the
# wallet accepts. `goos = "windows"` cross-compiles lightwalletd.exe: pure Go, no cgo.
{ pkgs, goos ? null }:

let
  version = "0.5.4";
  # As upstream's Makefile stamps a release; GetLightdInfo reports it.
  stamp = "-X github.com/zcash/lightwalletd/common.Version=v${version}";
in
pkgs.buildGo125Module ({
  pname = "lightwalletd";
  inherit version;
  src = pkgs.fetchFromGitHub {
    owner = "zcash";
    repo = "lightwalletd";
    rev = "v${version}";
    hash = "sha256-vlfC/2yuHx9wiOczyUfBmuI5KdLyonACVlwtUowSuDA=";
  };
  vendorHash = "sha256-DT1R6C6AoXR0FpyVTzw9VcF0DaPbvqvkrVsYg+6bP2g=";
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
