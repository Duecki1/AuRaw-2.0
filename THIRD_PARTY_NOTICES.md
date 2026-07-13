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

## BiRefNet

Subject and Background selection uses the BiRefNet General Lite (Swin-Tiny) ONNX model by Peng Zheng et al. The model is downloaded only after explicit user consent from the `danielgatis/rembg` release hosting and is licensed under the MIT License.

Local inference uses Microsoft ONNX Runtime through the Rust `ort` bindings. ONNX Runtime is licensed under the MIT License; `ort` is dual-licensed under MIT or Apache-2.0.
