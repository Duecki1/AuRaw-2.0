# RGB point curves

The Tone Curve panel now contains RGB (composite luminance), Red, Green, and Blue tabs.
Each curve stores up to eight ordered points and uses monotone cubic Hermite interpolation to avoid ringing.
The composite curve preserves chromatic ratios; channel curves operate independently in the same reversible scene-referred shaper so a diagonal curve is an exact no-op for HDR values.

The master curve is drawn white. Channel curves are drawn in their corresponding channel colors.
