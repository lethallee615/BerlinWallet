{
  description = "Berlin Wallet development shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          llvm = pkgs.llvmPackages;
          toolchain = pkgs.rust-bin.stable."1.91.1".default.override {
            extensions = [ "rustfmt" "clippy" ];
          };
        in {
          default = pkgs.mkShell {
            packages = [
              toolchain
              # aws-lc-sys' CMake configuration shells out to Go for code
              # generation when it is built from source rather than the
              # pre-generated tree. Without this the probe fails with
              # "Could not find Go" before the Rust build starts.
              pkgs.go
              llvm.clang
              llvm.libclang
              pkgs.cmake
              pkgs.gcc
              pkgs.pkg-config
              pkgs.openssl
              pkgs.sqlite
              pkgs.zlib
              pkgs.perl
            ];

            LIBCLANG_PATH = "${llvm.libclang.lib}/lib";

            # aws-lc-sys' CMake feature probes have been observed compiling at
            # -O0. An ambient `_FORTIFY_SOURCE` (from the distro glibc or a
            # carried-over environment) turns that into a -Werror failure, so
            # give every probe an optimization level. This is a C flag, not a
            # Cargo profile change.
            CFLAGS = "-O2";
            CXXFLAGS = "-O2";

            shellHook = ''
              export CC="${pkgs.gcc}/bin/gcc"
              export CXX="${pkgs.gcc}/bin/g++"
              export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(clang -print-resource-dir)/include -isystem /usr/include"
              echo "Berlin Wallet dev shell"
              echo "Rust: $(rustc --version)"
              echo "Go:   $(go version)"
              echo "Run: cargo fmt --check && cargo build --workspace"
              echo "If aws-lc-sys still fails after switching shells, its cached"
              echo "CMake configuration is stale; remove it with:"
              echo "  rm -rf target/debug/build/aws-lc-sys-*"
            '';
          };
        });
    };
}
