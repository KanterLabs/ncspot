---
name: Bug report
about: Create a report to help us improve
title: ''
labels: bug
assignees: ''

---

**Describe the bug**
A clear and concise description of what the bug is.

**To Reproduce**
Steps to reproduce the behavior:
1. Go to '...'
2. Click on '....'
3. Scroll down to '....'
4. See error

**Expected behavior**
A clear and concise description of what you expected to happen.

**Screenshots**
If applicable, add screenshots to help explain your problem.

**System (please complete the following information):**
 - OS: [e.g. Linux, macOS]
 - Terminal: [e.g. GNOME Terminal, Alacritty]
 - Version: [e.g. 0.4.0, `master` branch]
 - Installed from: [e.g. AUR, brew, cargo]

**Backtrace/Debug log**
Please attach a Resonance debug log and backtrace if Resonance has crashed. Run
`resonance info` first and include its `USER_CONFIGURATION_PATH`, `USER_CACHE_PATH`, and
`USER_RUNTIME_PATH` output so paths can be reproduced.

Instructions on how to capture debug logs can be found in the [developers
manual](https://github.com/KanterLabs/resonance/blob/main/doc/developers.md#debugging).

For backtraces, make sure you run a debug build of Resonance, e.g. by running the
command mentioned in the [compilation
instructions](https://github.com/KanterLabs/resonance/blob/main/doc/developers.md#compiling). The
latest backtrace is at `<USER_CACHE_PATH>/backtrace.log`, where `USER_CACHE_PATH` is the value
reported by `resonance info`; do not assume the old `~/.cache/ncspot` path.

**Additional context**
Add any other context about the problem here.
