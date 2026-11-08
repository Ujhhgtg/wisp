# Wisp native build changes

This directory vendors the `libghostty-vt-sys` 0.2.2 crate from crates.io,
published from `uzaaft/libghostty-rs` commit
`c9876b2902d5bb3e1f3d61b6395059f1e5e7b721`. Bindings and Rust FFI helpers
are unchanged. The upstream crate is licensed MIT OR Apache-2.0.

The native Ghostty source stays pinned to
`a887df42c56f6de86c0fe6da9c4eeca37931e083` and is fetched by the original
build script. Wisp changes only:

- Cargo file-change tracking uses the local paths.
- Optimized Cargo profiles select Zig ReleaseFast/ReleaseSmall even when
  Rust debug symbols are enabled. `LIBGHOSTTY_VT_SYS_OPTIMIZE` remains an
  explicit override; unoptimized debug builds still select Zig Debug.
- The fetched source receives `patches/grapheme-map.patch` before building.
  The grapheme table uses an 80% maximum load and reports its usable capacity
  to reflow code. Fully occupied tables otherwise spend most combining-text
  parsing time probing and deleting hash entries.
- `patches/repeat-character.patch` batches printable ASCII REP commands through
  the existing `printSlice` path. It follows upstream commit
  `5b70f208bc6870bb32f13720376840edc3c5ed31`, restricted to ASCII and a 128-codepoint
  buffer (512 bytes of stack). Other characters retain the original loop.

For the standard 512-entry grapheme table, raw table slots increase from 512
to 1024: 10,768 to 21,520 bytes on x86_64. Cell storage and grapheme text
allocation are unchanged. This increases the total page size, so unlimited
scrollback can consume additional memory.

An explicit `GHOSTTY_SOURCE_DIR` remains authoritative and is never modified
by this build script. To benchmark a custom checkout with the same fix,
apply both patches to that checkout yourself. A pkg-config library also remains
external and is not patched.

Validation: Ghostty's `Page` native test filter passes 821 tests with both
patch changes. Wisp additionally exercises dense combining characters with
mixed-width text across repeated resize/reflow operations.
The applicable native `printRepeat`/`printSlice` and Stream suites pass 297
tests. A separate differential test compares scalar and batched REP across
four small grids, fourteen mode/charset/style contexts, ASCII/CJK/emoji, and
counts up to 8193, including cell contents, styles, protection, wrapping,
cursor state, and page integrity. The entire Terminal suite is not claimed:
an existing `fullReset tracked pins` test cannot compile in ReleaseFast because
it requires runtime-safety checks.
