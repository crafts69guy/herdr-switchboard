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

# Choose Provider; dividers group repository hosts without selectable headers.
Type "s"
Sleep 600ms
Screenshot docs/media/projects-sort.png
Down
Down
Down
Enter
Sleep 1.2s
Screenshot docs/media/projects-provider.png
Down
Sleep 200ms
Up
Sleep 200ms

# Return to Recent for the rest of the tour.
Type "s"
Up
Up
Up
Enter
Sleep 400ms

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
