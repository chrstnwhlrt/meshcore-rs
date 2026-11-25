{
  description = "meshcore-rs - Rust client library for MeshCore companion radios";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, crane, rust-overlay }:
    let
      inherit (nixpkgs) lib;

      # nixpkgs 26.11 dropped x86_64-darwin.
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];

      # The rust-version from Cargo.toml as a toolchain version ("1.89" -> "1.89.0").
      msrv =
        let declared = (lib.importTOML ./Cargo.toml).package.rust-version;
        in if lib.length (lib.splitString "." declared) == 2 then "${declared}.0" else declared;

      perSystem = system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };

          craneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          });

          # The Cargo sources, the fixtures the parity test reads and the
          # README, whose examples are doctests.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              ./tests/fixtures
              ./README.md
            ];
          };

          # The ble feature links libdbus to talk to BlueZ (Linux only; on other
          # systems the feature has no effect).
          bleNativeInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.pkg-config ];
          bleInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.dbus ];

          commonArgs = {
            inherit src;
            inherit (craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; }) pname version;
            strictDeps = true;
            # With ble and serde, so the checks cover all of the code.
            cargoExtraArgs = "--locked --all-features";
            nativeBuildInputs = bleNativeInputs;
            buildInputs = bleInputs;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # The oldest Rust the crate declares it builds with.
          msrvCraneLib = (crane.mkLib pkgs).overrideToolchain (p: p.rust-bin.stable.${msrv}.minimal);
          msrvArgs = commonArgs // { pname = "${commonArgs.pname}-msrv"; };

          defaultArgs = commonArgs // {
            pname = "${commonArgs.pname}-default-features";
            cargoExtraArgs = "--locked";
          };

          package = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            # the test check runs them
            doCheck = false;
          });
        in
        {
          inherit package;

          checks = {
            inherit package;
            clippy = craneLib.cargoClippy (commonArgs // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });
            # Without the optional features, which checks their cfg gates.
            clippy-default-features = craneLib.cargoClippy (defaultArgs // {
              cargoArtifacts = craneLib.buildDepsOnly defaultArgs;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            });
            test = craneLib.cargoTest (commonArgs // { inherit cargoArtifacts; });
            doc = craneLib.cargoDoc (commonArgs // {
              inherit cargoArtifacts;
              cargoDocExtraArgs = "--no-deps";
              RUSTDOCFLAGS = "--deny warnings";
            });
            fmt = craneLib.cargoFmt { inherit src; };
            msrv = msrvCraneLib.cargoTest (msrvArgs // {
              cargoArtifacts = msrvCraneLib.buildDepsOnly msrvArgs;
            });
          };

          # The toolchain with rust-analyzer, and libdbus for the ble feature.
          devShell = craneLib.devShell {
            packages = bleNativeInputs;
            buildInputs = bleInputs;
          };
        };

      outputs = lib.genAttrs systems perSystem;
    in
    {
      packages = lib.mapAttrs (_: o: { default = o.package; }) outputs;
      checks = lib.mapAttrs (_: o: o.checks) outputs;
      devShells = lib.mapAttrs (_: o: { default = o.devShell; }) outputs;
    };
}
