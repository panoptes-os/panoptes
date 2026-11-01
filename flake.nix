{
  description = "Panoptes: a harness and orchestrator for graphical AI agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # Cage 0.3.1 with -r and panoptes_window_v1.
  inputs.cage-src = {
    url = "github:panoptes-os/cage/panoptes";
    flake = false;
  };

  outputs =
    {
      self,
      nixpkgs,
      cage-src,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.jq
            pkgs.sqlite
            pkgs.lua5_4
            pkgs.shellcheck
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
          ];
        };
      });

      # Linux only: the engine drives cgroup v2 and zswap.
      packages = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          panoptes = pkgs.callPackage ./nix/package.nix { };
          cage = pkgs.callPackage ./nix/cage.nix { src = cage-src; };
        in
        {
          inherit panoptes cage;
          default = panoptes;
          # nix build .#image && docker load < result
          image = pkgs.callPackage ./nix/image.nix { inherit panoptes cage; };
        }
      );

      checks = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system: {
        inherit (self.packages.${system}) panoptes cage;
        # cargo test, except the root-only tests, which are #[ignore].
        unit = self.packages.${system}.panoptes.overrideAttrs {
          pname = "panoptes-unit";
          doCheck = true;
        };
        # The loom models, with panoptes-sync swapped to loom: every
        # library's and binary's `loom_models`. See
        # docs/design/verification.md.
        loom = self.packages.${system}.panoptes.overrideAttrs (old: {
          pname = "panoptes-loom";
          doCheck = true;
          cargoTestFlags = [
            "--lib"
            "--bins"
          ];
          checkFlags = [ "loom_models" ];
          env = (old.env or { }) // {
            RUSTFLAGS = "--cfg panoptes_loom";
          };
        });
        # TLC on panoptes-sync's TLA+ specs: its channel under loom is std's
        # mpsc, and loom tries every wait a real build can make. See
        # crates/panoptes-sync/tla.
        tla =
          let
            pkgs = nixpkgs.legacyPackages.${system};
          in
          pkgs.runCommand "panoptes-tla" { nativeBuildInputs = [ pkgs.tlaplus ]; } ''
            export HOME=$TMPDIR
            cp -r ${./crates/panoptes-sync} sync
            bash sync/tla/check.sh
            touch $out
          '';
        cage-size = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-size.nix {
          inherit (self.packages.${system}) cage;
        };
        cage-windows = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-windows.nix {
          inherit (self.packages.${system}) cage;
        };
        # The same scenario with the fork built under ASan and UBSan.
        cage-windows-asan = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-windows.nix {
          name = "cage-windows-asan";
          cage = self.packages.${system}.cage.overrideAttrs (old: {
            pname = "cage-asan";
            mesonFlags = (old.mesonFlags or [ ]) ++ [
              "-Db_sanitize=address,undefined"
              "-Db_lundef=false"
            ];
            dontStrip = true;
          });
        };
        window-protocol = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/window-protocol.nix {
          inherit cage-src;
        };
        desk = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/desk.nix {
          inherit (self.packages.${system}) panoptes cage;
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
