# Hyperlinks and mouse pointer shapes

Wisp uses Kitty's default link mappings:

- Hover over a detected URL or an OSC 8 hyperlink to show a pointing hand and underline.
- An unmodified left click opens the link after the 500 ms multi-click interval.
- Double/triple clicks select words/lines and cancel a pending link click.
- Shift overrides application mouse grabs and enables local link handling.
- Ctrl+Shift+left release opens immediately, including while an application has grabbed the mouse.
- A local linear selection uses the text pointer; rectangular selection uses a crosshair.
- Ordinary application mouse grabs use the arrow pointer.

Links are passed as one argument to `xdg-open`, without shell evaluation. OSC 8
targets take priority over their displayed text. Automatic detection uses Kitty's
default URL schemes and follows soft wrapping, bounded to three rows on either
side of the hovered row. URL scans are cached until terminal output, viewport
changes, or geometry changes; motion within one cell does not rescan text.

OSC 22 supports CSS pointer names, common X11 aliases, push/pop/set, support
queries, and independent primary/alternate screen stacks. A reset clears both
stacks. Link hover and local selection take priority over application pointer
shapes, as specified by Kitty's protocol.

References:

- [Kitty default mouse mappings and URL options](https://github.com/kovidgoyal/kitty/blob/master/kitty/options/definition.py)
- [Kitty hover and click dispatch](https://github.com/kovidgoyal/kitty/blob/master/kitty/mouse.c)
- [Kitty pointer shape protocol](https://sw.kovidgoyal.net/kitty/pointer-shapes/)

Wisp does not load Kitty's configurable `mouse_map`, `open_actions`, or custom URL
regular expressions. Shell-integration prompt click movement and drag-and-drop
link transfer are separate features.
