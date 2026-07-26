{
  lib,
  rustPlatform,
  pkg-config,
  libpcap,
}:

rustPlatform.buildRustPackage {
  pname = "syswatch";
  # Read from Cargo.toml rather than restated here — a hand-maintained
  # copy silently drifted from 0.7.0 through five releases, because
  # nothing in CI builds this file.
  version = (lib.importTOML ./Cargo.toml).package.version;

  # `lib.cleanSource` drops VCS and editor cruft but keeps `target/`,
  # which is multiple GB on any working checkout — every `nix build`
  # was copying it into the store and invalidating on each cargo run.
  # Deny-list rather than an explicit fileset: it can only over-include,
  # so a new source file can't silently go missing from the build.
  src = lib.cleanSourceWith {
    src = lib.cleanSource ./.;
    filter =
      path: type:
      let
        base = baseNameOf (toString path);
      in
      !(type == "directory" && (base == "target" || base == ".github"));
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config ];

  # libpcap pulled in transitively by `netwatch-sdk` (used by the Net tab
  # for per-interface counters via the SDK's packet helpers). macpow on
  # macOS uses IOKit + SMC from the base SDK — no extra Nix inputs.
  # The `gpu-nvidia` Cargo feature pulls `nvml-wrapper` which needs
  # `nvml` at link/runtime; it's opt-in (default = []) so the default
  # `nix build` doesn't include it.
  buildInputs = [ libpcap ];

  meta = {
    description = "Single-host, read-only system diagnostics TUI. Twelve tabs covering CPU, memory, disks, processes, GPU, power, services, network, plus a Timeline scrubber and an Insights anomaly engine. Sibling to netwatch.";
    homepage = "https://github.com/matthart1983/syswatch";
    license = lib.licenses.mit;
    mainProgram = "syswatch";
  };
}
