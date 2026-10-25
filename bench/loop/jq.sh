# Sourced: put a jq binary on PATH.
command -v jq > /dev/null || PATH=$(nix build --no-link --print-out-paths nixpkgs#jq \
  | while read -r p; do [ -x "$p/bin/jq" ] && echo "$p/bin"; done | head -1):$PATH
