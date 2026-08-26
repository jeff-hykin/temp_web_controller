{
    description = "web_ctrl - browser gamepad + live camera view for dimos robots over LCM and zenoh";

    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
        rust-overlay.url = "github:oxalica/rust-overlay";
        rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    };

    outputs = { self, nixpkgs, rust-overlay }:
        let
            systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
            forEachSystem = function: nixpkgs.lib.genAttrs systems (system: function system);
        in
        {
            packages = forEachSystem (system:
                let
                    pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };

                    # One pinned toolchain for every build here, carrying both musl
                    # targets so the cross builds do not need a second rustc.
                    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
                        targets = [
                            "x86_64-unknown-linux-musl"
                            "aarch64-unknown-linux-musl"
                        ];
                    };
                    rustPlatform = pkgs.makeRustPlatform {
                        cargo = rustToolchain;
                        rustc = rustToolchain;
                    };

                    # Imported only for their musl cross toolchains, not to build anything.
                    crossPkgsFor = config: import nixpkgs { inherit system; crossSystem.config = config; };

                    commonArgs = {
                        pname = "web_ctrl";
                        version = "0.1.0";
                        src = ./.;
                        cargoLock.lockFile = ./Cargo.lock;
                    };

                    native = rustPlatform.buildRustPackage commonArgs;

                    buildCross = rustTarget: crossPkgs:
                        let
                            targetSnake = builtins.replaceStrings [ "-" ] [ "_" ] rustTarget;
                            targetUpper = pkgs.lib.toUpper targetSnake;
                            binDirectory = "${crossPkgs.stdenv.cc}/bin";
                            prefix = crossPkgs.stdenv.cc.targetPrefix;
                        in
                        rustPlatform.buildRustPackage (commonArgs // {
                            pname = "web_ctrl-${rustTarget}";

                            # The test binary is built for the foreign target and cannot run here.
                            doCheck = false;

                            buildPhase = ''
                                runHook preBuild
                                cargo build --release --target ${rustTarget}
                                runHook postBuild
                            '';

                            installPhase = ''
                                runHook preInstall
                                mkdir -p $out/bin
                                install -m755 target/${rustTarget}/release/web_ctrl $out/bin/web_ctrl
                                runHook postInstall
                            '';

                            "CARGO_TARGET_${targetUpper}_LINKER" = "${binDirectory}/${prefix}cc";

                            # zstd-sys, lz4-sys and ring compile C from build scripts. Without
                            # these cc-rs reaches for the host clang with --target=<triple>,
                            # which has no matching sysroot and dies on `#include <string.h>`.
                            "CC_${targetSnake}" = "${binDirectory}/${prefix}cc";
                            "CXX_${targetSnake}" = "${binDirectory}/${prefix}c++";
                            "AR_${targetSnake}" = "${binDirectory}/${prefix}ar";
                        });
                in
                {
                    web_ctrl = native;
                    default = native;
                    linux-x86 = buildCross "x86_64-unknown-linux-musl" (crossPkgsFor "x86_64-unknown-linux-musl");
                    linux-arm64 = buildCross "aarch64-unknown-linux-musl" (crossPkgsFor "aarch64-unknown-linux-musl");
                });

            devShells = forEachSystem (system:
                let
                    pkgs = import nixpkgs { inherit system; overlays = [ (import rust-overlay) ]; };
                    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
                        targets = [
                            "x86_64-unknown-linux-musl"
                            "aarch64-unknown-linux-musl"
                        ];
                        extensions = [ "rust-src" "rust-analyzer" ];
                    };
                    linkerFor = config:
                        let crossPkgs = import nixpkgs { inherit system; crossSystem.config = config; };
                        in "${crossPkgs.stdenv.cc}/bin/${crossPkgs.stdenv.cc.targetPrefix}cc";
                in
                {
                    default = pkgs.mkShell {
                        packages = [ rustToolchain pkgs.pkg-config ];

                        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = linkerFor "x86_64-unknown-linux-musl";
                        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = linkerFor "aarch64-unknown-linux-musl";

                        shellHook = ''
                            echo "web_ctrl dev shell (Rust $(rustc --version | cut -d' ' -f2))"
                            echo ""
                            echo "  ./run/build               -- every target into dist/"
                            echo "  nix build .#linux-arm64   -- Linux aarch64 musl (the orin)"
                            echo "  nix build .#linux-x86     -- Linux x86_64 musl"
                            echo ""
                        '';
                    };
                });
        };
}
