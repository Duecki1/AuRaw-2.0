// SPDX-License-Identifier: GPL-3.0-or-later
/*
 * darktable sigmoid coefficient calculation, ported from
 * darktable 5.6.0 src/iop/sigmoid.c.
 *
 * Copyright (C) 2020-2026 darktable developers.
 * Copyright (C) 2026 AuRaw contributors (Rust port).
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 */

pub const MIDDLE_GREY: f32 = 0.1845;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SigmoidColorProcessing {
    #[default]
    PerChannel,
    RgbRatio,
}

impl SigmoidColorProcessing {
    pub(crate) const fn shader_value(self) -> f32 {
        match self {
            Self::PerChannel => 0.0,
            Self::RgbRatio => 1.0,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::PerChannel => "Per channel",
            Self::RgbRatio => "RGB ratio",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SigmoidParams {
    pub contrast: f32,
    pub skew: f32,
    /// Percent of reference white, matching darktable's UI domain.
    pub display_white_target: f32,
    /// Percent of reference white, matching darktable's UI domain.
    pub display_black_target: f32,
    pub color_processing: SigmoidColorProcessing,
    /// Percent, matching darktable's UI domain.
    pub hue_preservation: f32,
}

impl Default for SigmoidParams {
    fn default() -> Self {
        Self {
            contrast: 1.5,
            skew: 0.0,
            display_white_target: 100.0,
            display_black_target: 0.0152,
            color_processing: SigmoidColorProcessing::PerChannel,
            hue_preservation: 100.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SigmoidCoefficients {
    pub white_target: f32,
    pub black_target: f32,
    pub paper_exposure: f32,
    pub film_fog: f32,
    pub film_power: f32,
    pub paper_power: f32,
    pub hue_preservation: f32,
    pub color_processing: f32,
}

fn generalized_loglogistic_sigmoid(
    value: f32,
    magnitude: f32,
    paper_exposure: f32,
    film_fog: f32,
    film_power: f32,
    paper_power: f32,
) -> f32 {
    let clamped_value = value.max(0.0);
    let film_response = (film_fog + clamped_value).powf(film_power);
    let paper_response =
        magnitude * (film_response / (paper_exposure + film_response)).powf(paper_power);
    if paper_response.is_nan() {
        magnitude
    } else {
        paper_response
    }
}

/// Exact coefficient construction used by darktable 5.6.0's `commit_params`.
pub(crate) fn coefficients(params: SigmoidParams) -> SigmoidCoefficients {
    let contrast = params.contrast.clamp(0.1, 10.0);
    let skew = params.skew.clamp(-1.0, 1.0);
    let display_white_target = params.display_white_target.clamp(20.0, 1600.0);
    let display_black_target = params.display_black_target.clamp(0.0, 15.0);

    let ref_film_power = contrast;
    let ref_paper_power = 1.0;
    let ref_magnitude = 1.0;
    let ref_film_fog = 0.0;
    let ref_paper_exposure =
        (ref_film_fog + MIDDLE_GREY).powf(ref_film_power) * ((ref_magnitude / MIDDLE_GREY) - 1.0);
    let delta = 1e-6;
    let ref_slope = (generalized_loglogistic_sigmoid(
        MIDDLE_GREY + delta,
        ref_magnitude,
        ref_paper_exposure,
        ref_film_fog,
        ref_film_power,
        ref_paper_power,
    ) - generalized_loglogistic_sigmoid(
        MIDDLE_GREY - delta,
        ref_magnitude,
        ref_paper_exposure,
        ref_film_fog,
        ref_film_power,
        ref_paper_power,
    )) / (2.0 * delta);

    let paper_power = 5.0f32.powf(-skew);
    let temp_film_power = 1.0;
    let temp_white_target = 0.01 * display_white_target;
    let temp_white_grey_relation = (temp_white_target / MIDDLE_GREY).powf(1.0 / paper_power) - 1.0;
    let temp_paper_exposure = MIDDLE_GREY.powf(temp_film_power) * temp_white_grey_relation;
    let temp_slope = (generalized_loglogistic_sigmoid(
        MIDDLE_GREY + delta,
        temp_white_target,
        temp_paper_exposure,
        ref_film_fog,
        temp_film_power,
        paper_power,
    ) - generalized_loglogistic_sigmoid(
        MIDDLE_GREY - delta,
        temp_white_target,
        temp_paper_exposure,
        ref_film_fog,
        temp_film_power,
        paper_power,
    )) / (2.0 * delta);

    let film_power = ref_slope / temp_slope;
    let white_target = 0.01 * display_white_target;
    let black_target = 0.01 * display_black_target;
    let white_grey_relation = (white_target / MIDDLE_GREY).powf(1.0 / paper_power) - 1.0;
    let white_black_relation = if black_target == 0.0 {
        f32::INFINITY
    } else {
        (black_target / white_target).powf(-1.0 / paper_power) - 1.0
    };
    let film_fog = MIDDLE_GREY * white_grey_relation.powf(1.0 / film_power)
        / (white_black_relation.powf(1.0 / film_power)
            - white_grey_relation.powf(1.0 / film_power));
    let paper_exposure = (film_fog + MIDDLE_GREY).powf(film_power) * white_grey_relation;

    SigmoidCoefficients {
        white_target,
        black_target,
        paper_exposure,
        film_fog,
        film_power,
        paper_power,
        hue_preservation: (0.01 * params.hue_preservation).clamp(0.0, 1.0),
        color_processing: params.color_processing.shader_value(),
    }
}

#[cfg(test)]
mod tests {
    use super::{coefficients, generalized_loglogistic_sigmoid, SigmoidParams, MIDDLE_GREY};

    #[test]
    fn default_coefficients_match_darktable_5_6_c_reference() {
        let c = coefficients(SigmoidParams::default());
        let expected = [
            (c.white_target, 1.0),
            (c.black_target, 0.000151999993),
            (c.paper_exposure, 0.359695464),
            (c.film_fog, 0.00138432207),
            (c.film_power, 1.4909091),
            (c.paper_power, 1.0),
        ];
        for (actual, reference) in expected {
            assert!(
                (actual - reference).abs() < 5e-6,
                "darktable coefficient mismatch: {actual} != {reference}"
            );
        }
    }

    #[test]
    fn default_curve_pins_middle_grey_and_targets() {
        let c = coefficients(SigmoidParams::default());
        let black = generalized_loglogistic_sigmoid(
            0.0,
            c.white_target,
            c.paper_exposure,
            c.film_fog,
            c.film_power,
            c.paper_power,
        );
        let grey = generalized_loglogistic_sigmoid(
            MIDDLE_GREY,
            c.white_target,
            c.paper_exposure,
            c.film_fog,
            c.film_power,
            c.paper_power,
        );
        let very_bright = generalized_loglogistic_sigmoid(
            1.0e20,
            c.white_target,
            c.paper_exposure,
            c.film_fog,
            c.film_power,
            c.paper_power,
        );

        assert!(
            (black - c.black_target).abs() < 2e-6,
            "{black} != {}",
            c.black_target
        );
        assert!((grey - MIDDLE_GREY).abs() < 2e-6, "{grey} != {MIDDLE_GREY}");
        assert!((very_bright - c.white_target).abs() < 2e-5);
    }

    #[test]
    fn curve_is_monotonic_for_darktable_parameter_extremes() {
        for contrast in [0.1, 1.5, 10.0] {
            for skew in [-1.0, 0.0, 1.0] {
                let c = coefficients(SigmoidParams {
                    contrast,
                    skew,
                    ..SigmoidParams::default()
                });
                let mut previous = -1.0;
                for sample in 0..=2000 {
                    let x = sample as f32 / 100.0;
                    let y = generalized_loglogistic_sigmoid(
                        x,
                        c.white_target,
                        c.paper_exposure,
                        c.film_fog,
                        c.film_power,
                        c.paper_power,
                    );
                    assert!(y.is_finite());
                    assert!(
                        y + 1e-6 >= previous,
                        "curve decreased at {x}: {previous} -> {y}"
                    );
                    previous = y;
                }
            }
        }
    }
}
