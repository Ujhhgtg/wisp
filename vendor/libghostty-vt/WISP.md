# Wisp binding extensions

This is `libghostty-vt` 0.2.2 from crates.io, published from
`uzaaft/libghostty-rs` commit `c9876b2902d5bb3e1f3d61b6395059f1e5e7b721`.
The upstream license is preserved in `LICENSE`.

Wisp adds two safe read-only methods:

- `Terminal::pixel_size()` reports content pixel dimensions through the existing
  C API, for image and placeholder geometry.
- `Graphics::image_by_number()` borrows the newest native image with a client
  image number. Its lifetime matches existing `Graphics::image()` handles.

The latter calls the small `wisp_kitty_graphics_image_by_number` extension in
the matching native patch set. No Rust object layout is cast to a native handle.
All other bindings and behavior remain upstream's implementation.
