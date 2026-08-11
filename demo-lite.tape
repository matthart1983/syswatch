# SysWatch Lite — the one-screen view.
#
# Story: twelve tabs is an instrument for a fleet operator. When the question
# is just "why is this machine hot, slow, or loud?", `--lite` answers it on one
# 80x24 screen with six keys — and `L` escalates to the full TUI without
# restarting anything, because both views share the same collector.
#
# Prereqs learned the hard way:
#   - Terminal must be at least 80x24 or Lite renders its too-small notice
#     instead of the layout. Width/Height/FontSize below are deliberately the
#     same as NetWatch's demo-lite.tape: the two Lite views share a grid, and
#     the GIFs should be directly comparable.
#   - Load must start BEFORE syswatch launches, so the CPU chart has something
#     to draw and the process table has a clear top row.
#   - `--tick 250` is not cosmetic. The charts hold one sample per column and
#     fill from the right in real time, so at the 1 Hz default the 78-column
#     CPU chart needs 78 seconds — most of the GIF would be empty space, which
#     reads as a broken render rather than an honest one. At 4 Hz it fills in
#     ~20s and nothing is faked: the axis label and the sparkline header both
#     derive from the tick and say so on screen.
#   - HOME is redirected to a scratch tree (see scripts/demo-lite-env.sh) so
#     the recording doesn't inherit the operator's theme.
#
# Record with:  vhs demo-lite.tape
# Needs: ./target/release/syswatch, vhs, scripts/demo-lite-env.sh.

Output demo-lite.gif

Set Shell "bash"
Set FontSize 16
Set Width 860
Set Height 500
Set Theme "Catppuccin Mocha"
Set TypingSpeed 30ms
Set Framerate 18
Set Padding 10

# Hidden setup. The PATH shim points `syswatch` at this repo's release build
# for the recording only, so the GIF shows the command users actually type
# without depending on whatever version is in ~/.cargo/bin.
Hide
Type "export PATH=$PWD/target/release:$PATH"
Enter
Type "export HOME=$(./scripts/demo-lite-env.sh)"
Enter
Type "yes > /dev/null & yes > /dev/null & yes > /dev/null &"
Enter
Type "clear"
Enter
Show

# Launch. No sudo, no config.
Type "syswatch --lite --tick 250"
Enter

# Let the charts fill from the right and the busy processes sort to the top.
# CPU gets three rows to memory's two — when a machine feels wrong, CPU is the
# answer more often than RAM.
Sleep 23s

# Walk down to a live process. Rows are ordered by CPU, so the top of the list
# is where the answer usually is.
Down@400ms 2
Sleep 2s

# Expand in place: pid, user, command, threads, memory, start time. The
# selection does not move — the list scrolls under the cursor instead.
Enter
Sleep 5s

# Collapse. Esc unwinds one layer at a time.
Escape
Sleep 1500ms

# Filter incrementally, on process name or user.
Type "/"
Sleep 800ms
Type "yes"
Sleep 3s
Enter
Sleep 2500ms

# Clear the filter — the second Esc, since the first closed detail.
Escape
Sleep 2500ms

# Deliberately NOT shown: `L` escalating to the full twelve-tab TUI. It works,
# and it keeps the collector warm, but the full view wants ~130 columns and
# renders clipped in this 80-column frame — an honest picture that reads as a
# broken one. That story belongs in prose.

Type "q"
Sleep 1s

# Tear down the background load (hidden).
Hide
Type "pkill -x yes 2>/dev/null; clear"
Enter
Sleep 500ms
