# Hero: the Projects switcher over live agents, workspaces, repos, and worktrees.
Output docs/media/projects.gif
Source demo/tapes/common.tape

Sleep 1s
Ctrl+B
Sleep 200ms
Type "p"
Sleep 2.4s

# Groups narrow the list; the Context column counts each one.
Tab
Sleep 800ms
Tab
Sleep 800ms
Tab
Sleep 900ms

# Every repository row carries its own Inspector: branch, last commit, tree, README.
Type@700ms "jjjj"
Sleep 900ms

# Fuzzy search across every group, star the hit, then open it in a workspace.
Shift+Tab
Shift+Tab
Shift+Tab
Sleep 400ms
Type "/"
Sleep 300ms
Type@150ms "raytr"
Sleep 1.3s
Ctrl+S
Sleep 1.2s
Enter
Sleep 2.6s
