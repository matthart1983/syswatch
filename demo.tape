Output demo.gif

Require syswatch

Set Shell "bash"
Set FontSize 14
Set Width 1200
Set Height 700
Set Theme "Catppuccin Mocha"
Set TypingSpeed 30ms
Set Framerate 18
Set Padding 10

# Boot syswatch at a fast tick so the sparklines and counters move on camera.
Hide
Type "./target/release/syswatch --tick 500"
Enter
Sleep 4s
Show

# [1] Overview — KPI strip, per-core grid, top procs, live insights.
Sleep 5s

# [2] CPU — aggregate sparkline + per-core utilization bars.
Type "2"
Sleep 4s

# [3] Memory — RAM bar, swap, top RSS.
Type "3"
Sleep 4s

# [6] Procs — sortable table; cycle the sort key so the table reshuffles.
Type "6"
Sleep 3s
Type "s"
Sleep 2s
Type "s"
Sleep 2s
Down@200ms 6
Sleep 2s

# [8] Power — battery, source, throttle, draw.
Type "8"
Sleep 4s

# [9] Services — sortable; switch to status sort to surface failed units.
Type "9"
Sleep 3s
Type "s"
Sleep 3s

# [-] Timeline — stacked activity strips + scrubber. Walk back, then return live.
Type "-"
Sleep 3s
Left@200ms 12
Sleep 2s
# Return to live (End key isn't supported by vhs; use right-arrow until at 0).
Right@200ms 12
Sleep 2s

# [+] Insights — plain-English anomaly cards.
Type "+"
Sleep 5s

# Back to Overview to land the closing frame.
Type "1"
Sleep 4s

Type "q"
