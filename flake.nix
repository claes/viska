{
  description = "Viska: one-shot protected text delivery through Linux uinput";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      each = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in {
      nixosModules.default = import ./nix/module.nix { inherit self; };
      packages = each (system:
        let
          pkgs = pkgsFor system;
          viska = pkgs.callPackage ./nix/package.nix {};
          demo = pkgs.writeShellApplication {
            name = "viska-demo";
            runtimeInputs = [ viska pkgs.coreutils pkgs.jq pkgs.bashInteractive ];
            text = builtins.readFile ./scripts/demo.sh;
          };
          test = pkgs.writeShellApplication {
            name = "viska-test";
            runtimeInputs = [ pkgs.python3 ];
            text = ''
              export VISKA_BIN_DIR=${viska}/bin
              exec python3 ${./tests/integration.py}
            '';
          };
          vm = nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [ self.nixosModules.default ./nix/vm.nix ];
          };
          test-vm = pkgs.writeShellApplication {
            name = "viska-test-vm";
            runtimeInputs = [ pkgs.coreutils ];
            text = ''
              work=$(mktemp -d -t viska-vm.XXXXXXXX)
              trap 'rm -rf -- "$work"' EXIT
              export NIX_DISK_IMAGE="$work/disk.qcow2"
              export TMPDIR="$work"
              echo "Disposable Swedish-layout VM; close it with poweroff from the guest."
              echo "No host home or clipboard is shared. VM state is deleted on exit."
              ${vm.config.system.build.vm}/bin/run-viska-test-vm-vm
            '';
          };
        in { default = viska; inherit viska demo test test-vm; });
      apps = each (system: {
        demo = { type = "app"; program = "${self.packages.${system}.demo}/bin/viska-demo"; };
        test = { type = "app"; program = "${self.packages.${system}.test}/bin/viska-test"; };
        test-vm = { type = "app"; program = "${self.packages.${system}.test-vm}/bin/viska-test-vm"; };
        default = { type = "app"; program = "${self.packages.${system}.default}/bin/viskactl"; };
      });
      devShells = each (system:
        let pkgs = pkgsFor system; in { default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.python3 pkgs.jq ];
        }; });
      checks = each (system:
        let pkgs = pkgsFor system; in {
          unit-tests = self.packages.${system}.default;
          layouts = pkgs.runCommand "viska-layout-provenance" {
            nativeBuildInputs = [ pkgs.python3 ];
            archive = pkgs.fetchurl {
              url = "mirror://xorg/individual/data/xkeyboard-config/xkeyboard-config-2.41.tar.xz";
              hash = "sha256-8CzWuVcpXg1QI2o9sVglJWyS9n7x9zvxx3pLF57fco8=";
            };
          } ''
            tar xf "$archive"
            mkdir layouts
            python3 ${./scripts/generate-layouts.py} "$PWD/xkeyboard-config-2.41" ${pkgs.libxkbcommon}/lib/libxkbcommon.so
            diff -u ${./layouts/us.json} layouts/us.json
            diff -u ${./layouts/se.json} layouts/se.json
            diff -u ${./layouts/SOURCES.json} layouts/SOURCES.json
            touch "$out"
          '';
          module = pkgs.runCommand "viska-module-evaluation" {} ''
            test ${if (nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [ self.nixosModules.default {
                system.stateVersion = "25.11";
                services.viska = { enable = true; keyboardLayout = "se"; authorization.typeUsers = [ "trigger" ]; };
              } ];
            }).config.services.viska.keyboardLayout == "se" then "1" else "0"} = 1
            touch "$out"
          '';
        } // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
          integration = import ./nix/integration.nix { inherit pkgs self; };
        });
    };
}
