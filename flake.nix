{
  description = "konter: turns raw bank data into hledger books";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # What `konter --version` prints after the version number.
      rev = self.rev or self.dirtyRev or "dev";

      per =
        pkgs:
        let
          lib = pkgs.lib;
          craneLib = crane.mkLib pkgs;
          # Only what Cargo reads. The flake itself only ever sees files git
          # tracks, so books/ never gets this far anyway.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./rustfmt.toml
              ./crates
            ];
          };
          common = {
            inherit src;
            pname = "konter";
            version = (lib.importTOML ./Cargo.toml).workspace.package.version;
            strictDeps = true;
          };
          # The dependencies, built once and reused for as long as Cargo.lock
          # doesn't change.
          cargoArtifacts = craneLib.buildDepsOnly common;
          konter = craneLib.buildPackage (
            common
            // {
              inherit cargoArtifacts;
              # The tests are a check of their own (below).
              doCheck = false;
              KONTER_REV = rev;
              meta.mainProgram = "konter";
            }
          );
          # The Matrix relay for the server (docs/server.md). Its dependencies
          # are many, so they're built apart from konter's, and only when
          # it's asked for.
          relayArgs = common // {
            pname = "konter-relay";
            cargoExtraArgs = "--locked -p konter-relay";
          };
          relay = craneLib.buildPackage (
            relayArgs
            // {
              cargoArtifacts = craneLib.buildDepsOnly relayArgs;
              doCheck = false;
              meta.mainProgram = "konter-relay";
            }
          );
        in
        {
          packages = {
            default = konter;
            inherit konter relay;
            # The hledger `konter verify` is checked against: nixpkgs' as it is,
            # so `nix flake update nixpkgs` is how it moves.
            hledger = pkgs.hledger;
          };
          checks = {
            inherit konter;
            # Every crate's tests, not just the default member's.
            tests = craneLib.cargoTest (
              common
              // {
                inherit cargoArtifacts;
                cargoTestExtraArgs = "--workspace";
              }
            );
          };
          devShell = craneLib.devShell {
            packages = [
              pkgs.hledger
              pkgs.just
              pkgs.cargo-insta
              pkgs.clippy
              pkgs.rustfmt
              pkgs.rust-analyzer
            ];
          };
        };
    in
    {
      packages = forEachSystem (pkgs: (per pkgs).packages);
      checks = forEachSystem (pkgs: (per pkgs).checks);
      devShells = forEachSystem (pkgs: {
        default = (per pkgs).devShell;
      });
    };
}
