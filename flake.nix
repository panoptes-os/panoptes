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
            pkgs.cargo-mutants
            pkgs.jq
            pkgs.ripgrep
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
          # The fork and its wlroots, built with ASan and UBSan, for the
          # -asan checks. wlroots too: Cage hands its objects to wlroots, so
          # Cage's memory errors often surface there, and an uninstrumented
          # wlroots reads freed memory without a report.
          sanitize = old: {
            mesonFlags = (old.mesonFlags or [ ]) ++ [
              "-Db_sanitize=address,undefined"
              "-Db_lundef=false"
            ];
            dontStrip = true;
          };
          cage-asan =
            (cage.override {
              cage = pkgs.cage.override { wlroots_0_20 = pkgs.wlroots_0_20.overrideAttrs sanitize; };
            }).overrideAttrs
              (old: sanitize old // { pname = "cage-asan"; });
        in
        {
          inherit panoptes cage cage-asan;
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
        # library's and binary's `loom_models`. Each crate that uses
        # panoptes-sync must run at least one. See
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
          checkPhase = ''
            set -o pipefail
            cargoCheckHook 2>&1 | tee loom.log
            bash ${./nix/rules/loom-count.sh} loom.log
          '';
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
        # The verification rules that a script checks: what the policy
        # files name, and that the rules' two copies agree. See nix/rules.
        rules =
          let
            pkgs = nixpkgs.legacyPackages.${system};
            script =
              name:
              pkgs.writeShellApplication {
                inherit name;
                runtimeInputs = [
                  pkgs.cargo
                  pkgs.coreutils
                  pkgs.diffutils
                  pkgs.gawk
                  pkgs.gnugrep
                  pkgs.jq
                  pkgs.ripgrep
                ];
                text = builtins.readFile ./nix/rules/${name}.sh;
              };
          in
          pkgs.runCommand "panoptes-rules"
            {
              src = nixpkgs.lib.fileset.toSource {
                root = ./.;
                fileset = nixpkgs.lib.fileset.unions [
                  ./Cargo.toml
                  ./Cargo.lock
                  ./crates
                  ./docs/design/verification.md
                  ./docs/agents/verification.md
                ];
              };
            }
            ''
              cp -r $src source
              chmod -R u+w source
              cd source
              export CARGO_HOME=$TMPDIR/cargo
              ${script "policy"}/bin/policy
              ${script "rule-copies"}/bin/rule-copies
              touch $out
            '';
        cage-size = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-size.nix {
          inherit (self.packages.${system}) cage;
        };
        cage-windows = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-windows.nix {
          inherit (self.packages.${system}) cage;
        };
        # Each -asan check runs the scenario before it under cage-asan.
        cage-windows-asan = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-windows.nix {
          name = "cage-windows-asan";
          cage = self.packages.${system}.cage-asan;
        };
        cage-menu = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-menu.nix {
          inherit (self.packages.${system}) cage;
        };
        cage-menu-asan = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-menu.nix {
          name = "cage-menu-asan";
          cage = self.packages.${system}.cage-asan;
        };
        cage-remap = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-remap.nix {
          inherit (self.packages.${system}) cage;
        };
        cage-remap-asan = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-remap.nix {
          name = "cage-remap-asan";
          cage = self.packages.${system}.cage-asan;
        };
        window-protocol = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/window-protocol.nix {
          inherit cage-src;
        };
        desk = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/desk.nix {
          inherit (self.packages.${system}) panoptes cage;
        };
        resd = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/resd.nix {
          inherit (self.packages.${system}) panoptes;
        };
        cgroup = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cgroup.nix {
          inherit (self.packages.${system}) panoptes;
        };
        desk-resd = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/desk-resd.nix {
          inherit (self.packages.${system}) panoptes cage;
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
