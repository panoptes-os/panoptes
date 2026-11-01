# panoptes-desk with --resd against panoptes-resd in a VM, with swap and
# zswap on: desk-resd.sh checks that the kiosk is frozen between calls,
# parked after a long wait and woken by the next call, and thawed by the
# desk itself once resd is gone.
#
# A VM because the build sandbox has no cgroupfs.
{
  testers,
  writeShellApplication,
  panoptes,
  cage,
  coreutils,
  gawk,
  jq,
  socat,
  util-linux,
  wev,
}:

let
  check = writeShellApplication {
    name = "desk-resd-check";
    runtimeInputs = [
      panoptes
      coreutils
      gawk
      jq
      socat
      util-linux
    ];
    # jq programs are single-quoted on purpose.
    excludeShellChecks = [ "SC2016" ];
    text = builtins.readFile ./desk-resd.sh;
  };
in
testers.runNixOSTest {
  name = "desk-resd";
  nodes.machine = {
    virtualisation.memorySize = 1536;
    boot.kernelParams = [ "zswap.enabled=1" ];
    # Test VMs ignore swapDevices; desk-resd.sh turns this disk into swap.
    virtualisation.emptyDiskImages = [ 512 ];
    environment.systemPackages = [
      panoptes
      cage
      check
      wev
    ];
    # MemAvailable here is below the 4 GiB --mem-low-mib default, which
    # parks every waiting kiosk at the next tick; only the wait parks here.
    systemd.services.resd.serviceConfig.ExecStart =
      "${panoptes}/bin/panoptes-resd --park-after-ms 2000 --mem-low-mib 0 --tick-ms 200";
  };
  # The timeouts allow for a builder without KVM, which emulates the VM.
  testScript = ''
    from datetime import timedelta

    machine.wait_for_unit("multi-user.target")
    machine.succeed("desk-resd-check >&2", timeout=timedelta(minutes=30))
  '';
}
