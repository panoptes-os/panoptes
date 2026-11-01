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

  # CI runs `cargo test` (.github/workflows/checks.yml). The cgroup tests
  # that need root are #[ignore]d; the `cgroup` check runs them in a VM, and
  # crates/panoptes-cgroup/tests/run-root.sh on a host.
  doCheck = false;

  meta = {
    description = "A harness and orchestrator for graphical AI agents on Linux";
    license = lib.licenses.agpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "panoptes-resd";
  };
}
