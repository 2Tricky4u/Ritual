# Render with: vhs demo.tape  →  assets/demo.gif   (https://github.com/charmbracelet/vhs)
# Run from a project that has a .ritual/ with some findings for the best capture.
Output assets/demo.gif

Require ritual

Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 16
Set Width 1280
Set Height 720
Set Padding 20
Set Theme { "background": "#212337", "foreground": "#ebfafa" }

# CLI smoke: styled subcommand output
Type "ritual status" Enter Sleep 2s

# Dashboard
Type "ritual" Enter Sleep 2.5s

# Findings tab → detail overlay
Type "2" Sleep 1.5s
Enter Sleep 2.5s
Escape Sleep 1s

# Settings editor
Type "S" Sleep 2.5s
Escape Sleep 1s

# Help, then quit
Type "?" Sleep 2s
Escape Sleep 500ms
Type "q" Sleep 1s
