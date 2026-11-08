# Performance measurements

Use release builds for throughput measurements. Wisp keeps the native Ghostty
parser optimized even when release debug symbols are enabled. The dependency's
original build script treated Cargo's `DEBUG=true` as Zig Debug regardless of
`OPT_LEVEL`, enabling expensive page-integrity checks in combining-text paths.

## Kitty benchmark comparison

Measured on the same machine on 2026-11-08, with the same configuration at
`~/.config/wisp/config.toml`, `scrollback_limit=10000`, a 1600×900 Wayland
window, and a 171×44 terminal grid:

```sh
python3 tests/gui_benchmark.py --repetitions 30 --label current \
  unique_unicode csi long_escape_codes
```

| Workload | Before | After | Ratio |
| --- | ---: | ---: | ---: |
| Unique multi-codepoint Unicode cells | 22.2 MiB/s | 86.9 MiB/s | 3.91× |
| CSI codes with few characters | 78.9 MiB/s | 162.7 MiB/s | 2.06× |
| Long escape sequences | 157.8 MiB/s | 176.7 MiB/s | 1.12× |

Both versions used optimized native libraries; an accidentally unoptimized
diagnostic binary is excluded. The script records the actual terminal grid,
arguments, configuration path, and report in `target/gui-benchmark/`.
The paired reports are `optimized-before.json` and `final.json`. A subsequent
full six-workload run is recorded in `final-all.json`: ASCII 563.1, Unicode
1029.5, unique Unicode 86.0, CSI 157.5, long escapes 170.8, and images
1134.2 MiB/s. Short-run results vary slightly between invocations.

This invokes the installed `kitten __benchmark__`. Its data goes through the
controlling PTY while its report is collected separately. Rendering is
suppressed by default. `--render` allows rendering, but Kitty's timings still
measure parsing and query completion rather than physical display latency.

Kitty generates random data for CSI and long-escape workloads. Ratios can vary
between runs. The default benchmark also uses the alternate screen, so it does
not measure scrollback retention even when a positive limit is configured.
Use `--with-scrollback` to exercise the main screen. Window dimensions matter,
especially for the combining-character workload.

## Changes and tradeoffs

- Synchronized output suppresses production snapshots, cell extraction, image
  capture, and resulting text shaping. Query replies remain live. Resetting
  mode 2026 publishes the completed screen promptly; repeated enable requests
  refresh a one-second watchdog. In the 160×48 production-Session benchmark,
  snapshots consumed fell from 586 to 6 across the three 30-repetition cases.
- The pointer-shape observer skips ordinary text and irrelevant OSC/DCS/APC/PM
  payloads using byte searches, buffers only bounded OSC 22 content, and parses
  relevant private CSI parameters without a temporary vector.
- The native grapheme table uses an 80% maximum load. Profiling the original
  deterministic Unicode fixture placed about 85% of CPU samples in grapheme
  insertion/deletion hash probes. At 160×48, direct native parsing increased
  from 18.85 to 87.69 MiB/s in a controlled 20-repetition comparison.
- Printable ASCII REP commands of at least eight characters use the existing
  batched `printSlice` path with a fixed 512-byte stack buffer. This follows
  [Ghostty's upstream REP optimization](https://github.com/ghostty-org/ghostty/commit/5b70f208bc6870bb32f13720376840edc3c5ed31),
  restricted to ASCII. Other characters retain their original scalar path.

The grapheme patch also changes the reported usable capacity to `maxLoad()`;
changing only the load factor can break reflow capacity checks. The native
`Page` test filter passes 821 tests with both changes, and Wisp verifies dense
combining/CJK text across repeated resizing. A standard grapheme table grows
by 10,752 bytes on x86_64; cell storage and grapheme text allocations do not
double. See [native build details](../vendor/libghostty-vt-sys/WISP.md).

Kitty's CSI fixture includes `REP 100`, which expands compact commands into
substantial terminal-cell work. A deterministic fixture using Kitty's branch
weights measured 89.47 MiB/s for direct Ghostty before the REP change and
214.29 MiB/s afterward at 160×48. Applicable native `printRepeat`, `printSlice`,
and Stream suites pass 297 tests; scalar/batched differential checks cover small
grids, terminal modes, character sets, wrapping, styles, and protection. Wisp's
production tests also compare REP with literal output around batch boundaries.

Keyboard-to-snapshot release measurements after these changes show
0.10–0.16 ms at the 99th percentile for 100-key samples at 20/35/50 ms intervals.
These timings exclude GPU presentation and are not a claim about physical
keyboard-to-display latency.

## Focused diagnostics

```sh
cargo run --release --example kitten_benchmark -- --repetitions 30 unique_unicode csi long_escape_codes
cargo run --release --example unicode_profile -- 20 160 48
cargo run --release --example pointer_throughput -- 64
cargo run --release --example input_latency -- 100 20 35 50
```

The headless diagnostics exclude GPU/window presentation. `unicode_profile`
compares direct VT parsing with and without periodic production snapshots;
it intentionally requests snapshots even while mode 2026 is set to isolate
their cost. `pointer_throughput` measures the output observer separately;
its hot-cache byte-search rates are not whole-terminal throughput.

## Combined graphics and rendering review

The 2026-11-08 review of the VT conformance, graphics transport, and multicolor
ligature fixes measured these additional changes against commit `3d1de3f`:

| CPU workload | Before | After |
| --- | ---: | ---: |
| Background-only updates, 160×48 FiraCode grid | 5451.6 µs/frame | 92.7 µs/frame |
| Cursor-only updates, same grid | 23.1 µs/frame | 23.0 µs/frame |
| Dirty uniform text, same grid | 2136.0 µs/frame | 2174.0 µs/frame |
| Dirty alternating-color text, same grid | 2382.7 µs/frame | 2391.8 µs/frame |
| Image-upload packet serialization | 3556–3559 MiB/s | 4490–4509 MiB/s |

The text comparison uses 300 updates per workload with the production
`TextLayout`. Background and decoration changes now retain shaped glyphs and
update the cached attributes without invoking shaping. Cursor-only caching is
preserved; ordinary dirty-text costs remain within short-run variation. A
maintained diagnostic resets the scene before each workload and uses the
configured font:

```sh
cargo run --release --example text_throughput -- 300
```

Upload serialization reuses the first header and a base64 packet buffer instead
of allocating maps, strings, and packets for every 3072 payload bytes. A paired
microbenchmark of 40 eight-MiB uploads measures approximately 26% higher
serialization throughput, with allocation/reallocation calls decreasing from
19,127 to 15 per upload. Framing tests verify control fields, continuation flags,
base64 chunk limits, and exact reconstructed bytes across chunk boundaries.
These are component CPU measurements, excluding PTY parsing, GPU work, and
physical display latency; they are not whole-terminal speedup claims.

A longer paired production GUI run compares the imported fixes with the reviewed
build, using `--render --repetitions 100`, a 1600×900 window, a 171×44 grid, and
the same user configuration with 100,000 lines of scrollback:

| Kitty workload | Imported fixes | Reviewed build |
| --- | ---: | ---: |
| Unique multi-codepoint Unicode cells | 79.4 MB/s | 80.6 MB/s |
| CSI codes with few characters | 147.9 MB/s | 148.9 MB/s |
| Long escape codes | 1088.4 MB/s | 1101.2 MB/s |

Reports are saved as `target/gui-benchmark/{imported,reviewed}-render-100.json`.
These longer runs do not reproduce the small slowdown observed in an initial
30-repetition comparison. Kitty's timings still measure parsing/query completion
with rendering enabled, rather than physical display latency. The reviewed
build's key-to-snapshot p99 is 0.117–0.171 ms across 100-key samples at 20/35/50 ms
intervals. All four GUI smoke commands pass, including default `icat` GIF
playback after client exit; the general smoke uses 0.03 seconds of process CPU
over its two-second idle interval.
