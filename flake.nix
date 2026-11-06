{
  description = "pivot: live-preview Alt+Tab window switcher for the COSMIC desktop";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        pivot = pkgs.callPackage ./nix/package.nix { };
        default = pivot;
      });

      overlays.default = final: _prev: { pivot = final.callPackage ./nix/package.nix { }; };

      nixosModules.default = import ./nix/module.nix self;

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.pivot ];
          packages = with pkgs; [ clippy rustfmt rust-analyzer ];
        };
      });
    };
}
