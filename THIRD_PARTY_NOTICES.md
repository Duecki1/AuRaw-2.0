# Third-party notices

## Ansel highlight reconstruction

`src/shaders/highlights.wgsl` ports the fast Bayer LCh highlight-reconstruction
method from Ansel (`src/iop/highlights.c` and `data/kernels/basic.cl`, inspected
from the sibling `ansel` checkout).

Ansel is licensed GPL-3.0-or-later. Any distribution of this direct algorithmic
port must remain compatible with that license and retain the appropriate Ansel
copyright and license notices.

## darktable demosaicing

The Bayer RCD stages in `src/shaders/pass1.wgsl` through `pass4.wgsl`, the
Markesteijn X-Trans stages in `src/shaders/xtrans_*.wgsl`, and the dual-demosaic
mask behavior were ported with reference to darktable release 5.6.0:

- `src/iop/demosaicing/rcd.c`
- `src/iop/demosaicing/xtrans.c`
- `src/iop/demosaicing/dual.c`
- corresponding OpenCL kernels under `data/kernels/`

The RCD implementation credits Luis Sanz Rodríguez and the original
RCD-Demosaicing project. The X-Trans implementation is based on Frank
Markesteijn's algorithm as adapted through dcraw and darktable.

darktable and these source files are licensed GPL-3.0-or-later. Distribution of
this port must remain GPL-compatible and retain the applicable copyright,
authorship, and license notices.
