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

`[window] confirm_on_close = true` (the default) asks before closing when the
default shell has a running foreground/background or stopped job, or a program
launched with `-e` is still running. An idle default shell closes immediately.
Replacing the default shell with `exec` also asks before closing. Set it to
`false` to always close without asking.

The confirmation fills the window and supports mouse clicks, `Y`/`N`,
`Tab`/arrow keys, `Enter`, and `Esc`. “Keep open” is selected initially.
It uses English, Simplified Chinese, or Traditional Chinese according to
`LC_ALL`, then `LC_MESSAGES`, then `LANG`; unsupported locales use English.
Chinese script subtags are respected, with CN/SG selecting Simplified and
TW/HK/MO selecting Traditional when no script is specified. Job inspection
runs only on close; it does not poll while the terminal is idle or parsing.

## validation

```sh
cargo test --all-targets
cargo build --release
cargo build --example keyboard_driver --example pointer_driver
python3 tests/gui_smoke.py
python3 tests/gui_close_confirmation.py
```
