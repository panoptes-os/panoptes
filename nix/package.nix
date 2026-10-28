{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "panoptes";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  # The cgroup tests need root and a cgroup v2 mount; the bench reruns
  # cover them.
  doCheck = false;

  meta = {
    description = "Run hundreds of graphical AI agents on one Linux box";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "panoptes-resd";
  };
}
