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

The English-only confirmation follows Kitty's `kitten ask` cell layout,
including the centered bold message, compact three-row box-drawing buttons,
colored shortcut letters, and yellow default-choice frame. `Y` / `N`, `Enter`,
`Esc` / `Ctrl+C`, and mouse clicks match Kitty. Enter selects Yes, as in Kitty.
Job inspection runs only on close, with no periodic process polling.

Clipboard and primary-selection pastes use Kitty's default control-code warning:
control characters (including ESC, CR, and DEL) require confirmation, as do
newlines when bracketed paste is disabled. The dialog offers **Sanitize and
paste** (`S`, the default), **Paste anyway** (`A`), and **Cancel** (`C`). Sanitizing
replaces control characters with visible control pictures and unbracketed
newlines with NEL (`ESC E`). Paste anyway preserves the original control codes;
bracketed-paste terminators are removed before wrapping, as in Kitty. Ordinary
text and bracketed newlines do not warn. The policy applies at the live VT state
when the paste is requested. Kitty's optional large-paste warning is not enabled
by default and is not part of this policy.

## validation

```sh
cargo test --all-targets
cargo build --release
cargo build --example keyboard_driver --example pointer_driver
python3 tests/gui_smoke.py
python3 tests/gui_close_confirmation.py
python3 tests/gui_paste_warning.py
```
