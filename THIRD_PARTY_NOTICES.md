## darktable demosaicing

The Bayer RCD stages, Markesteijn X-Trans stages, and dual-demosaic mask behavior in this revision were ported with reference to darktable release 5.6.0:

- `src/iop/demosaicing/rcd.c`
- `src/iop/demosaicing/xtrans.c`
- `src/iop/demosaicing/dual.c`
- corresponding OpenCL kernels under `data/kernels/`

The RCD implementation credits Luis Sanz Rodríguez and the original RCD-Demosaicing project. The X-Trans implementation is based on Frank Markesteijn's algorithm as adapted through dcraw and darktable.

darktable and these source files are licensed GPL-3.0-or-later. Distribution of this port must remain GPL-compatible and retain the applicable copyright, authorship, and license notices.

# Third-party notices

These notices identify source-derived components and separately licensed runtime
assets. They do not replace the license texts supplied by the respective
projects.

## Phosphor Icons

Application interface icons come from
[Phosphor Icons](https://github.com/phosphor-icons/core), used through the
`egui-phosphor` Rust crate.

Copyright (c) 2020 Phosphor Icons.

Phosphor Icons is licensed under the MIT License. The `egui-phosphor` crate is
licensed under MIT OR Apache-2.0.

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

## Ansel highlight reconstruction

AuRaw's guided pre-demosaic highlight reconstruction is adapted from the
`highlights` module in [Ansel](https://github.com/aurelienpierreeng/ansel),
including its interpolate, clipping-mask propagation, chroma reconstruction,
and remosaic design. The corresponding AuRaw implementation is primarily in:

- `src/shaders/highlights.wgsl`
- `src/shaders/highlight_lch_pass.wgsl`
- `src/pipeline/basicadj.rs`

Copyright remains with the Ansel and darktable contributors for their original
work; the Rust/WGSL adaptation is copyright 2026 AuRaw contributors. Ansel is
distributed under GNU GPL version 3 terms, compatible with AuRaw's GPL-3.0
distribution. The adapted files retain attribution and SPDX identifiers.

## BiRefNet

Subject and Background selection uses the BiRefNet General Lite (Swin-Tiny) ONNX model by Peng Zheng et al. The model is downloaded only after explicit user consent from the `danielgatis/rembg` release hosting and is licensed under the MIT License.

Local inference uses Microsoft ONNX Runtime through the Rust `ort` bindings. ONNX Runtime is licensed under the MIT License; `ort` is dual-licensed under MIT or Apache-2.0.


## ViTMatte

Subject and Not Subject masks use ViTMatte Small (Composition-1k) as a second-stage alpha-matting refiner after BiRefNet. Object masks use the same refiner automatically after SAM 2.1 selection. AuRaw derives a conservative trimap from the coarse mask and applies ViTMatte only in the uncertain boundary band, preserving known foreground/background interiors.

AuRaw downloads the ONNX export from the `Xenova/vitmatte-small-composition-1k` Hugging Face repository only after the existing AI-model download consent. The exact file is pinned by size and SHA-256 before use. The upstream `hustvl/vitmatte-small-composition-1k` model is licensed under the Apache License 2.0.

## Segment Anything 2.1

Promptable Object selection uses the SAM 2.1 Hiera Tiny image encoder and mask decoder originally developed by Meta AI Research. AuRaw downloads ONNX exports from the `akiyamanx/sam2.1-hiera-tiny-onnx` Hugging Face repository only after explicit user consent. The files are verified against pinned SHA-256 digests before use. SAM 2.1 and the redistributed ONNX weights are licensed under the Apache License 2.0.

The object-mask implementation follows the public SAM 2.1 point-prompt interface used by AnyLabeling: normalized RGB encoder input, cached high-resolution/image embedding outputs, foreground/background point prompts, previous-mask logits, and candidate mask scores.

## LaMa ONNX

Local inpainting uses the `lama_fp32.onnx` model distributed by
[Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX), an ONNX port of the
original LaMa model. The model repository identifies the model as licensed
under the Apache License 2.0. AuRaw downloads it only after an explicit user
choice and verifies it against a pinned SHA-256 digest before use.

## RawNIND UtNet2

Optional AI RAW denoise uses the `rawdenoise-nind` package published by
[darktable-ai](https://github.com/darktable-org/darktable-ai). It contains the
RawNIND UtNet2 Bayer joint-denoise/demosaic graph and the linear Rec.2020 graph
used for X-Trans. AuRaw downloads the published darktable-ai 5.6 release asset
only after explicit consent and pins the archive and extracted ONNX files by
SHA-256. The model, its upstream implementation, and darktable-ai integration
are licensed under GPL-3.0; RawNIND training photographs are published under CC
BY 4.0 or CC0 as documented by the model card.

## Lensfun

Lens correction uses the Lensfun library and its camera/lens profile database.
The Lensfun libraries are licensed under the GNU Lesser General Public License,
version 3. Lensfun's bundled applications are licensed under GPL-3.0, and the
profile database is licensed under Creative Commons Attribution-ShareAlike 3.0.
Packaged AuRaw builds may redistribute the Lensfun shared or static library and database;
the corresponding license texts and source information remain available from
the Lensfun project.

## Android Lensfun dependencies

Android Lensfun builds include GLib (LGPL-2.1-or-later), PCRE2 (BSD-3-Clause),
libffi (MIT), and zlib (zlib license) as static dependencies of the native
AuRaw library. Their source releases and licenses remain available from their
respective upstream projects.
