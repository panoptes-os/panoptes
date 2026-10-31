# panoptes-desk end to end in a VM, as systemd runs it: a delegated cgroup,
# the Cage fork at 1024x768, and MCP on the desk's socket. wev (Wayland)
# and xev (X11) log the input they receive, so desk.sh reads clicks, chords,
# Unicode text, scrolling, and drags back from the apps themselves.
#
# A VM because the build sandbox has no cgroupfs.
{
  testers,
  writeShellApplication,
  panoptes,
  cage,
  coreutils,
  file,
  jq,
  socat,
  wev,
  xev,
}:

let
  check = writeShellApplication {
    name = "desk-check";
    runtimeInputs = [
      coreutils
      file
      jq
      socat
    ];
    # jq programs are single-quoted on purpose.
    excludeShellChecks = [ "SC2016" ];
    text = builtins.readFile ./desk.sh;
  };
in
testers.runNixOSTest {
  name = "desk";
  nodes.machine = {
    virtualisation.memorySize = 1536;
    environment.systemPackages = [
      panoptes
      cage
      check
      wev
      xev
    ];
  };
  # The timeouts allow for a builder without KVM, which emulates the VM.
  testScript = ''
    from datetime import timedelta

    machine.wait_for_unit("multi-user.target")
    machine.succeed(
        "systemd-run --unit=desk -p Delegate=yes"
        " --setenv=PATH=/run/current-system/sw/bin"
        " panoptes-desk --run-dir /run/desk --thumb-ms 0"
    )
    machine.wait_for_file("/run/desk/desk.sock", timeout=timedelta(minutes=5))
    machine.succeed("desk-check /run/desk >&2", timeout=timedelta(minutes=30))
  '';
}
