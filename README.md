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

Graphics commands, resource limits, and validation instructions are documented in
[`docs/kitty-graphics.md`](docs/kitty-graphics.md).

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
