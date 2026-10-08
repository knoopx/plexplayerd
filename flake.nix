{
  description = "plexplayerd";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
    in {
      packages = nixpkgs.lib.genAttrs systems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          plexplayerd = pkgs.rustPlatform.buildRustPackage {
            pname = "plexplayerd";
            version = "0.1.0";
            src = self;
            cargoLock = {
              lockFile = ./Cargo.lock;
            };
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ pkgs.alsa-lib.dev pkgs.alsa-lib ];
          };
        in {
          inherit plexplayerd;
          default = plexplayerd;
        }
      );

      devShells = nixpkgs.lib.genAttrs systems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          # alsa-lib is split outputs: the `dev` output carries include/ and
          # lib/pkgconfig/alsa.pc, while plain alsa-lib is the runtime-only
          # `out` output (libasound.so). alsa-sys needs both via pkg-config.
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.pkg-config
              pkgs.alsa-lib.dev
              pkgs.alsa-lib
            ];
          };
        }
      );
    };
}
