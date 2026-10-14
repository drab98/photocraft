# DTF Prep: a PhotoCraft plug-in

DTF Prep is a clean-room PhotoCraft ABI v1 filter for RGB artwork with straight alpha. It converts
soft transparency into deterministic hard-alpha round-dot or line coverage, can ventilate opaque
artwork, knock out a garment colour, modestly boost shadow colour, and remove locally isolated
halftone specks.

The implementation is `no_std`, has no imports or allocator, and uses `_image.x` / `_image.y` for a
single document-anchored screen across every processing band. Its fixed two-pixel overlap supports
the strongest cleanup level without a band-boundary seam.

## Build and test

```sh
rustup target add wasm32-unknown-unknown
./build.sh
cargo test --manifest-path ../../../crates/plugins/Cargo.toml --test dtf_prep
```

`build.sh` writes the installable module to `dist/dtf-prep.wasm`. The ABI, parameter format, straight
alpha convention, host selection blend, and pixel-based (not physical-resolution) geometry are
documented in [`docs/plugins.md`](../../../docs/plugins.md).

## Cleanup levels

- **Off:** no neighbourhood cleanup.
- **Light:** removes a retained pixel with no other retained pixel in its 3×3 neighbourhood.
- **Medium:** requires at least three retained pixels in the 3×3 neighbourhood, including itself.
- **Strong:** requires at least five retained pixels in the 5×5 neighbourhood, including itself.

Opaque pixels protected by **Soft Transparency Only** are never removed by cleanup. Cleanup is a
small local density test, not a connected-component analysis.
