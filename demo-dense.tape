# SysWatch Dense — every subsystem on one screen.
#
# Story: Lite answers "why is this machine hot, slow, or loud?" on 80x24.
# Dense answers the opposite question — "what is this machine doing, all of
# it, at once?" — by spending every row of a 130x44 terminal on data: six
# boxes, zero chrome rows, and identity, sort state, page range and every
# keybind carried inside the box borders so a heading costs nothing.
#
# Prereqs learned the hard way:
#   - The frame must be 130x44, the size the grid was drawn at. At FontSize 14
#     with Padding 10 that is exactly Width 1222 x Height 743 — measured with
#     `tput cols/lines` inside vhs, not guessed, because pixel dimensions off
#     by a cell change the band heights the GIF is meant to show. (The compact
#     three-box fallback is far below this, at 100x37.)
#   - Load must start BEFORE syswatch launches, so the graphs have something
#     to draw and the process table has a clear top row. `yes` drives cpu,
#     cores and procs; the looping rate-limited downloads drive the net
#     mirror and the disk box's write trace at the same time.
#   - `--tick 250` is not cosmetic. Graphs hold one sample per column and
#     fill from the right in real time, so at the 1 Hz default the ~120
#     column cpu graph needs two minutes — most of the GIF would be empty
#     space, which reads as a broken render rather than an honest one. At
#     4 Hz it fills in ~30s and nothing is faked: every axis measures the
#     history it is actually showing and says so on screen.
#   - HOME is redirected to a scratch tree (see scripts/demo-dense-env.sh) so
#     the recording doesn't inherit the operator's theme, and doesn't
#     overwrite it either — `V` and `t` both write the view and theme back to
#     the running config.
#
# Record with:  vhs demo-dense.tape
# Needs: ./target/release/syswatch, vhs, curl, scripts/demo-dense-env.sh.

Output demo-dense.gif

Set Shell "bash"
Set FontSize 14
Set Width 1222
Set Height 743
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
Type "export HOME=$(./scripts/demo-dense-env.sh)"
Enter
Type "yes > /dev/null & yes > /dev/null & yes > /dev/null &"
Enter
# Real downloads, rate-limited and looped, to feed the net mirror. Four of
# them, and that number is measured rather than chosen for drama: VHS records
# by streaming the terminal over ttyd on localhost, and syswatch redraws this
# screen ~30 times a second, so `lo0` carries ~5M/s each way of the recording
# watching itself. The net box titles whichever interface is busiest by
# rx+tx, so anything under ~10M/s loses that contest and the GIF comes out
# labelled `lo0`. Four 5M/s streams hold the real interface on top through
# the redraw bursts, and the resulting ~4:1 rx:tx ratio is what makes the
# mirror read as a download rather than a hum.
#
# The first lands in the scratch HOME, which also gives the disk box a write
# trace to draw; the rest go to /dev/null so the recording doesn't write most
# of a gigabyte to the operator's SSD.
Type "while :; do curl -s --limit-rate 5M http://archive.ubuntu.com/ubuntu/dists/noble/Contents-amd64.gz -o $HOME/dl.bin; done & echo $! > $HOME/dl.pid"
Enter
Type "for i in 1 2 3; do while :; do curl -s --limit-rate 5M http://archive.ubuntu.com/ubuntu/dists/noble/Contents-amd64.gz -o /dev/null; done & echo $! >> $HOME/dl.pid; done"
Enter
Type "clear"
Enter
Show

# Launch. No sudo, no config.
Type "syswatch --dense --tick 250"
Enter

# Let all six boxes fill from the right at once: cpu's full-height graph and
# its vitals row, memory's composition and history, the mirrored net pair,
# the per-core grid, disk read/write, and the process table sorted by cpu.
Sleep 26s

# Walk down to a live process. Rows are ordered by cpu, and the detail block
# is hoisted into the top of the same box — it expands in place, above the
# table, so the row order never shifts under the cursor.
Down@400ms 3
Sleep 3s

# `6` zooms the process table to the whole frame — thirty-nine rows instead of
# the seven the grid affords. Every box border carries its own bracketed
# hotkey, so the affordance is on screen before you need it.
Type "6"
Sleep 5s

# Esc restores the grid.
Escape
Sleep 1500ms

# `3` zooms the one box that earns a mirror. Download grows up from the
# shared axis, upload grows down, so traffic symmetry becomes a shape: this
# machine is pulling files, and that reads as a cliff above the line with
# only the acknowledgement traffic under it.
Type "3"
Sleep 5s

Escape
Sleep 2s

# Deliberately NOT shown: `V` cycling back to the twelve-tab full view. It
# works, it keeps the collector warm, and at this size it renders correctly —
# but that view is already the README's lead GIF, and re-recording it here
# buys nothing except megabytes. The story this GIF has to tell is the one
# screen you can't see anywhere else.

Type "q"
Sleep 1s

# Tear down the background load (hidden). The download loop has to die before
# the curl it is currently running, or it just starts another one — and it
# would outlive this shell, still pulling 20M/s, if neither happened.
Hide
Type "kill $(cat $HOME/dl.pid) 2>/dev/null; pkill -f 'archive.ubuntu' 2>/dev/null"
Enter
Sleep 500ms
Type "pkill -x yes 2>/dev/null; rm -f $HOME/dl.bin $HOME/dl.pid; clear"
Enter
Sleep 500ms
