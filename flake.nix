{
    description = "web_ctrl - browser gamepad + live camera view for dimos robots over LCM and zenoh";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    };

    outputs = { self, nixpkgs }:
        let
            systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
            forEachSystem = function: nixpkgs.lib.genAttrs systems (system: function nixpkgs.legacyPackages.${system});
        in
        {
            packages = forEachSystem (pkgs: rec {
                web_ctrl = pkgs.rustPlatform.buildRustPackage {
                    pname = "web_ctrl";
                    version = "0.1.0";
                    src = ./.;
                    cargoLock.lockFile = ./Cargo.lock;
                    nativeBuildInputs = [ pkgs.pkg-config ];
                    buildInputs = [ pkgs.openssl ];
                };
                default = web_ctrl;
            });

            devShells = forEachSystem (pkgs: {
                default = pkgs.mkShell {
                    packages = [
                        pkgs.cargo
                        pkgs.rustc
                        pkgs.rustfmt
                        pkgs.clippy
                        pkgs.rust-analyzer
                        pkgs.pkg-config
                        pkgs.openssl
                    ];
                    RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
                };
            });
        };
}
