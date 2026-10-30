# Our fork of Cage, panoptes-os/cage's panoptes branch, built with nixpkgs'
# own recipe for the release it forked from.
{ cage, src }:

cage.overrideAttrs {
  version = "0.3.1-panoptes";
  inherit src;
}
