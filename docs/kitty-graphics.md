# Kitty graphics

Wisp implements the actions in the [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/),
including the newer indexed-color and feature-query extensions. Ghostty retains
terminal state, normal image storage, placements, scrolling, and image IDs.
Wisp adds frame composition/playback, indexed palette updates, Unicode image
fragments, and native placement/lifecycle corrections.

| Area | Behavior |
| --- | --- |
| Transmit/query | RGB, RGBA, PNG, indexed pixels (`f=8/16`), zlib, chunked data |
| Local transport | Regular files, temporary files, POSIX shared memory, offsets and lengths |
| Placement | Multiple placements, crop, pixel offsets, cell sizing, z layers, cursor policy |
| Unicode | `U+10EEEE`, image/placement colors, row/column/high-ID diacritics, omitted-value inheritance |
| Relative placement | Normal or virtual parents, signed offsets, parent movement, cascading deletion |
| Frames | New frames, patches, base-frame dependencies, background canvases, alpha blend/overwrite |
| Composition | Source/destination rectangles, bounds checks, same-frame overlap rejection |
| Playback | Client selection, stopped/loading/running states, finite/infinite loops, gapless frames |
| Deletion | Placement filters, ID ranges, frame deletion, root promotion, hard/soft deletion |
| Responses | IDs/image numbers, frame acknowledgements, quiet settings, feature queries |

Queries using `a=q,e=<action>` report supported non-default keys and do not
interrupt a chunked upload. The optional transient usage hint is accepted;
Wisp may use its normal bounded caching policy instead of acting on that hint.
Indexed images follow palette/default color changes; animations and composition
are rejected for indexed images, as specified by the protocol.

## Animation

Start with a normal image using `a=t` or `a=T`. Add frames with `a=f` and
select a frame or start playback with `a=a`. Frame numbers are one-based;
the initial image is frame 1. Use `a=c` to composite regions between frames
and `a=d,d=f/F` to delete frames. `kitten icat animation.gif` also works.

The VT thread owns animation state and sleeps until the next frame deadline.
Playback continues without additional PTY output, including after `icat`
returns. Loading mode waits for appended frames rather than repeatedly
repainting the last frame. Stopped/completed animations have no playback
deadline, and images without visible placements do not schedule animation
redraws. Synchronized output still suppresses intermediate snapshots while
parsing and frame state changes continue.

Frame buffers are shared across snapshots. Stable generations and a bounded
GPU texture cache allow an animation loop to reuse uploaded textures. Edits
invalidate changed frames and their dependents. Deleting a base frame preserves
the pixels of its dependent frames.

## Layout and lifecycle

Unicode placeholders remain ordinary terminal cells: scrolling, wrapping,
selection, erasure, and alternate screens work through Ghostty's grid. Their
placeholder glyphs are suppressed while backgrounds and selection are retained.
Relative placements support chains up to 32 parents and report `ENOPARENT`,
`ECYCLE`, or `ETOODEEP` when appropriate.

Source rectangles are clipped to image bounds. Cell offsets are clamped to the
cell. When both `c` and `r` specify a box, Wisp preserves aspect ratio and centers
the image with letterbox/pillarbox padding, following the current protocol
documentation. Kitty versions whose normal placements stretch the image can
therefore render that particular case differently.

Successful retransmission of an ID removes its old placements. Failed uploads
preserve the previous image. A delete cancels incomplete uploads. Query-only
transmissions validate the data without replacing or storing an image. Local
file failures return the same `EBADF` response; special files are checked before
opening them. Temporary files and shared memory objects are removed according
to the transport rules.

## Resource limits

- Normal image storage: 64 MiB per screen.
- Animation frame storage: 320 MiB total across images, counting expanded
  canvases and shared allocations once; at most 65,536 frames per image.
- Decoded individual images/frames: 64 MiB.
- GPU cache: 64 MiB and 1,024 cached textures, with currently drawn textures
  retained even when the current frame exceeds the cache budget.

The limits bound resource use; invalid data or exhausted storage produces an
error response instead of partially displaying an upload. Frame storage is in
memory; Wisp does not spill animation frames to disk. Rendering still requires
the image dimensions to fit the GPU adapter's texture limits.

## Validation

```sh
cargo test --all-targets
cargo build --release
python3 tests/gui_graphics.py
python3 tests/gui_smoke.py
```

`tests/kitty_protocol.rs` uses a real PTY and production Session to check replies,
pixels, transport, playback after the client exits, lifecycle, and malformed
commands. It invokes installed `kitten icat` for a real animated GIF.
`tests/kitty_placeholders.rs` covers production placeholder snapshots. Unit
tests cover frame timing, composition, dependencies, quotas, stream boundaries,
and GPU upload reuse/eviction.

The Hyprland GUI test captures changing red/green/blue animation frames, a
Unicode image, and an image positioned relative to a virtual parent. It requires
`grim`, Python Pillow, and the same compositor helpers as `gui_smoke.py`.
Screenshots and results are written to `target/gui-graphics/`.

The graphics tests run alongside the broader VT/system suites described in
[terminal conformance](conformance.md). The general GUI smoke now controls only
its own windows' compositor properties, uses actual PTY geometry, and verifies
the image against a truecolor reference. It passes without skipped checks,
including exactly 3,200 static `icat` image pixels, hyperlinks, mouse input,
copy/paste, and resizing. The dedicated graphics GUI test checks real animation,
Unicode placeholder pixels, and relative images.

Performance checks preserve a direct path for ordinary VT/CSI output. In the
same 171×44 GUI, a paired 100-repetition randomized CSI run measured 158.6 MiB/s
before and 154.1 MiB/s afterward (about 2.8% lower). Reports are in
`target/gui-benchmark/graphics-{baseline-repeat,simd}-csi-100.json`. Release
key-to-snapshot P99 is 0.11–0.14 ms in 100-key samples at 20/35/50 ms intervals;
this excludes physical display latency.

The native patch set and safe Rust binding extensions are recorded in
[`vendor/libghostty-vt-sys/WISP.md`](../vendor/libghostty-vt-sys/WISP.md) and
[`vendor/libghostty-vt/WISP.md`](../vendor/libghostty-vt/WISP.md).
