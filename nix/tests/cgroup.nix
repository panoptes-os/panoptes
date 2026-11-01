# panoptes-cgroup's kernel tests (crates/panoptes-cgroup/tests/root.rs) in
# a VM with swap and zswap on, as root in a delegated scope, the way
# tests/run-root.sh runs them by hand.
#
# A VM because the build sandbox has no cgroupfs.
{
  testers,
  panoptes,
  jq,
}:

let
  # The root test binary alone; cargo names it root-<hash>.
  tests = panoptes.overrideAttrs (old: {
    pname = "panoptes-cgroup-root-tests";
    nativeBuildInputs = old.nativeBuildInputs ++ [ jq ];
    buildPhase = ''
      runHook preBuild
      cargo test --release --offline -p panoptes-cgroup --test root --no-run \
        --message-format=json > build.json
      runHook postBuild
    '';
    installPhase = ''
      runHook preInstall
      bin=$(jq -r 'select(.executable != null) | .executable' build.json)
      install -D "$bin" "$out/bin/panoptes-cgroup-root-tests"
      runHook postInstall
    '';
    doCheck = false;
  });
in
testers.runNixOSTest {
  name = "cgroup";
  nodes.machine = {
    virtualisation.memorySize = 1024;
    boot.kernelParams = [ "zswap.enabled=1" ];
    # Test VMs ignore swapDevices; the test script turns this disk into swap.
    virtualisation.emptyDiskImages = [ 512 ];
  };
  testScript = ''
    from datetime import timedelta

    machine.wait_for_unit("multi-user.target")
    machine.succeed("mkswap /dev/vdb && swapon /dev/vdb")
    machine.succeed(
        "systemd-run --scope --quiet --unit=cgroup-root-tests"
        " -p Delegate=yes -p MemoryAccounting=yes"
        " ${tests}/bin/panoptes-cgroup-root-tests"
        " --ignored --test-threads=1 --nocapture >&2",
        timeout=timedelta(minutes=10),
    )
  '';
}
