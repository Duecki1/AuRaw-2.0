#include "highlights.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <float.h>
#include <assert.h>

#define RED 0
#define GREEN 1
#define BLUE 2
#define ALPHA 3

typedef float dt_aligned_pixel_t[4] __attribute__((aligned(16)));

// Unified Region of Interest structure matching darktable
typedef struct dt_iop_roi_t {
  int x;
  int y;
  int width;
  int height;
  float scale;
} dt_iop_roi_t;

// Standard Bayer CFA color lookup macro
#define FC(row, col, filters) \
  (filters >> ((((row) & 1) << 1) + ((col) & 1)) * 2 & 3)

// Standard X-Trans CFA color lookup macro
#define FCxtrans(row, col, roi, xtrans) \
  (xtrans[((row) + (roi)->y) % 6][((col) + (roi)->x) % 6])

#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define MAX(a, b) ((a) > (b) ? (a) : (b))
#define SQRT3 1.7320508075688772935274463415058723669f
#define SQRT12 3.4641016151377545870548926830117447339f

// --- ANSEL MATH ENGINE: INPAINT / COLOR RECONSTRUCTION (Mode 2) ---

static inline float interp_pix_xtrans(const int ratio_next, const ssize_t offset_next,
                                      const float clip0, const float clip_next,
                                      const float *const in, const float *const ratios) {
  const float clip_val = fmaxf(clip0, clip_next);
  if(in[offset_next] >= clip_next - 1e-5f) {
    return clip_val;
  } else {
    if (ratio_next > 0)
      return fminf(in[offset_next] / ratios[ratio_next], clip_val);
    else
      return fminf(in[offset_next] * ratios[-ratio_next], clip_val);
  }
}

static inline void interpolate_color_xtrans(const void *const ivoid, void *const ovoid,
                                            const dt_iop_roi_t *const roi_in,
                                            const dt_iop_roi_t *const roi_out,
                                            int dim, int dir, int other,
                                            const float *const clip,
                                            const unsigned char (*const xtrans)[6],
                                            const int pass) {
  const int roff[3][3] = {{ 0, -1, -2}, { 1,  0, -3}, { 2,  3,  0}};
  dt_aligned_pixel_t ratios = {1.0f, 1.0f, 1.0f, 1.0f};

  int i = (dim == 0) ? 0 : other;
  int j = (dim == 0) ? other : 0;
  const ssize_t offs = (ssize_t)(dim ? roi_out->width : 1) * ((dir < 0) ? -1 : 1);
  const ssize_t offl = offs - (dim ? 1 : roi_out->width);
  const ssize_t offr = offs + (dim ? 1 : roi_out->width);
  int beg, end;
  if(dir == 1) {
    beg = 0;
    end = (dim == 0) ? roi_out->width : roi_out->height;
  } else {
    beg = ((dim == 0) ? roi_out->width : roi_out->height) - 1;
    end = -1;
  }

  float *in, *out;
  if(dim == 1) {
    out = (float *)ovoid + (size_t)i + (size_t)beg * roi_out->width;
    in = (float *)ivoid + (size_t)i + (size_t)beg * roi_in->width;
  } else {
    out = (float *)ovoid + (size_t)beg + (size_t)j * roi_out->width;
    in = (float *)ivoid + (size_t)beg + (size_t)j * roi_in->width;
  }

  for(int k = beg; k != end; k += dir) {
    if(dim == 1) j = k; else i = k;

    const unsigned char f0 = FCxtrans(j, i, roi_in, xtrans);
    const unsigned char f1 = FCxtrans(dim ? (j + dir) : j, dim ? i : (i + dir), roi_in, xtrans);
    const unsigned char fl = FCxtrans(dim ? (j + dir) : (j - 1), dim ? (i - 1) : (i + dir), roi_in, xtrans);
    const unsigned char fr = FCxtrans(dim ? (j + dir) : (j + 1), dim ? (i + 1) : (i + dir), roi_in, xtrans);
    const float clip0 = clip[f0];
    const float clip1 = clip[f1];
    const float clipl = clip[fl];
    const float clipr = clip[fr];
    const float clip_max = fmaxf(fmaxf(clip[0], clip[1]), clip[2]);

    if(i == 0 || i == roi_out->width - 1 || j == 0 || j == roi_out->height - 1) {
      if(pass == 3) out[0] = fminf(clip_max, in[0]);
    } else {
      if ((f0 != f1) && (in[0] < clip0 && in[0] > 1e-5f) && (in[offs] < clip1 && in[offs] > 1e-5f)) {
        const int r = roff[f0][f1];
        if (r > 0) ratios[r] = (3.f * ratios[r] + (in[offs] / in[0])) / 4.f;
        else ratios[-r] = (3.f * ratios[-r] + (in[0] / in[offs])) / 4.f;
      }

      if(in[0] >= clip0 - 1e-5f) {
        float add;
        if(f0 != f1) add = interp_pix_xtrans(roff[f0][f1], offs, clip0, clip1, in, ratios);
        else add = (fl != f0) ? interp_pix_xtrans(roff[f0][fl], offl, clip0, clipl, in, ratios) : interp_pix_xtrans(roff[f0][fr], offr, clip0, clipr, in, ratios);

        if(pass == 0) out[0] = add;
        else if(pass == 3) out[0] = fminf(clip_max, (out[0] + add) / 4.0f);
        else out[0] += add;
      } else {
        if(pass == 3) out[0] = in[0];
      }
    }
    out += offs;
    in += offs;
  }
}

static inline void interpolate_color(const void *const ivoid, void *const ovoid,
                                     const dt_iop_roi_t *const roi_out, int dim, int dir, int other,
                                     const float *clip, const unsigned int filters, const int pass) {
  float ratio = 1.0f;
  float *in, *out;

  int i = 0, j = 0;
  if(dim == 0) j = other; else i = other;
  ssize_t offs = dim ? roi_out->width : 1;
  if(dir < 0) offs = -offs;
  int beg, end;
  if(dim == 0 && dir == 1) { beg = 0; end = roi_out->width; }
  else if(dim == 0 && dir == -1) { beg = roi_out->width - 1; end = -1; }
  else if(dim == 1 && dir == 1) { beg = 0; end = roi_out->height; }
  else if(dim == 1 && dir == -1) { beg = roi_out->height - 1; end = -1; }
  else return;

  if(dim == 1) {
    out = (float *)ovoid + i + (size_t)beg * roi_out->width;
    in = (float *)ivoid + i + (size_t)beg * roi_out->width;
  } else {
    out = (float *)ovoid + beg + (size_t)j * roi_out->width;
    in = (float *)ivoid + beg + (size_t)j * roi_out->width;
  }
  for(int k = beg; k != end; k += dir) {
    if(dim == 1) j = k; else i = k;
    const float clip0 = clip[FC(j, i, filters)];
    const float clip1 = clip[FC(dim ? (j + 1) : j, dim ? i : (i + 1), filters)];
    if(i == 0 || i == roi_out->width - 1 || j == 0 || j == roi_out->height - 1) {
      if(pass == 3) out[0] = in[0];
    } else {
      if(in[0] < clip0 && in[0] > 1e-5f) {
        if(in[offs] < clip1 && in[offs] > 1e-5f) {
          if(k & 1) ratio = (3.0f * ratio + in[0] / in[offs]) / 4.0f;
          else ratio = (3.0f * ratio + in[offs] / in[0]) / 4.0f;
        }
      }

      if(in[0] >= clip0 - 1e-5f) {
        float add = 0.0f;
        if(in[offs] >= clip1 - 1e-5f) add = fmaxf(clip0, clip1);
        else if(k & 1) add = in[offs] * ratio;
        else add = in[offs] / ratio;

        if(pass == 0) out[0] = add;
        else if(pass == 3) out[0] = (out[0] + add) / 4.0f;
        else out[0] += add;
      } else {
        if(pass == 3) out[0] = in[0];
      }
    }
    out += offs;
    in += offs;
  }
}

// --- ANSEL MATH ENGINE: RECONSTRUCT IN LCH (Mode 1) ---

static void process_lch_bayer(const void *const ivoid, void *const ovoid,
                              const dt_iop_roi_t *const roi_out, const float clip, unsigned int filters) {
  #pragma omp parallel for collapse(2)
  for(int j = 0; j < roi_out->height; j++) {
    for(int i = 0; i < roi_out->width; i++) {
      float *const out = (float *)ovoid + (size_t)roi_out->width * j + i;
      const float *const in = (float *)ivoid + (size_t)roi_out->width * j + i;

      if(i == roi_out->width - 1 || j == roi_out->height - 1) {
        out[0] = MIN(clip, in[0]);
      } else {
        int clipped = 0;
        float R = 0.0f, Gmin = FLT_MAX, Gmax = -FLT_MAX, B = 0.0f;
        for(int jj = 0; jj <= 1; jj++) {
          for(int ii = 0; ii <= 1; ii++) {
            const float val = in[(size_t)jj * roi_out->width + ii];
            clipped = (clipped || (val > clip));
            const int c = FC(j + jj + roi_out->y, i + ii + roi_out->x, filters);
            switch(c) {
              case 0: R = val; break;
              case 1: Gmin = MIN(Gmin, val); Gmax = MAX(Gmax, val); break;
              case 2: B = val; break;
            }
          }
        }

        if(clipped) {
          const float Ro = MIN(R, clip);
          const float Go = MIN(Gmin, clip);
          const float Bo = MIN(B, clip);
          const float L = (R + Gmax + B) / 3.0f;
          float C = SQRT3 * (R - Gmax);
          float H = 2.0f * B - Gmax - R;
          const float Co = SQRT3 * (Ro - Go);
          const float Ho = 2.0f * Bo - Go - Ro;

          if(R != Gmax && Gmax != B) {
            const float ratio = sqrtf((Co * Co + Ho * Ho) / (C * C + H * H));
            C *= ratio;
            H *= ratio;
          }

          dt_aligned_pixel_t RGB = { 0.0f, 0.0f, 0.0f };
          RGB[0] = L - H / 6.0f + C / SQRT12;
          RGB[1] = L - H / 6.0f - C / SQRT12;
          RGB[2] = L + H / 3.0f;
          out[0] = RGB[FC(j + roi_out->y, i + roi_out->x, filters)];
        } else {
          out[0] = in[0];
        }
      }
    }
  }
}

static void process_lch_xtrans(const void *const ivoid, void *const ovoid,
                               const dt_iop_roi_t *const roi_in, const dt_iop_roi_t *const roi_out,
                               const float clip, const unsigned char (*const xtrans)[6]) {
  #pragma omp parallel for
  for(int j = 0; j < roi_out->height; j++) {
    float *out = (float *)ovoid + (size_t)roi_out->width * j;
    float *in = (float *)ivoid + (size_t)roi_in->width * j;
    int cl = 0;

    for(int i = 0; i < roi_out->width; i++) {
      cl = (cl << 1) & 6;
      if(j >= 2 && j <= roi_out->height - 3) {
        cl |= (in[-roi_in->width] > clip) | (in[0] > clip) | (in[roi_in->width] > clip);
      }

      if(i < 2 || i > roi_out->width - 3 || j < 2 || j > roi_out->height - 3) {
        out[0] = MIN(clip, in[0]);
      } else {
        int clipped = (in[0] > clip);
        if(!clipped) {
          clipped = cl;
          if(clipped) {
            for(int offset_j = -2; offset_j <= 0; offset_j++) {
              for(int offset_i = -2; offset_i <= 0; offset_i++) {
                if(clipped) {
                  clipped = 0;
                  for(int jj = offset_j; jj <= offset_j + 2; jj++) {
                    for(int ii = offset_i; ii <= offset_i + 2; ii++) {
                      const float val = in[(ssize_t)jj * roi_in->width + ii];
                      clipped = (clipped || (val > clip));
                    }
                  }
                }
              }
            }
          }
        }

        if(clipped) {
          dt_aligned_pixel_t mean = { 0.0f, 0.0f, 0.0f };
          dt_aligned_pixel_t RGBmax = { -FLT_MAX, -FLT_MAX, -FLT_MAX };
          int cnt[3] = { 0, 0, 0 };

          for(int jj = -1; jj <= 1; jj++) {
            for(int ii = -1; ii <= 1; ii++) {
              const float val = in[(ssize_t)jj * roi_in->width + ii];
              const int c = FCxtrans(j+jj, i+ii, roi_in, xtrans);
              mean[c] += val;
              cnt[c]++;
              RGBmax[c] = MAX(RGBmax[c], val);
            }
          }

          const float Ro = MIN(mean[0]/cnt[0], clip);
          const float Go = MIN(mean[1]/cnt[1], clip);
          const float Bo = MIN(mean[2]/cnt[2], clip);
          const float R = RGBmax[0];
          const float G = RGBmax[1];
          const float B = RGBmax[2];
          const float L = (R + G + B) / 3.0f;
          float C = SQRT3 * (R - G);
          float H = 2.0f * B - G - R;
          const float Co = SQRT3 * (Ro - Go);
          const float Ho = 2.0f * Bo - Go - Ro;

          if(R != G && G != B) {
            const float ratio = sqrtf((Co * Co + Ho * Ho) / (C * C + H * H));
            C *= ratio;
            H *= ratio;
          }

          dt_aligned_pixel_t RGB = { 0.0f, 0.0f, 0.0f };
          RGB[0] = L - H / 6.0f + C / SQRT12;
          RGB[1] = L - H / 6.0f - C / SQRT12;
          RGB[2] = L + H / 3.0f;
          out[0] = RGB[FCxtrans(j, i, roi_out, xtrans)];
        } else {
          out[0] = in[0];
        }
      }
      out++;
      in++;
    }
  }
}

// --- ANSEL MATH ENGINE: CLIP HIGHLIGHTS (Mode 0) ---

static void process_clip(const void *const ivoid, void *const ovoid,
                         const dt_iop_roi_t *const roi_out, const float clip, int filters) {
  const float *const in = (const float *const)ivoid;
  float *const out = (float *const)ovoid;

  if(filters) { // raw bayer/xtrans mosaic
    #pragma omp parallel for simd
    for(size_t k = 0; k < (size_t)roi_out->width * roi_out->height; k++) {
      out[k] = MIN(clip, in[k]);
    }
  } else { // non-raw image channels
    #pragma omp parallel for simd
    for(size_t k = 0; k < (size_t)3 * roi_out->width * roi_out->height; k++) {
      out[k] = MIN(clip, in[k]);
    }
  }
}

// --- THE SURGICAL MASTER ENTRY POINT ---

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
) {
  // Construct Local Region of Interest Structs expected by original Ansel functions
  dt_iop_roi_t roi_in = { .x = roi_x, .y = roi_y, .width = width, .height = height, .scale = 1.0f };
  dt_iop_roi_t roi_out = roi_in;

  // Calculate the global clipping ceiling
  const float clip = clip_threshold * fminf(processed_maximum[0], fminf(processed_maximum[1], processed_maximum[2]));

  if(!filters) {
    process_clip(in, out, &roi_out, clip, 0);
    return;
  }

  switch(mode) {
    case AURAW_HIGHLIGHTS_INPAINT: {
      const float clips[4] = { 0.987f * clip_threshold * processed_maximum[0],
                               0.987f * clip_threshold * processed_maximum[1],
                               0.987f * clip_threshold * processed_maximum[2], clip };

      if(filters == 9u) {
        #pragma omp parallel for
        for(int j = 0; j < height; j++) {
          interpolate_color_xtrans(in, out, &roi_in, &roi_out, 0, 1, j, clips, xtrans, 0);
          interpolate_color_xtrans(in, out, &roi_in, &roi_out, 0, -1, j, clips, xtrans, 1);
        }
        #pragma omp parallel for
        for(int i = 0; i < width; i++) {
          interpolate_color_xtrans(in, out, &roi_in, &roi_out, 1, 1, i, clips, xtrans, 2);
          interpolate_color_xtrans(in, out, &roi_in, &roi_out, 1, -1, i, clips, xtrans, 3);
        }
      } else {
        #pragma omp parallel for
        for(int j = 0; j < height; j++) {
          interpolate_color(in, out, &roi_out, 0, 1, j, clips, filters, 0);
          interpolate_color(in, out, &roi_out, 0, -1, j, clips, filters, 1);
        }
        #pragma omp parallel for
        for(int i = 0; i < width; i++) {
          interpolate_color(in, out, &roi_out, 1, 1, i, clips, filters, 2);
          interpolate_color(in, out, &roi_out, 1, -1, i, clips, filters, 3);
        }
      }
      break;
    }
    case AURAW_HIGHLIGHTS_LCH:
      if(filters == 9u) {
        process_lch_xtrans(in, out, &roi_in, &roi_out, clip, xtrans);
      } else {
        process_lch_bayer(in, out, &roi_out, clip, filters);
      }
      break;
    default:
    case AURAW_HIGHLIGHTS_CLIP:
      process_clip(in, out, &roi_out, clip, filters);
      break;
  }
}