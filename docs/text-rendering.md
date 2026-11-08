# Text shaping and cell colors

Wisp shapes adjacent text with the same font style together, even when its
foreground, selection, background, or decoration colors differ. Bold and italic
changes still create font-style boundaries. This follows Kitty's separation
between [font runs and cell sprites](https://github.com/kovidgoyal/kitty/blob/master/kitty/fonts.c):
foreground changes do not need to interrupt OpenType substitutions.

For example, FiraCode Nerd Font's `~>` keeps its ligature when `~` is green
and `>` is white. The glyph mask is shaped once, then split at terminal-cell
color boundaries. Each fragment receives the corresponding foreground or
selection color. Glyph fragments share the existing atlas cache; their cache
keys include the original glyph and pixel slice, so recoloring does not create
new raster data. Intrinsically colored emoji retain their original pixels.

Cursor-only frames and unchanged text rows keep their existing caches. Disabling
ligatures still disables the configured OpenType ligature/contextual features.

Validation uses actual FiraCode font substitutions and GPU pixel readback. It
compares uniform and multicolor `~>`, arrows, inequality signs, and longer
ligatures, checking that the pixel mask is unchanged and each cell retains its
color. Selection boundaries are included.

```sh
cargo test --lib ligature
```
