# Terminal conformance profile

Wisp targets a modern UTF-8 Linux terminal: commonly used ECMA-48 controls,
VT100/VT220 screen and keyboard behavior, selected later DEC controls, and
xterm-compatible color/query conventions. Kitty keyboard, graphics, and mouse
extensions are additional protocols. This is a concrete compatibility profile,
not a claim to implement every optional feature of every historical terminal.
ECMA-48 itself describes a selection of controls appropriate to each device.
[ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/)

## Automated coverage

`tests/standard_vt.rs` exercises the production stream, snapshot builder, and
native state, rather than a separate parser model. It includes 37 tests with
table-driven cursor/erase/edit cases and arbitrary stream chunk boundaries.

| Area | Verified behavior |
| --- | --- |
| Control bytes | BS, CR, LF/VT/FF, newline mode, ignored NUL/DEL/unused C0 controls |
| Cursor | Absolute/relative/default/zero/clipped parameters, save/restore, delayed wrap, no-wrap |
| Tabs | Default and custom stops, default/explicit TBC, forward/backward movement |
| Character sets | DEC special line drawing, G0/G1 designation, SI/SO, saved charset state |
| Margins | Vertical scrolling regions, origin mode, left/right margins, IND/RI/NEL, scroll up/down |
| Editing | Insert/replace mode, ICH/DCH/ECH, IL/DL, inclusive EL/ED ranges, background-color erase |
| Protection | DECSCA protected characters and selective erasure versus ordinary erasure |
| Rendition | Bold/faint/italic/blink/inverse/invisible/strike/overline, underline types/colors, independent resets |
| Colors | ANSI/indexed/RGB SGR, colon/semicolon forms, reverse-screen/default-color interaction |
| Buffers | Alternate screen and cursor restoration, history erasure, primary/history isolation |
| Reports | DSR, cursor position including origin mode, ANSI/private DECRQM, private mode save/restore |
| OSC | BEL/ST title framing, UTF-8 titles, palette setting and matching-terminator queries |
| DCS | DECRQSS rendition/margins/protection/cursor style; XTGETTCAP known/unknown capability replies |
| Reset | DECSTR resets terminal modes/pen/margins/charsets without erasing text or custom tabs; RIS restores initial state |
| Stream | Mixed Unicode, CSI, OSC, DCS and replies remain identical across 1–31 byte chunks |

Related suites cover PTY geometry, focus/paste/input negotiations, clipboard
shortcuts, selection, installed fonts, GPU color/decoration rendering, cursor
animation, and Kitty graphics. These are separate layers: correct parser state
does not by itself establish correct GPU pixels or desktop input delivery.

```sh
cargo test --test standard_vt
cargo test --all-targets
```

The native patches additionally have focused tests for stream control handling,
soft reset, DCS responses, and full rendition reporting. New reset/action tags
are appended so existing native action values retain their numbering.

Soft reset restores autowrap to Wisp's configured/default mode rather than
forcing it off. This follows xterm's modern compatibility behavior; historical
DEC reset tables instead specify autowrap off. It also restores the configured
cursor shape/blinking while retaining existing cells, history, and custom tabs.

## External VTTEST check

[VTTEST](https://invisible-island.net/vttest/) is an interactive compatibility
program whose screens normally require visual inspection. Wisp provides a
bounded external integration check rather than equating a successful launch
with conformance. The test starts the actual VTTEST executable in a production
PTY Session and checks its resulting screen cells and interpreted reports.

```sh
WISP_VTTEST=/path/to/vttest cargo test --test vttest_integration -- --ignored
```

The executable is an explicit external prerequisite. This test is marked
ignored in the ordinary Cargo run; it is not silently counted as a passing
test when VTTEST is absent.

Validated with VTTEST 2.7 (20251205):

- The cursor-movement chapter's 80- and 132-column border/E-box screens.
- The embedded-control screen's four identical character rows.
- The leading-zero cursor-addressing screen's expected sentence.
- The DSR report screen's terminal-status and origin-relative cursor results.
- The secondary device-attributes report's parsed fields.

This does not establish that every VTTEST menu, legacy mode, or visual feature
passes. The internal tests derive their expected control semantics from the
[xterm control reference](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
and DEC's [VT100](https://vt100.net/docs/vt100-ug/chapter3.html) and
[VT220](https://vt100.net/docs/vt220-rm/) manuals.

## Boundaries

The profile does not claim Tektronix/ReGIS, SIXEL, VT52 emulation, downloadable
DEC character sets, printer/serial hardware functions, or double-width and
double-height DEC text lines. It also does not claim the full VT420 rectangle,
page, and checksum command set.

The upstream [esctest suite](https://gitlab.freedesktop.org/terminal-wg/esctest)
was inspected as another source of behavioral cases. Its current checkout uses
Python 2.7 and relies on VT420 `DECRQCRA` checksums for many screen assertions.
Wisp does not currently claim that checksum extension, so no blanket esctest
pass rate is reported. Wisp's screen-cell assertions avoid adding an unadvertised
test-only checksum implementation solely to make that suite appear supported.

Window manipulation and desktop effects remain subject to the window system.
Keyboard, mouse, and compositor smoke tests are validated separately from these
headless screen/state checks. Graphics-specific bounds and resource limits are
documented in [Kitty graphics](kitty-graphics.md).

## Host rendering and input

The host renderer preserves underline style and color, strikeout, overline,
conceal, and blink attributes. Blink reshapes only affected rows and has no
timer when no cells blink. A block cursor retains readable text; an unfocused
block cursor uses an outline. Visual bells have a bounded lifetime. IME preedit
is rendered locally at the terminal cursor and is sent to the child only when
committed. Font-size changes update the PTY geometry.

The process environment selects an installed compatible terminfo description,
preferring `xterm-ghostty`, then `xterm-256color`, `xterm`, or `vt100`. An explicit
program `TERM` override is respected. `TERM_PROGRAM=wisp`, its version, and
`COLORTERM=truecolor` identify the host. The lookup is read-only and supports
normal, hexadecimal-directory, and inline ncurses entries.

Legacy Ctrl+I/M/[ aliases, modified F3, and F13–F24 follow the advertised xterm
encodings. Negotiated Kitty keyboard encoding retains disambiguation and key
identities. Horizontal wheel and extra mouse buttons are encoded independently
from vertical scrolling. Shortcut key releases are consumed along with their
terminal-owned presses.

OSC 52 and iTerm2 text clipboard writes use the same application clipboard path
as copy/selection. `protocols.clipboard_write` defaults to true and can disable
those application writes. Clipboard read requests remain ignored. Version and
color-scheme queries identify Wisp and use the effective terminal background.

## General GUI smoke

```sh
cargo build --release
cargo build --example keyboard_driver --example pointer_driver
python3 tests/gui_smoke.py
```

The smoke test verifies actual window pixels and input delivery: configured
font/background/margins, ANSI colors against truecolor references, both detected
and OSC 8 hyperlinks, exact SGR mouse coordinates, an 80×40 `icat` image, typed
text, drag selection and `Ctrl+Shift+C/V`, resize/SIGWINCH, last-cell rendering,
and idle CPU.

To isolate the application from compositor effects, the harness overrides
opacity, rounding, and animations only for windows it creates. Geometry comes
from actual PTY/pixel-size reports. Clipboard/PRIMARY and prior focus are
restored in cleanup. Checks are not skipped to accommodate transparency.

The fixed-US virtual keyboard helper is required for physical-key shortcut
testing: `wtype` can install a custom keymap whose physical letter positions
do not correspond to the requested characters. Dependencies include Hyprland
Lua dispatchers, `grim`, `wtype`, `xkbcli`, `wl-copy`, `wl-paste`, `kitten`, Python,
and Pillow. Artifacts are written to `target/gui-smoke/`.

The validated full run passes 259 automated tests, plus the explicitly invoked
external VTTEST integration. The latter is one ignored test in a normal Cargo
run and is executed separately with the external binary available. The final
release also passes the full general GUI smoke command above, with no skipped
stages or assertions.

Dialog compatibility is checked against the actual `kitten ask` executable in
`tests/dialog_conformance.rs`: cell placement, glyphs, colors, bold attributes,
and compact fallback layouts match at multiple grid sizes. The close and paste
GUI suites verify keyboard/mouse routing and restore the terminal after cancel.
Paste tests additionally assert exact approved/sanitized bytes, warning
suppression for bracketed newlines, and bracketed-paste terminator removal.

## Performance validation

Measured on 2026-11-08 using the installed `kitten __benchmark__`, the production
PTY/VT path, a 160×48 grid, and 100,000 lines of scrollback:

```sh
cargo run --release --example kitten_benchmark -- --repetitions 30
```

| Workload | Reported MB/s |
| --- | ---: |
| ASCII | 487.6 |
| Unicode | 868.6 |
| Unique multi-codepoint cells | 84.0 |
| CSI with few characters | 154.5 |
| Long escape codes | 1074.5 |
| Images | 1086.8 |

A longer, 100-repetition long-escape run improved from 128.7 MB/s before the
DCS/OSC performance fixes to 1090.3 MB/s afterward. DCS state is allocated on
first use; already-discarded OSC ASCII is skipped in batches without consuming
its terminators or cancellation controls. These measurements exclude GPU
rendering and do not establish physical input-to-display latency.

The production input-latency probe measured key-to-snapshot p99 values of
0.117–0.171 ms across 20/35/50 ms input intervals, with 100 keys per interval.
The final general GUI smoke used 0.03 seconds of process CPU over its two-second
idle interval.
