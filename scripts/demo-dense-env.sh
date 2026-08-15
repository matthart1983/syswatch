#!/usr/bin/env bash
# Prepare an isolated HOME for the SysWatch Dense demo recording, and print it.
#
# Why: syswatch reads its config from `dirs::config_dir()`, which is derived
# from $HOME. Recording against the operator's real config makes the GIF depend
# on whatever theme and tick they happen to have set. Pointing HOME at a scratch
# tree makes the recording reproducible and leaves the real config untouched.
# (Both NetWatch and SysWatch have shipped a GIF in the wrong theme by skipping
# this, and a stray recording has overwritten a real config by not skipping it
# hard enough — the isolation protects the operator as much as the GIF.)
#
# `dark` rather than the palette-deferring `terminal` theme: this GIF sits in
# the same README as demo.gif and demo-lite.gif, and the three should look like
# one product. Dense is built on colour that carries meaning — magnitude in the
# graphs, severity on the meters — and the 16-colour theme steps that ramp
# rather than blending it, which is correct but not the picture to lead with.
#
# `dots` + `graph_fade` is the btop-style look the other two record under.
#
# Usage (from a tape):  export HOME=$(./scripts/demo-dense-env.sh)
set -euo pipefail

DEMO_HOME="$(mktemp -d -t syswatch-demo-home)"
CFG_DIR="$DEMO_HOME/Library/Application Support/syswatch"

# Linux puts it under ~/.config; create both so the tape is portable.
mkdir -p "$CFG_DIR" "$DEMO_HOME/.config/syswatch"

cat > "$CFG_DIR/config.toml" <<'TOML'
theme = "dark"
graph_style = "dots"
graph_fade = true
view = "dense"
default_tab = "overview"
tick_ms = 1000
TOML

cp "$CFG_DIR/config.toml" "$DEMO_HOME/.config/syswatch/config.toml"

echo "$DEMO_HOME"
