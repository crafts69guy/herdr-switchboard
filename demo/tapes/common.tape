# Shared by every take: the frame, then a hidden launch into the sandbox's own
# Herdr server and a staged session. `demo/render.sh` runs tapes from the
# repository root, which is what the relative path below assumes.
Set Shell bash
Set FontFamily "BlexMono Nerd Font Mono"
Set FontSize 15
Set Width 1600
Set Height 900
Set Padding 14
# Huế Mưa (hue-theme), the same palette demo/sandbox.sh pins for Herdr.
Set Theme {"name":"Hue Mua","background":"#001F3E","foreground":"#E5F4FF","cursor":"#00CF6A","selection":"#123F6E","black":"#0A2E52","red":"#FF5A63","green":"#00CF6A","yellow":"#FF8D00","blue":"#10B6F8","magenta":"#8A7BFF","cyan":"#10B6F8","white":"#9DB4D4","brightBlack":"#4E7AA6","brightRed":"#FF5A63","brightGreen":"#00CF6A","brightYellow":"#FF8D00","brightBlue":"#10B6F8","brightMagenta":"#8A7BFF","brightCyan":"#10B6F8","brightWhite":"#E5F4FF"}
Set WindowBar Colorful
Set BorderRadius 10
Set Framerate 24
Set TypingSpeed 90ms
Set CursorBlink false

Hide
Type "bash demo/enter.sh herdr"
Enter
Sleep 3s
Type "stage"
Enter
Sleep 7s
Show
