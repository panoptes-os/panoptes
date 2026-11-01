# Run with:
#   podman run --privileged --cgroupns=private --network=host --shm-size=4g \
#     --pids-limit=-1 panoptes:0.1.0
# Each desk's programs start many threads, and Podman's default pid limit
# counts every one of them; --pids-limit=-1 lifts it.
{
  dockerTools,
  writeShellApplication,
  makeFontsConf,
  panoptes,
  bashInteractive,
  cacert,
  cage,
  chromium,
  coreutils,
  dejavu_fonts,
  gawk,
  tini,
  util-linux,
}:

let
  entry = writeShellApplication {
    name = "panoptes-entry";
    runtimeInputs = [
      coreutils
      gawk
      panoptes
    ];
    text = builtins.readFile ./entry.sh;
  };

  fonts = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };

  # Desks run as this user through setpriv; Chromium's sandbox refuses root.
  nss = dockerTools.fakeNss.override {
    extraPasswdLines = [ "panoptes:x:1000:1000:Panoptes agent:/var/empty:/bin/sh" ];
    extraGroupLines = [ "panoptes:x:1000:" ];
  };
in
dockerTools.buildLayeredImage {
  name = "panoptes";
  tag = panoptes.version;

  contents = [
    panoptes
    entry
    nss
    bashInteractive
    cacert
    cage
    chromium
    coreutils
    tini
    util-linux
    dockerTools.binSh
  ];

  extraCommands = ''
    mkdir -p tmp run var/tmp
    chmod 1777 tmp var/tmp
  '';

  config = {
    Entrypoint = [
      "tini"
      "--"
      "panoptes-entry"
    ];
    Cmd = [
      "sleep"
      "infinity"
    ];
    Env = [
      "FONTCONFIG_FILE=${fonts}"
      "SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt"
    ];
    # links the GHCR package to the repository
    Labels."org.opencontainers.image.source" = "https://github.com/panoptes-os/panoptes";
  };
}
