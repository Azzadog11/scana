# Design files

The sources for Scana's logo and menu icons. The app builds in PNGs made from these
(`crates/scana-app/assets/`).

## Logo

- `scana-logo.vectorcraft`: the source, for VectorCraft. `scana-logo.svg`: the same as SVG.
- A leaping kangaroo over a CAN signal, ink `#17181B` on workshop orange `#F28C28`.
- The kangaroo silhouette is "Jumping kangaroo" from OpenClipart, via FreeSVG.org (image 71169),
  in the public domain (CC0). The signal, the layout and the colours are Scana's own.
- The app's copies: export the artboard as PNG at scale 0.125 (`logo-64.png`, the top bar) and
  0.5 (`logo-256.png`).

## Menu icons

- `icons/*.svg`: 24 x 24, white 2 px lines with round ends, drawn for Scana. White, so the app
  tints them (orange for the page you are on, grey for the rest).
- The app's copies are 48 px PNGs:
  `vectorcraft-cli convert icons/NAME.svg ../crates/scana-app/assets/icons/icon-NAME.png --scale 2`.
- A new icon also needs a line in `crates/scana-app/src/assets.rs`.

## Colours

The workshop palette (dark grey, orange accent, amber warnings, red faults, green good) lives
in `crates/scana-app/src/theme.rs`, with the high-contrast and light themes.
