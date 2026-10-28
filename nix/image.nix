# `panoptes up` runs this image with the flags the engine needs, and says
# why each is there (crates/panoptes/src/fleet.rs).
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
  poppler-utils,
  tini,
  util-linux,
  wlr-randr,
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
    poppler-utils
    tini
    util-linux
    wlr-randr
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
      "PANOPTES_PDFTOTEXT=pdftotext"
      "SSL_CERT_FILE=${cacert}/etc/ssl/certs/ca-bundle.crt"
    ];
    # links the GHCR package to the repository
    Labels."org.opencontainers.image.source" = "https://github.com/panoptes-os/panoptes";
  };
}
