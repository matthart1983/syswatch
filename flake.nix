{
  description = "syswatch — single-host system diagnostics TUI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    # Explicit list rather than `eachDefaultSystem`: that helper still
    # claims x86_64-darwin, which nixpkgs dropped in 26.11, so the flake
    # advertised a platform it could not evaluate. Intel Macs are served
    # by the release tarballs, which are built from cargo directly.
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ] (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        syswatch = pkgs.callPackage ./package.nix { };
      in
      {
        packages = {
          syswatch = syswatch;
          default = syswatch;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ syswatch ];
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            clippy
            rustfmt
          ];
        };
      }
    );
}
