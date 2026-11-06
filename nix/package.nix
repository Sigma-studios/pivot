{ lib, rustPlatform, mesa, libgbm ? null }:

rustPlatform.buildRustPackage {
  pname = "pivot";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [ ../Cargo.toml ../Cargo.lock ../src ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  # libgbm was split out of mesa in late 2024; fall back to mesa on older nixpkgs.
  buildInputs = [ (if libgbm != null then libgbm else mesa) ];

  meta = {
    description = "Live-preview Alt+Tab window switcher for the COSMIC desktop";
    homepage = "https://github.com/Sigma-studios/pivot";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "pivot";
  };
}
