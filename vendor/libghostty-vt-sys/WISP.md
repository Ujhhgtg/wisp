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
- `graphics-conformance.patch` corrects range deletion, upload cancellation,
  automatic ID collisions, and adds the borrowed image-number lookup used by
  Wisp's safe binding extension.
- `graphics-relative.patch` adds parent validation, 32-level chains, cycle
  detection, parent-following geometry, virtual parents, and cascading deletion.
  `graphics-relative-limits.patch` and `graphics-relative-anchors.patch` extend
  cleanup after eviction and exclude invisible placeholder fragments from anchors.
- `graphics-replace.patch` validates query data, clears old placements on a
  successful replacement, implements exact local-file offset/length reads, and
  reports uniform file errors with regular-file checks before opening.
- `graphics-geometry.patch` uses clipped source extents for size calculations
  and clamps placement offsets to a cell.
- `standard-vt.patch` ignores DEL/unused C0 bytes, handles default TBC and ANSI
  mode reports, and implements DECSTR without erasing text/history or tabs.
- `standard-dcs.patch` uses the existing DCS parser for DECRQSS and XTGETTCAP
  in the embedded VT handler, including negative responses.
- `standard-rendition.patch` reports extended underline styles, underline color,
  and overline accurately in rendition status responses.
- `standard-host-reset.patch` restores the host's configured cursor appearance
  and blink defaults during DECSTR.
- `standard-performance.patch` allocates DCS state only when needed and skips
  discarded OSC ASCII in batches while preserving control-byte handling.
- `printable-runs.patch` finds printable UTF-32 runs with SIMD comparisons before
  dispatching them to the existing print-slice handler. C0 and DEL still use
  the established control path; UTF-8 decoding and cell writing are unchanged.

The build records the complete patch set in its private Cargo output directory.
When the patch set changes, only its managed source files are restored to the
pinned commit before applying all patches in order. Repeated builds do not
attempt to apply overlapping incremental patches twice.

For the standard 512-entry grapheme table, raw table slots increase from 512
to 1024: 10,768 to 21,520 bytes on x86_64. Cell storage and grapheme text
allocation are unchanged. This increases the total page size, so unlimited
scrollback can consume additional memory.

An explicit `GHOSTTY_SOURCE_DIR` remains authoritative and is never modified
by this build script. To benchmark a custom checkout with the same fix,
apply the complete ordered patch set to that checkout yourself. It must include
the image-number C helper required by the matching safe bindings. A pkg-config
library likewise needs those matching extensions and is not patched in place.

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

Additional native graphics filters pass 508 tests with six expected skips;
relative placement/storage filters pass 131 tests and the anchor filter passes
57 tests. Wisp additionally exercises graphics through its real PTY, snapshot,
GPU, and actual-window paths. Animation state and rendering live in Wisp rather
than adding incompatible fields to the native image C ABI.

The standard performance changes pass 59 focused OSC/stream tests and 63 DCS
tests, including every byte value at vector boundaries, input chunk splitting,
and lazy-allocation failure and recovery. Controlled native comparisons show
ASCII, Unicode, combining-cell, and CSI throughput within 2% or better than the
pre-standard baseline; discarded long OSC no longer slows the parser.

The printable-run changes pass 147 native stream tests. New differential cases
check all byte values around vector boundaries, empty and control-only runs,
and every split in mixed UTF-8/control input against scalar parsing. On the
portable baseline CPU target, paired native Unicode fixtures improve from
approximately 1074 to 1170 MiB/s without changing terminal features or CPU
compatibility. End-to-end measurements are recorded in Wisp's performance notes.
