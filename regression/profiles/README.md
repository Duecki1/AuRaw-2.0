# Reference processing profiles

Store reviewed darktable/Ansel XMP sidecars or styles here. A valid profile for this regression suite must:

1. Select the intended demosaic method and disable automatic method switching.
2. Disable output sharpening, resizing, display tone mapping, local contrast, denoise, and other unrelated modules unless that module is the subject of the test.
3. Export a scene-linear floating-point TIFF in the manifest color space.
4. Use explicit white balance, orientation, crop, black/white levels, and highlight reconstruction settings.
5. Be hashed and versioned together with the reference application.

Profiles are deliberately not fabricated by the framework because module schemas and processing history are version-specific. Generate them in the pinned reference application, inspect the history stack, and commit the exact sidecar after licensing review.
