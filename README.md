# wisp

wisp is a terminal emulator written in Rust based on `libghostty-vt`,
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

copy [`config.example.toml`](config.example.toml) to your chosen configuration
path.

## build prerequisites

first install `mise`, then:

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

## validation

```sh
cargo test --all-targets
cargo build --release
cargo build --example keyboard_driver --example pointer_driver
python3 tests/gui_smoke.py
```
