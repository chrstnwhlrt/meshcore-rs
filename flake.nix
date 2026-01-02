{
  description = "meshcore-rs - Rust client library for MeshCore";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        inherit (pkgs) lib;

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        src = craneLib.cleanCargoSource ./.;

        darwinDeps = lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.apple_sdk.frameworks.IOKit
          pkgs.darwin.apple_sdk.frameworks.Security
        ];

        commonArgs = {
          inherit src;
          inherit (craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; }) pname version;
          strictDeps = true;
          buildInputs = [ pkgs.libudev-zero ] ++ darwinDeps;
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        pkg = craneLib.buildPackage (commonArgs // { inherit cargoArtifacts; });
      in
      {
        checks = {
          inherit pkg;
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          fmt = craneLib.cargoFmt { inherit src; };
          test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });
        };

        packages.default = pkg;

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = [ pkgs.cargo-watch pkgs.pkg-config pkgs.libudev-zero ] ++ darwinDeps;
          PKG_CONFIG_PATH = "${pkgs.libudev-zero}/lib/pkgconfig";
        };
      }
    );
}
