{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "viska";
  version = "0.3.0";
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ../layouts ../tests ../LICENSE ];
  };
  cargoLock.lockFile = ../Cargo.lock;
  doCheck = true;
  postInstall = ''
    install -Dm644 LICENSE "$out/share/licenses/viska/LICENSE"
    install -Dm644 layouts/COPYING.xkeyboard-config "$out/share/licenses/viska/COPYING.xkeyboard-config"
  '';
  meta = {
    description = "One-shot sensitive text delivery via Linux uinput";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "viskactl";
  };
}
