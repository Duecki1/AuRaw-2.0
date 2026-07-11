# Third-party notices

`src/shaders/highlights.wgsl` ports the fast Bayer LCh highlight-reconstruction
method from Ansel (`src/iop/highlights.c` and `data/kernels/basic.cl`, inspected
from the sibling `ansel` checkout).

Ansel is licensed GPL-3.0-or-later. Any distribution of this direct algorithmic
port must remain compatible with that license and retain the appropriate Ansel
copyright and license notices.
