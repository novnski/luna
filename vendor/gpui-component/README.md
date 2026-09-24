# Bundled GPUI base source

This directory contains the `gpui-base` 0.5.2 source used by Luna, from the
previously pinned component fork at revision `03cd9947a6aa07921cb284ad66c87fd6735e55b0`.
It is licensed under Apache-2.0; see `LICENSE-APACHE` and
`crates/base/LICENSE-APACHE`. The workspace manifest keeps only the crate and
dependencies needed by Luna and points GPUI imports to `../zui`.
