# Sourced by the glarb bench scripts: toolset PATH and fonts.
B=/var/tmp/glarb-bench
for f in "$B"/logs/nixbuild.out "$B"/logs/nixbuild.extra; do
  [ -f "$f" ] || continue
  while read -r p; do
    case $p in /nix/store/*) [ -d "$p/bin" ] && PATH=$p/bin:$PATH ;; esac
  done < "$f"
done
export PATH FONTCONFIG_FILE=$B/fonts.conf
