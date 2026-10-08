# Zebra at its release tag, fetched by hash, and the same tree with patches/zebra applied.
# ci/check-patches.sh (checks.<system>.patches) holds the series to its allowlist.
{ pkgs }:

let
  version = "7.0.0-rc.0";

  upstream = pkgs.fetchFromGitHub {
    owner = "ZcashFoundation";
    repo = "zebra";
    rev = "v${version}";
    hash = "sha256-1Uu6SpZeSfpXwUYWr5vN192HSk7vZUTjBtRLl2bT2es=";
  };

  patchDir = ../patches/zebra;
in
{
  inherit version upstream;

  patched = pkgs.applyPatches {
    name = "zebra-src-${version}-patched";
    src = upstream;
    # readDir lists names sorted, so the series applies in order.
    patches = map (name: patchDir + "/${name}") (builtins.attrNames (builtins.readDir patchDir));
  };
}
