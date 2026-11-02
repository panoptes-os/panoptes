#!/bin/sh
# Opens the mock in its own Ghostty window. Nix fetches Ghostty and Node.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
fetch() { nix --extra-experimental-features 'nix-command flakes' build --no-link --print-out-paths "$1"; }
node=$(fetch 'nixpkgs#nodejs^out')/bin/node

case $(uname -s) in
Darwin)
	ghostty=$(fetch 'nixpkgs#ghostty-bin^out')
	exec open -na "$ghostty/Applications/Ghostty.app" --args \
		--config-default-files=false --config-file="$here/ghostty.conf" \
		--font-family=Menlo --font-size=15 \
		--command="$node '$here/mock.js'"
	;;
*)
	ghostty=$(fetch 'nixpkgs#ghostty^out')
	exec "$ghostty/bin/ghostty" \
		--config-default-files=false --config-file="$here/ghostty.conf" \
		--font-family="DejaVu Sans Mono" --font-size=11.25 \
		--gtk-titlebar=false --gtk-single-instance=false --window-decoration=none \
		--command="$node '$here/mock.js'"
	;;
esac
