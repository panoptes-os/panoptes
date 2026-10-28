{
  description = "Panoptes: agents that sleep in turns";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
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
        in
        {
          inherit panoptes;
          default = panoptes;
          # nix build .#image && docker load < result
          image = pkgs.callPackage ./nix/image.nix { inherit panoptes; };
          # the host CLI alone, linked statically: a host needs only podman
          # or docker besides it
          cli = (pkgs.pkgsStatic.callPackage ./nix/package.nix { }).overrideAttrs {
            cargoBuildFlags = [
              "-p"
              "panoptes"
            ];
          };
        }
      );

      checks = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (system: {
        inherit (self.packages.${system}) panoptes;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
