# Window margin and grid geometry

`[window].margin_width` matches Kitty's `window_margin_width` point units and
edge ordering. Wisp's default is **21.75 pt**, equivalent to 29 physical pixels
at scale factor 1.0 and 58 pixels at scale factor 2.0. Set it to `0` for no
intentional margin.

```toml
[window]
margin_width = 21.75
```

Values can be a scalar or an array of one to four values:

| Value | Edges |
| --- | --- |
| `12` or `[12]` | All four sides |
| `[6, 12]` | Vertical, horizontal |
| `[3, 12, 6]` | Top, horizontal, bottom |
| `[3, 6, 9, 12]` | Top, right, bottom, left |

The alias `window_margin_width` is also accepted inside `[window]`.
All values must be finite and non-negative.

`GridGeometry` converts points to physical pixels using `96 / 72 * scale_factor`,
rounds each edge, and calculates the usable terminal columns and rows from the
actual font cell dimensions. Any sub-cell remainder is split between the two
sides, with the extra odd pixel on the right or bottom. This matches Kitty's
default `placement_strategy = center`.

Rendering, text selection, mouse reports, hyperlinks, image placements, and PTY
pixel-size reports must use the same content origin and size. Margins and
sub-cell remainder are included in the mouse encoder's padding fields and
excluded from the terminal's pixel dimensions. Hovering a margin does not
resolve a terminal cell; a selection drag outside the content can use the
clamped conversion while requesting autoscroll.

If the window is too small to fit its requested margins and one cell, margins
shrink proportionally. A window smaller than a single font cell keeps a 1×1
terminal and clips that cell to the available pixels. No coordinate or padding
calculation underflows.

References:

- [Kitty window margin](https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.window_margin_width)
- [Kitty placement strategy](https://sw.kovidgoyal.net/kitty/conf/#opt-kitty.placement_strategy)
