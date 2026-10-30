# The desk generates its panoptes_window_v1 bindings from its own copy of
# the protocol file; it must match the Cage fork's, byte for byte.
{ runCommand, cage-src }:

runCommand "window-protocol" { } ''
  cmp ${cage-src}/protocols/panoptes-window-v1.xml \
    ${../../crates/panoptes-desk/protocols/panoptes-window-v1.xml}
  touch $out
''
