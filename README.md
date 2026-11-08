# Wisp

Wisp is a Linux terminal emulator written in Rust using Ghostty VT,
`winit`, `wgpu`, and `glyphon`. It runs on Wayland and X11, shapes installed
fonts with OpenType ligatures, and anchors shaped glyph clusters to the
terminal cell grid.

The terminal implements Kitty keyboard and mouse modes, bracketed paste,
PRIMARY selection, `Ctrl+Shift+C/V`, OSC 8 links, detected URLs, OSC 22 mouse
shapes, and Neovide's per-corner cursor spring animation. Links and mouse
actions follow Kitty's default mappings; custom Kitty `mouse_map` settings
are not imported.

Kitty graphics are decoded and rendered on the GPU. Supported transfers
include raw RGB/RGBA, PNG, compressed/chunked data, files, temporary files,
and shared memory. Placements support source cropping, scaling, clipping,
scrolling, and z layers. Decoded pixels and GPU textures are cached by image
generation. Regular `kitten icat image.png` works; Unicode placeholder
placements and animated image frames are not yet implemented.

## Configuration

Wisp checks these paths in order and loads the first file that exists:

1. `$XDG_CONFIG_HOME/wisp/config.toml`
2. `./.config/wisp/config.toml`
3. `$HOME/.config/wisp/config.toml`

Copy [`config.example.toml`](config.example.toml) to your chosen configuration
path. Font size is expressed in points and defaults to `11`; the cursor
defaults to `beam`. `scrollback_limit` defaults to `100000`; `-1` retains history
without a configured limit, and `0` disables it. Positive values currently use
the underlying Ghostty storage limit in bytes.

```toml
[font]
family = "FiraCode Nerd Font"
size = 11.0
ligatures = true

[window]
margin_width = 21.75
```

`window.margin_width` uses Kitty's point units and accepts a scalar or an
array of 1–4 values: all edges; vertical/horizontal; top/horizontal/bottom;
or top/right/bottom/left. The default `21.75` becomes 29 physical pixels
at scale 1. Cell centering, text, cursor, images, mouse coordinates, and
terminal size queries use the same geometry.

Details: [window geometry](docs/window-geometry.md),
[links and pointer states](docs/hyperlinks.md),
[cursor animation provenance](docs/cursor-animation.md).

## Build prerequisites

`libghostty-vt-sys 0.2.2` builds the pinned Ghostty source with Zig. Use Zig
`0.15.2` (the crate's required version) and make sure the Zig dependency URLs
are reachable. For a local checkout, set `GHOSTTY_SOURCE_DIR` to a compatible
Ghostty source tree. The project also needs the normal Linux development
libraries for Wayland/X11 and a Vulkan or GLES driver.

Wisp vendors the small native build crate to apply grapheme-table and repeated
character performance fixes to the fetched Ghostty source and to preserve native
optimization when building release binaries with debug symbols. Custom source
checkouts remain untouched. Details and memory costs are in
[the native build notes](vendor/libghostty-vt-sys/WISP.md).

```sh
zig version              # should report 0.15.2
cargo run --release
```

To launch another program or an explicit config:

```sh
cargo run --release -- --config /path/to/config.toml -e /bin/bash
```

If a proxy rejects Ghostty's pinned Zig package URLs, unset the proxy
variables for the build:

```sh
env -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY \
    -u http_proxy -u https_proxy -u all_proxy cargo build --release
```

## Verification

```sh
cargo test --all-targets
cargo run --release --example input_latency -- 100 20 35 50
cargo run --release --example throughput -- 16
cargo run --release --example kitten_benchmark -- --repetitions 30 unique_unicode csi long_escape_codes
```

The tests exercise the production snapshot/input paths, real PTY subprocesses,
installed font shaping, and Vulkan GPU pixel readback. Graphics integration
also invokes an installed `kitten icat` using automatic, stream, file, and
shared-memory transfers. Python 3 is needed for PTY fixtures; `kitten` is
needed for the external graphics test. GPU tests require a working adapter
and the FiraCode Nerd Font and DejaVu Sans Mono test fonts.

`input_latency` measures key-to-PTY-echo and key-to-snapshot latency separately.
`throughput` compares direct Ghostty VT parsing with Wisp's PTY/VT/snapshot
path. These benchmarks explicitly exclude window/GPU presentation time.
`kitten_benchmark` runs the installed Kitty benchmark through the production
Session at 160×48, using the normal configuration search paths. A real-window
benchmark and measured comparisons are described in
[performance notes](docs/performance.md).

A real Wayland/Hyprland window smoke test is available:

```sh
cargo build --release
cargo build --example pointer_driver
python3 tests/gui_smoke.py
python3 tests/gui_latency.py --idle 0.1 1 5 15
python3 tests/gui_latency.py --blink --idle 0.1 1 5 15
python3 tests/gui_nano.py
```

It opens temporary Wisp windows, sends virtual-pointer and keyboard input,
checks URL/OSC 8 clicks through a fake opener, captures actual `icat` pixels,
and measures idle CPU. It requires Hyprland with Lua dispatchers, `grim`,
`wtype`, Python Pillow, and `kitten`. Screenshot comparisons tolerate compositor
opacity/color management; offscreen GPU tests assert exact pixel values.
Artifacts are written to `target/gui-smoke/`.

`gui_latency.py` uses `WISP_TRACE_TIMING=1` to log input, PTY, layout,
surface acquisition, and frame presentation timestamps. It measures the
application's present call, not the display's physical scanout. The tracing
flag is disabled by default and does not log typed text.
`gui_nano.py` runs an actual Nano window with mouse support and confirms
intermediate cursor-animation frames after a click.

## Rendering and Scheduling

PTY bytes remain on a bounded channel and pending frames use a single latest
snapshot slot. Continuous bulk output batches snapshots; interactive key
echo bypasses that bulk timer. The VT thread sleeps until input or an actual
deadline. Winit redraws on terminal updates, cursor animation, or blink and
uses Wayland frame callbacks. Unchanged text rows reuse shaped glyphs, cursor
frames reuse prepared text, and unchanged images are not decoded or uploaded
again.

Synchronized output (`CSI ? 2026 h/l`) suppresses screen snapshots while parsing
and query replies remain live. Each enable request refreshes a one-second
watchdog; a missing reset cannot leave the display frozen indefinitely.
