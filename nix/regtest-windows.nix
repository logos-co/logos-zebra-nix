# The regtest chain's two processes for Windows, as the wallet app's Windows doctest runs them
# under Git Bash: `bin/zebrad -c <zebrad.toml> start` on the smoke binary beside zebrad_c.dll,
# and lightwalletd.exe.
{ pkgs, smoke }:

{
  # The smoke binary's serve mode has no signal handling on Windows: stop it with taskkill.
  zebrad = pkgs.runCommand "regtest-zebrad-windows" { } ''
    mkdir -p $out/bin
    cp ${smoke}/bin/* $out/bin/
    cat > $out/bin/zebrad <<'SH'
    #!/bin/bash
    # `zebrad -c <zebrad.toml> start`, the one form the test scripts use.
    if [ "$#" -ne 3 ] || [ "$1" != -c ] || [ "$3" != start ]; then
      echo "usage: zebrad -c <zebrad.toml> start" >&2
      exit 2
    fi
    here="$(cd "$(dirname "$0")" && pwd)"
    exec "$here/zebrad-c-smoke.exe" "$here/zebrad_c.dll" serve "$2"
    SH
    chmod +x $out/bin/zebrad
  '';

  lightwalletd = import ./lightwalletd.nix { inherit pkgs; goos = "windows"; };
}
