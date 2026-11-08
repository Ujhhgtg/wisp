# wisp

Wisp is a Linux terminal emulator written in Rust based on `libghostty-vt`,
`winit`, `wgpu`, and `glyphon`.

## features

- gpu rendering
- ligatures
- kitty keyboard protocol
- kitty graphics protocol
- mouse
- hyperlinks
- animated cursor

## configuration

wisp checks these paths in order and loads the first file that exists:

1. `$XDG_CONFIG_HOME/wisp/config.toml`
2. `$HOME/.config/wisp/config.toml`
3. `./.config/wisp/config.toml`

Copy [`config.example.toml`](config.example.toml) to your chosen configuration
path.

## Build prerequisites

`libghostty-vt-sys 0.2.2` builds the pinned Ghostty source with Zig. Use Zig
`0.15.2`. The project also needs the normal Linux development libraries
for Wayland/X11 and a Vulkan or GLES driver.

```sh
zig version              # should report 0.15.2
cargo run --release
```

To launch another program or an explicit config:

```sh
cargo run --release -- --config /path/to/config.toml -e /bin/bash
```
