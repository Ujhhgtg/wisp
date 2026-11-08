# wisp

wisp is a Linux terminal emulator written in Rust based on `libghostty-vt`,
`winit`, `wgpu`, and `glyphon`.

## features

- gpu rendering
- ligatures
- kitty keyboard protocol
- kitty graphics protocol, including animations, Unicode placeholders, and relative placements
- mouse
- hyperlinks
- animated cursor
- colored underline styles, strikeout, overline, conceal, and blinking text
- OSC 52 clipboard writes and IME preedit

The tested VT/xterm compatibility profile, external VTTEST checks, and legacy
boundaries are documented in [`docs/conformance.md`](docs/conformance.md).

Graphics commands, resource limits, and validation instructions are documented in
[`docs/kitty-graphics.md`](docs/kitty-graphics.md).
Ligatures preserve per-cell colors across color and selection changes; see
[`docs/text-rendering.md`](docs/text-rendering.md).

## configuration

wisp checks these paths in order and loads the first file that exists:

1. `$XDG_CONFIG_HOME/wisp/config.toml`
2. `$HOME/.config/wisp/config.toml`
3. `./.config/wisp/config.toml`

copy [`config.example.toml`](config.example.toml) to your chosen configuration
path.

## build prerequisites

`libghostty-vt-sys 0.2.2` builds the pinned Ghostty source with Zig `0.15.2`.

```sh
zig version              # should report 0.15.2
cargo run --release
```

## usage

```sh
cargo run --release
```

to launch another program or an explicit config:

```sh
cargo run --release -- --config /path/to/config.toml -e /bin/bash
```

`Ctrl+Shift+C/V` copies and pastes, `Shift+PageUp/PageDown` scrolls history,
and `Ctrl+Shift+Plus/Minus/0` increases, decreases, or resets font size.
`[protocols] clipboard_write = false` disables application clipboard writes;
clipboard-read requests are ignored.

## validation

```sh
cargo test --all-targets
cargo build --release
cargo build --example keyboard_driver --example pointer_driver
python3 tests/gui_smoke.py
```

The general GUI smoke checks fonts, colors, margins, hyperlinks, exact mouse
bytes, image pixels, real copy/paste, resizing, and idle CPU. It requires Hyprland
with Lua dispatchers, `grim`, `wtype`, `xkbcli`, `wl-copy`, `wl-paste`, `kitten`,
Python, and Pillow. It makes only its own test windows opaque, preserves the
clipboard/PRIMARY, and restores focus afterward.
