#!/usr/bin/env bash
# Prepare an isolated HOME for the SysWatch Lite demo recording, and print it.
#
# Why: syswatch reads its config from `dirs::config_dir()`, which is derived
# from $HOME. Recording against the operator's real config makes the GIF depend
# on whatever theme and graph style they happen to have set. Pointing HOME at a
# scratch tree makes the recording reproducible and leaves the real config
# untouched. (NetWatch learned this the hard way — a release GIF shipped in the
# wrong theme because the recording inherited the operator's config.)
#
# The pinned values are syswatch's own defaults rather than the palette-
# deferring "terminal" theme NetWatch Lite records under: this GIF sits beside
# demo.gif in the same README, and the two should look like one product.
#
# `graph_fade` only reaches Lite's sparklines — under `bars` the CPU and memory
# charts stack eighths in a flat color by design (see `lite::draw_chart`), so
# fade is a no-op there rather than something the recording suppresses.
#
# Usage (from a tape):  export HOME=$(./scripts/demo-lite-env.sh)
set -euo pipefail

DEMO_HOME="$(mktemp -d -t syswatch-demo-home)"
CFG_DIR="$DEMO_HOME/Library/Application Support/syswatch"

# Linux puts it under ~/.config; create both so the tape is portable.
mkdir -p "$CFG_DIR" "$DEMO_HOME/.config/syswatch"

cat > "$CFG_DIR/config.toml" <<'TOML'
theme = "dark"
graph_style = "bars"
graph_fade = true
default_tab = "overview"
tick_ms = 1000
TOML

cp "$CFG_DIR/config.toml" "$DEMO_HOME/.config/syswatch/config.toml"

echo "$DEMO_HOME"
