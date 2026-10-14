# DTF Prep 0.1 for PhotoCraft

DTF Prep prepares transparent RGB artwork for Direct-to-Film printing. It replaces soft alpha from
glows, fades, shadows, anti-aliased residue, and similar pixels with a printable pattern of fully
opaque colour and fully transparent holes. Removed pixels are always `0,0,0,0`, so hidden colour
cannot cause a RIP to build an unwanted white underbase.

The ready-to-install file is:

`dist/dtf-prep.wasm`

## Install and run

1. Open PhotoCraft.
2. Click **Filter → Plug-ins → Install Plug-in…**.
3. Choose `dist/dtf-prep.wasm`.
4. Open an RGB/RGBA PNG as a normal pixel layer.
5. Click **Filter → Plug-ins → DTF Prep…**.
6. Adjust the live preview, then click **OK**.

Installing a newer build with the same ID replaces the older DTF Prep version.

## Recommended starting settings

- **Alpha Threshold:** 8
- **Processing Mode:** Soft Transparency Only
- **Pattern:** Round Dots
- **Cell Size:** 8 px
- **Angle:** 22.5°
- **Knockout:** Off
- **Shadow Color Boost:** 0
- **Speck Cleanup:** Light

For a softer, more breathable solid print, choose **Full Artwork Forced Halftone** and start with
**Ventilation Percent: 20**. Always make a small production test because film, powder, ink, RIP
screening, press settings, and fabric all affect the smallest reliable dot.

## Controls

- **Alpha Threshold (0–255):** clears pixels whose alpha is below this value. A value of 8 removes
  only very faint residue.
- **Processing Mode:** **Soft Transparency Only** preserves fully opaque artwork and screens only
  partial transparency. **Full Artwork Forced Halftone** may also open holes in opaque areas.
- **Pattern:** chooses **Round Dots** or **Lines**.
- **Cell Size (px):** controls pattern spacing. Smaller values are finer; larger values are heavier.
- **Angle:** rotates the document-anchored screen from 0° to 180°.
- **Ventilation Percent:** limits maximum coverage in Full Artwork mode. It does nothing in Soft
  Transparency Only mode.
- **Knockout:** removes Black, White, or a Custom garment colour. Custom R/G/B use 0–255 values.
- **Knockout Tolerance:** controls how far a colour may be from the garment colour. The edge is
  handed smoothly to the halftone rather than cut away abruptly.
- **Shadow Color Boost:** conservatively increases colour saturation in darker retained pixels.
  Leave it at 0 when exact source RGB is more important.
- **Speck Cleanup:** **Light** removes isolated single pixels; **Medium** requires three retained
  pixels in a 3×3 neighbourhood; **Strong** requires five in a 5×5 neighbourhood. This is a local
  density cleanup, not connected-component analysis.

## Process only part of the image

PhotoCraft applies the result through the current selection.

- To process one region, select that artwork and run DTF Prep.
- To protect a face, eyes, logo, or small lettering, select the detail, invert the selection, then
  run DTF Prep.
- For strict binary alpha, use a hard-edged, non-feathered selection. Feathered or anti-aliased
  selection coverage is blended by PhotoCraft after the plug-in runs and can reintroduce partial
  alpha at the selection edge.

## Why hard alpha matters

A normal soft edge stores partial coverage in each pixel. Many DTF RIPs interpret every non-zero
alpha pixel as needing white ink underneath it, which can create a pale box or haze. DTF Prep turns
that coverage into spatial dots or lines: every plug-in output pixel is either alpha 0 or alpha 1.

## Current v0.1 limitations

- PhotoCraft plug-in ABI v1 provides no document DPI, so Cell Size is honestly measured in pixels;
  there is no fake LPI or millimetre control.
- The filter supports normal RGB pixel layers with straight alpha (RGBA internally). PhotoCraft
  imports ordinary RGB and RGBA PNGs into this form. Grayscale, CMYK, Lab, no-alpha surfaces,
  individual alpha channels, and Quick Mask targets are rejected safely.
- ABI v1 has no custom colour picker, so Custom Knockout uses separate R, G, and B sliders.
- ABI v1 returns only numeric filter error codes. Code 1 means malformed buffer geometry, code 2
  means an unsupported colour/channel layout, and code 3 means malformed parameters.
- Plug-in dialogs do not store named presets in ABI v1; the defaults above are the Normal starting
  point. Very fine patterns can look approximate in PhotoCraft's low-resolution live preview; the
  full-resolution result applied by **OK** is authoritative.
- DTF Prep does not modify or rasterize text/vector layers and does not perform face detection, OCR,
  or font recognition. Small raster text can be removed and replaced manually with a clean
  PhotoCraft text/vector layer before or after DTF preparation.
- A feathered selection can produce partial final alpha because selection blending happens outside
  the plug-in. No selection or a hard 0/1 selection preserves the hard-alpha guarantee.
