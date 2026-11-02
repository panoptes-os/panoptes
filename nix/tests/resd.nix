# panoptes-resd end to end in a VM, with swap and zswap on: resd.sh drives
# a real kiosk cgroup through release, park, acquire, restart, eviction
# and removal, as root and as a desk.
#
# A VM because the build sandbox has no cgroupfs.
{
  testers,
  writeShellApplication,
  panoptes,
  coreutils,
  diffutils,
  gawk,
  jq,
  socat,
  util-linux,
}:

let
  check = writeShellApplication {
    name = "resd-check";
    runtimeInputs = [
      panoptes
      coreutils
      diffutils
      gawk
      jq
      socat
      util-linux
    ];
    text = builtins.readFile ./resd.sh;
  };
in
testers.runNixOSTest {
  name = "resd";
  nodes.machine = {
    virtualisation.memorySize = 1024;
    boot.kernelParams = [ "zswap.enabled=1" ];
    # Test VMs ignore swapDevices; resd.sh turns this disk into swap.
    virtualisation.emptyDiskImages = [ 512 ];
    environment.systemPackages = [ check ];
    # MemAvailable here is below the 4 GiB --mem-low-mib default, which
    # parks every waiting kiosk at the next tick; only the wait parks here.
    systemd.services.resd.serviceConfig.ExecStart =
      "${panoptes}/bin/panoptes-resd --park-after-ms 2000 --mem-low-mib 0 --tick-ms 200";
  };
  testScript = ''
    from datetime import timedelta

    machine.wait_for_unit("multi-user.target")
    machine.succeed("resd-check >&2", timeout=timedelta(minutes=10))
  '';
}
