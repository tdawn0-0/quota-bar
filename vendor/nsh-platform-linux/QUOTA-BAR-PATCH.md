# Local text-rendering fix

This is the published `nsh-platform-linux` 0.5.0 source, from NSH commit
`f4d68f7f9f31eb7b3ecc8a4cbc31096d5c6238a1` (Apache-2.0).

The source change is in `src/text.rs`, `main_font`: use `HintStyle::None`
instead of `HintStyle::Slight`, matching the engine-thread font context.
With Noto Sans CJK at 11 px, the old drawing context moved the top stroke of
“周” beyond its unhinted GSK ink bounds. The Vulkan glyph texture and Cairo
rounded-clip traversal lost that stroke even with generous text line height.

Keep the panel's clipping and default renderer unchanged. Remove this Cargo
patch when an upstream release includes the equivalent rendering fix.

The ignored `text::tests::gpu_preserves_cjk_top_stroke` regression checks
actual GPU pixels along the stroke that disappeared in the reported panel.
It needs a display, Vulkan and Noto Sans CJK:

```sh
cargo test --release -p nsh-platform-linux --lib \
  gpu_preserves_cjk_top_stroke -- --ignored
```

The upstream font-registration test fixture and its OFL license are included
in `test-data/`; its include path is adjusted because the published crate
omits the surrounding gallery directory.
