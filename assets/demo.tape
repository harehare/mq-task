Output "demo.gif"

Require echo

Set Shell "zsh"
Set FontSize 24
Set Width 1200
Set Height 800
Set TypingSpeed 50ms
Set FontFamily "Hack Nerd Font"
Set Padding 16
Set WindowBar Colorful
Set Framerate 20

Hide
Type "clear"
Enter
Show

Type `mq-task`
Enter
Sleep 1.5s

Type `mq-task JavaScript`
Enter
Sleep 1.5s

Type `mq-task Go`
Enter
Sleep 1.5s

Type `mq-task Python`
Enter
Sleep 1.5s

Type `mq-task mq`
Enter
Sleep 1.5s

Type `mq-task deploy`
Enter
Sleep 2.5s

Type `mq-task deploy -- env=prod`
Enter
Sleep 2.5s

# TUI: live dependency graph
Type `mq-task tui`
Enter
Sleep 1s

Type `/ci`
Enter
Sleep 0.5s

Enter
Sleep 8s

Type `q`
Sleep 0.5s
