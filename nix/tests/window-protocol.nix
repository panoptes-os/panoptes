# The desk generates its panoptes_window bindings from its own copy of
# the protocol file; it must match the Cage fork's, byte for byte.
{ runCommand, cage-src }:

runCommand "window-protocol" { } ''
  cmp ${cage-src}/protocols/panoptes-window.xml \
    ${../../crates/panoptes-desk/protocols/panoptes-window.xml}
  touch $out
''
