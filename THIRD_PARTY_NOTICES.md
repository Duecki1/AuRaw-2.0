## darktable demosaicing

The Bayer RCD stages, Markesteijn X-Trans stages, and dual-demosaic mask behavior in this revision were ported with reference to darktable release 5.6.0:

- `src/iop/demosaicing/rcd.c`
- `src/iop/demosaicing/xtrans.c`
- `src/iop/demosaicing/dual.c`
- corresponding OpenCL kernels under `data/kernels/`

The RCD implementation credits Luis Sanz Rodríguez and the original RCD-Demosaicing project. The X-Trans implementation is based on Frank Markesteijn's algorithm as adapted through dcraw and darktable.

darktable and these source files are licensed GPL-3.0-or-later. Distribution of this port must remain GPL-compatible and retain the applicable copyright, authorship, and license notices.

# Third-party notices

## darktable sigmoid

Portions of this project are adapted from darktable 5.6.0:

- `src/iop/sigmoid.c`
- `data/kernels/sigmoid.cl`

Copyright (C) 2020-2026 darktable developers.

The adapted Rust and WGSL code is located in:

- `src/pipeline/sigmoid.rs`
- `src/shaders/tonemap.wgsl`

The port includes the generalized log-logistic curve and coefficient calculation, negative-value desaturation, channel ordering, per-channel hue/energy preservation, and RGB-ratio hyperbolic gamut compression.

darktable and these adaptations are licensed under the GNU General Public License, version 3 or (at your option) any later version. AuRaw is distributed under compatible GPL-3.0 terms. Source-file headers retain attribution and license identifiers.
