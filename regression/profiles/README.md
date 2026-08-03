# Pinned reference processing histories

The YAML files in this directory are audited processing-history contracts. Each one is bound to an exact reference-engine version or source revision and is SHA-256 pinned by `../reference-engines.yaml`.

A contract fixes:

1. Active-area crop, orientation, black/white metadata, and camera white balance.
2. Inpaint-opposed highlight reconstruction and fixed Bayer/X-Trans demosaic methods.
3. Disabled denoise, creative tone/color, sharpening, and resize modules.
4. Linear D65 Rec.2020 working/output color and float32 TIFF export.

The contracts deliberately avoid fabricated XMP parameter blobs: darktable and Ansel serialize module parameters according to the exact build. Reference-generation wrappers must apply the contract in the pinned application, export the full-resolution linear TIFF, and retain the application-generated XMP/style with the reference artifacts. The renderer manifest records the contract path and hash.

Validate the committed contracts:

```sh
python3 scripts/image_regression.py validate-reference-engines \
  --config regression/reference-engines.yaml
```
