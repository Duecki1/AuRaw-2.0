#ifndef AURAW_HIGHLIGHTS_H
#define AURAW_HIGHLIGHTS_H

#include <sys/types.h> // ssize_t -- highlights.c uses it; not pulled in
                        // automatically under strict -std=c11/c17 on some
                        // toolchains (MSVC has no sys/types.h at all; if you
                        // ever need to build this on Windows with MSVC,
                        // typedef ssize_t to SSIZE_T from <BaseTsd.h> instead).

#ifdef __cplusplus
extern "C" {
#endif

// Mirrors darktable/ansel's dt_iop_highlights_mode_t, restricted to the
// three modes actually ported in highlights.c.
enum auraw_highlights_mode {
  AURAW_HIGHLIGHTS_CLIP = 0,    // naive per-channel clip
  AURAW_HIGHLIGHTS_LCH = 1,     // reconstruct in LCH space (fast, single-pass)
  AURAW_HIGHLIGHTS_INPAINT = 2, // color reconstruction / inpainting (slower, best quality)
};

// Runs highlight clipping/reconstruction on a still-mosaiced (single sample
// per pixel) raw buffer, in raw sensor units -- i.e. BEFORE white balance,
// BEFORE the camera->sRGB color matrix, and before any demosaic that mixes
// channels together. This must run first because it needs to see each
// channel's raw clip point independently; once channels are combined via
// WB/color-matrix, there's no way to tell "genuinely bright" apart from
// "one raw channel clipped early."
//
// in / out: width*height single-channel float buffers (may alias for CLIP
//   mode but reconstruction modes assume distinct buffers sized identically)
// filters: LibRaw's packed Bayer `filters` value, or 9 to select X-Trans
// xtrans: 6x6 X-Trans CFA pattern (ignored when filters != 9)
// mode: one of enum auraw_highlights_mode
// clip_threshold: fraction of full-scale to treat as the clip point
//   (darktable/ansel default is 1.0; can be set slightly below 1.0 to be
//   conservative about sensor non-linearity near saturation)
// processed_maximum: per-channel white level in raw sensor units (R, G, B,
//   G2), i.e. LibRaw's `maximum` scaled per-channel if per-channel
//   white/black levels are in use.
void auraw_process_highlights(
    const float *const in,
    float *const out,
    int width,
    int height,
    int roi_x,
    int roi_y,
    int filters,
    const unsigned char xtrans[6][6],
    int mode,
    float clip_threshold,
    const float processed_maximum[4]
);

#ifdef __cplusplus
}
#endif

#endif // AURAW_HIGHLIGHTS_H