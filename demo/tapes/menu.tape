# The central menu routes to every surface; here it opens Usage.
#
# Walk to the row rather than filter to it: the shared picker keeps the cursor's
# index while a query narrows the list, so a filter typed after any motion can
# land Enter on a different row — once, on the plugin updater.
Output docs/media/menu.gif
Source demo/tapes/common.tape

Sleep 1s
Ctrl+B
Sleep 200ms
Space
Sleep 2.2s
Type@450ms "jjjjjj"
Sleep 1.2s
Enter
Sleep 4.5s
Escape
Sleep 800ms
