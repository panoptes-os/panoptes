# Sourced by the E14 scripts: PATH from the nix build of the E14 toolset.
E=/var/tmp/glarb-bench/e14
while read -r p; do
  [ -d "$p/bin" ] && PATH=$p/bin:$PATH
  [ -x "$p/libexec/at-spi-bus-launcher" ] && ATSPI_LIBEXEC=$p/libexec
done < "$E/paths.txt"
export PATH ATSPI_LIBEXEC FONTCONFIG_FILE=/var/tmp/glarb-bench/fonts.conf
