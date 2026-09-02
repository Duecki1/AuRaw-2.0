# Third-party notices

CalibRaw is GPL-3.0-or-later software, but it uses adapted source, data, native
libraries, and optional downloaded AI models from other projects. Those works
retain their own copyright and license terms. A name below identifies
provenance; it does not imply sponsorship or endorsement of CalibRaw.

## Adapted source and algorithms

### darktable 5.6.0

The following CalibRaw implementations were adapted from or validated directly
against darktable release 5.6.0:

- Bayer RCD demosaicing in `crates/calibraw-gpu/src/shaders/pass1.wgsl` through
  `pass4.wgsl`, based on `src/iop/demosaicing/rcd.c` and its OpenCL kernels.
- Markesteijn X-Trans demosaicing and dual-demosaic behavior in
  `crates/calibraw-gpu/src/shaders/xtrans/`, `xtrans_demosaic.wgsl`, and
  `xtrans_finish.wgsl`, based on
  `src/iop/demosaicing/xtrans.c`, `dual.c`, and their OpenCL kernels.
- LCh and inpaint-opposed highlight reconstruction in
  `crates/calibraw-gpu/src/shaders/highlights.wgsl` and
  `crates/calibraw-core/src/pipeline/raw_loader.rs`, based primarily on
  `src/iop/highlights.c`, `src/iop/hlreconstruct/opposed.c`,
  `src/iop/hlreconstruct/segbased.c`, and `data/kernels/basic.cl`.
- The sigmoid display transform in `crates/calibraw-core/src/pipeline/sigmoid.rs`
  and `crates/calibraw-gpu/src/shaders/tonemap.wgsl`, based on
  `src/iop/sigmoid.c` and `data/kernels/sigmoid.cl`.

Copyright (C) 2010-2026 darktable developers and the authors named in the
upstream files. darktable is GPL-3.0-or-later. The referenced source is in the
[darktable 5.6.0 tree](https://github.com/darktable-org/darktable/tree/release-5.6.0).

RCD credits Luis Sanz Rodríguez, Ingo Weyrich, Hanno Schwalm, and the original
RCD-Demosaicing project. Dual demosaicing credits Ingo Weyrich and Hanno
Schwalm.
Markesteijn demosaicing credits Frank Markesteijn, as adapted through dcraw and
darktable. Their applicable notices are retained through the darktable notice.

### Ansel and dcraw

The XYZ-to-camera normalization and related temperature/tint conventions in
`crates/calibraw-core/src/pipeline/raw_loader/libraw_loader.rs` follow darktable
and the related Ansel/dcraw implementations. CalibRaw's former Ansel-derived
highlight path was subsequently replaced by the darktable 5.6.0 implementation
documented above.

Ansel and the relevant adapted source are GPL-3.0-or-later. The implementation
was reviewed at Ansel revision
[`17343ac785c067be88b33ba141c5a80bdcaab1b6`](https://github.com/aurelienpierreeng/ansel/tree/17343ac785c067be88b33ba141c5a80bdcaab1b6).

### GIMP 3.0.4

The clone/heal behavior and checkerboard Gauss-Seidel/SOR perceptual Laplace
solver in `crates/calibraw-ai/src/remove.rs` are adapted from
`app/paint/gimpheal.c` in GIMP 3.0.4.

Copyright GIMP contributors. GIMP 3.0.4 and this adaptation are
GPL-3.0-or-later. See the pinned
[`gimpheal.c`](https://gitlab.gnome.org/GNOME/gimp/-/blob/GIMP_3_0_4/app/paint/gimpheal.c)
and [GIMP license](https://www.gimp.org/docs/userfaq.html#legal).

### RapidRAW design reference

RapidRAW influenced parts of CalibRaw's interface and workflow. No RapidRAW source
code is included or adapted. RapidRAW itself is AGPL-3.0.

## Bundled data

`data/wb_presets.json` is a compact snapshot derived from darktable's white
balance preset database. It is covered by the darktable GPL-3.0-or-later notice
above. The exact CalibRaw snapshot has SHA-256
`70abcc507668b9a3588a650902666369e27bfc3261dec9e04391b45f172624e7`.

Release packages include the Lensfun profile database. Lensfun licenses its
database under
[CC BY-SA 3.0](https://github.com/lensfun/lensfun/blob/v0.3.4/data/COPYING.CC_BY-SA_3.0).
The database snapshot supplied by a desktop package manager can differ from the
Lensfun library version.

## Optional AI models

AI model files are not committed to this repository. CalibRaw downloads them only
after user consent, stores them in its model cache, and verifies their size and
SHA-256 digest before use. Model copyright and license terms remain separate
from CalibRaw's GPL license; users must comply with the model terms for their use.

| Feature | Model and immutable source | License |
| --- | --- | --- |
| Remove | [Carve/LaMa-ONNX Big-LaMa](https://huggingface.co/Carve/LaMa-ONNX/tree/a3ee2fca54baebec351b8fa7786154ffa7555aa6), an ONNX port of the original LaMa model | Apache-2.0 |
| Subject selection | [BiRefNet v1 ONNX checkpoints](https://github.com/ZhengPeng7/BiRefNet/releases/tag/v1) | MIT |
| Object selection | [SAM 2.1 Hiera Tiny ONNX encoder and decoder](https://huggingface.co/akiyamanx/sam2.1-hiera-tiny-onnx/tree/aa11669045f8d82c74e46f8f77c9b56792c90ebb), based on Meta SAM 2 | Apache-2.0 |
| RAW denoise | [darktable-ai RawNIND release 5.6.0](https://github.com/darktable-org/darktable-ai/tree/release-5.6.0/models/rawdenoise-nind) | GPL-3.0 |

## Native libraries and release packages

Depending on the platform, release artifacts link or bundle the following
native components:

- [LibRaw 0.22.1](https://github.com/LibRaw/LibRaw/tree/0.22.1), Copyright
  LibRaw LLC and other named contributors. LibRaw is dual-licensed under
  LGPL-2.1 and CDDL-1.0; CalibRaw uses the LGPL-2.1 option.
- [Lensfun 0.3.4](https://github.com/lensfun/lensfun/tree/v0.3.4). The library
  is LGPL-3.0 and its separately packaged profile database is CC BY-SA 3.0.
- [ONNX Runtime](https://github.com/microsoft/onnxruntime), under the MIT
  license. Desktop Automatic mode downloads a pinned platform archive from
  [CalibRaw Artifacts](https://huggingface.co/Duecki/CalibRaw-Artifacts); Manual
  mode uses the runtime selected by the user. Android builds obtain the pinned
  runtime selected by `ort-sys` and package its shared library.
- Android native builds also statically link pinned GLib (LGPL-2.1-or-later),
  GNU libiconv (LGPL-2.1), PCRE2 (BSD-3-Clause), libffi (MIT), and zlib
  (Zlib). Their versions and source hashes are pinned in
  `android/app/src/main/cpp/CMakeLists.txt` or its resolved GLib subprojects.
- The committed Gradle wrapper is part of Gradle, licensed under Apache-2.0.

Windows and macOS artifacts may bundle additional runtime libraries discovered
from the build environment. `BUILD-INFO.txt` and `SHA256SUMS.txt` in each
artifact identify the build and bundled binaries. Those libraries retain their
upstream licenses.

## Rust dependencies

Resolved Rust crate names, versions, and checksums are recorded in
`Cargo.lock`. Each crate retains the license declared by its package and source
repository. CalibRaw does not relicense those dependencies. This includes the
bundled egui default fonts (Hack, Noto Emoji, Ubuntu Light, and emoji-icon-font)
and the Phosphor icon font; their MIT, OFL-1.1, Ubuntu Font Licence 1.0, and
other applicable terms are reproduced with the crate notices.

Complete resolved Rust dependency license texts and package attribution are in
[`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md). Regenerate that file from
`Cargo.lock` with the command documented at its top.

## License policy

Rust dependency licenses are checked in CI against the explicit allow-list in
`deny.toml`. A new proprietary, noncommercial, research-only, or otherwise
unreviewed Rust dependency license fails that check. Native components,
adapted source, bundled data, and optional model licenses require separate
manual review and are recorded above.

The full GPL-3.0 text governing CalibRaw and compatible adaptations is in
[`COPYING`](COPYING).
