# Cursor animation

`src/cursor.rs` adapts Neovide's cursor motion implementation at revision
`86cae876492e7ac8aa89be816ef4f217326f5289`:

- [Cursor renderer](https://github.com/neovide/neovide/blob/86cae876492e7ac8aa89be816ef4f217326f5289/src/renderer/cursor_renderer/mod.rs)
- [Spring animation](https://github.com/neovide/neovide/blob/86cae876492e7ac8aa89be816ef4f217326f5289/src/renderer/animation_utils.rs)
- [Upstream MIT license](https://github.com/neovide/neovide/blob/86cae876492e7ac8aa89be816ef4f217326f5289/LICENSE)

The retained copyright and permission notice is in `LICENSES/Neovide-MIT.txt`.

Motion uses Neovide's analytical critically damped spring, the same
`omega = 4 / animation_length`, the same 0.01-pixel settling threshold, and
the same independent springs for the four cursor corners. Long jumps use
the direction-normalized leading/trailing corner durations; the default
leading corners move immediately and the trailing corners take 150 ms.
Horizontal jumps of at most two cells use Neovide's 40 ms short-jump
duration for every corner. The result is one deforming quadrilateral.

The beam is two logical pixels wide and one full cell high, matching
[Zed's default bar cursor](https://github.com/zed-industries/zed/blob/b5cd310b3b54dfc659fcc06453e6358843dfe216/crates/editor/src/element.rs#L11081).
Zed's terminal uses the same `CursorLayout` as its editor. Wisp scales the beam
width with the display scale factor, independently of font size. The underline
thickness remains one eighth of a cell, matching Neovide; `cell_percentage`
controls that thickness. Disabling `trail` gives every corner the same
duration; disabling `smooth` snaps all corners immediately.

Wisp snaps the first appearance, reappearance after a prolonged hide, and
cell-size changes to the new cursor rectangle. Temporary hides shorter than
250 ms preserve the previous corners so ncurses redraws, including Nano
mouse positioning, retain cursor movement animation. A new destination's
clock starts when that destination arrives, excluding earlier idle time.
These are deliberate terminal integration rules. Neovide's Neovim-specific insert-mode, command-line,
multi-grid scrolling, particle effects, and Skia antialiasing are outside
this module; Wisp's renderer is responsible for rasterizing the resulting
quadrilateral and handling blink/focus.

The numerical regression tests include the upstream default trajectory
after one 60 Hz frame: an 80-pixel long jump leaves the trailing corner at
5.908044 pixels, and an 8-pixel short jump places it at 3.970654 pixels.
They also check frame splitting, settling, shape transforms, and fractional
pixel origins.
