{
  description = "Panoptes: agents that sleep in turns";

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
            pkgs.codex
            # npx for chrome-devtools-mcp
            pkgs.nodejs
            pkgs.himalaya
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
        cage-size = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-size.nix {
          inherit (self.packages.${system}) cage;
        };
        cage-windows = nixpkgs.legacyPackages.${system}.callPackage ./nix/tests/cage-windows.nix {
          inherit (self.packages.${system}) cage;
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
