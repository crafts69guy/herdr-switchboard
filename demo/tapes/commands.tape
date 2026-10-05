# Commands: search history and presets, then fill the exact command and run it.
#
# The query must match exactly one row and nothing may move the cursor first:
# whatever Enter fills is then run for real on the recording machine.
Output docs/media/commands.gif
Source demo/tapes/common.tape

Sleep 1s
Ctrl+B
Sleep 200ms
Type "r"
Sleep 2.4s
Type "/"
Sleep 300ms
Type@150ms "graph"
Sleep 1.4s
Enter
Sleep 1.4s
Enter
Sleep 2.8s
