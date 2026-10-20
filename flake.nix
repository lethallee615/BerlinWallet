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

            shellHook = ''
              export CC="${pkgs.gcc}/bin/gcc"
              export CXX="${pkgs.gcc}/bin/g++"
              export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(clang -print-resource-dir)/include -isystem /usr/include"
              echo "Berlin Wallet dev shell"
              echo "Rust: $(rustc --version)"
              echo "Run: cargo fmt --check && cargo build --workspace"
            '';
          };
        });
    };
}
