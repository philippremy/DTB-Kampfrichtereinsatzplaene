# Embedded document assets

Everything in this tree is compiled into the binary by `rust-embed` and served
to the Typst `World` in `dtb-ke-export`. **No file here is read from disk at
runtime.** Drop the real artwork in at the paths below; until then the exporter
falls back (bundled Typst font, monogram emblem) and still renders.

## `fonts/`

The document is typeset in **Archivo**. Commit the four static weights:

| file | style |
|------|-------|
| `Archivo-Regular.ttf`             | regular |
| `Archivo-Italic.ttf`              | italic |
| `Archivo-Bold.ttf`                | bold |
| `Archivo-BoldItalic.ttf`          | bold italic |
| `Archivo_Condensed-ExtraBold.ttf` | condensed extrabold — the competition title |

Note: Typst strips the width/weight words from the condensed file's family name
and folds it into the **"Archivo"** family, so the template selects it as a
*variant* — `text(font: "Archivo", weight: "extrabold", stretch: 75%)` — not by
a separate family name.

(The variable `Archivo[wdth,wght].ttf` + `Archivo-Italic[wdth,wght].ttf` also
work with Typst 0.15, but the static instances are the safe choice.)

If `fonts/` is empty the exporter registers Typst's bundled fonts instead, so
output renders but does not match the house style.

## `logos/`

| file | usage |
|------|-------|
| `dtb.svg`    | DTB word-mark, top-left of every page |
| `turnen.svg` | "TURNEN! · RHÖNRADTURNEN" swoosh, top-right of every page |

SVG is preferred; `dtb.png` / `turnen.png` are accepted as a fallback.

## `emblems/`

Optional per-organisation emblems, `emblems/{slug}.svg` (or `.png`). Slugs come
from `dtb_ke_export::org_slug(&OrganizationDTO)`:

- `dtb`, `irv`
- `lfv-badischer-turner-bund`, `lfv-bayerischer-turnverband`, … (one per
  `LandesturnverbandDTO`)

A missing emblem is fine — the exporter draws a text monogram instead.

### Committed 2026-09-01 — all 22

All 20 Landesturnverbände + `dtb` + `irv`, vectorised to SVG from the fetched
artwork (`FETCHED_LOGOS/`). Colours are the exact source values **except the
black ink**: every near-black fill/stroke (Illustrator "rich black" `#231f20`,
darkened greys, `black`, …) is unified to `#000000` so the UI can retint it in
one pass (`OrgEmblem::theme_ink` swaps `#000000` → `currentColor` + a root
`color`, so wordmarks stay legible in dark mode; the exporter keeps it black).
Page/white backgrounds are stripped. How each was produced (pipeline in
`work/logos/`):

| source form | emblems | method |
|---|---|---|
| already flat-vector SVG | dtb, ntb, ptb, rhtb, rtb, shtv, mecklenburg-vorpommern, hamburg | `svgo` only (drop editor cruft + invisible bounding rects) |
| vector PDF | hessischer-turnverband, schwaebischer-turnerbund | `pdf2svg` → `svgo` |
| flat 2-colour raster | maerkischer-turnerbund-brandenburg, turnverband-mittelrhein, saechsischer-turn-verband, thueringer-turnverband, saarlaendischer-turnerbund, westfaelischer-turnerbund | per-colour `potrace` (palette-quantise → mask → trace → merge) |
| multi-colour raster figure | bremer-turnverband, bayerischer-turnverband | layered `potrace`, ~6 colours; BTV's subtle blue gradient flattened to one blue |
| gradient raster, hand-rebuilt | badischer-turner-bund (magenta→yellow flame + grey ribbon), landesturnverband-sachsen-anhalt (near-flat faceted red) | traced silhouettes + reconstructed `<linearGradient>` |
| gradient raster, hard | berliner-turn-und-freizeitsport-bund | 3 traced letter silhouettes, per-letter `<linearGradient>`, `mix-blend-mode: multiply`; source is only 171 px so it is approximate (the hidden "F" is not recoverable) |
| badge with background | irv | 3 flat colours (`#e0e3eb` plate / `#899ab9` ring / `#003664` figure+wordmark) layered via `trace_layers.py` (`TRACE_TURD=30`), then the traced plate swapped for a rounded `<rect rx>`. **Keeps** its pale background, unlike the others. (`irv.sh`) |

Two fetched marks are **not** Landesturnverbände (special-interest federations,
no slug) — vectorised for future use under `FETCHED_LOGOS/vectorized/`:
`atb.svg` (Akademischer Turnbund), `btsv.svg` (Bayerischer Turnspiel-Verband).
Bavaria's Landesturnverband is the **Bayerischer Turnverband** (`btv_logo2…`).

Each file carries an explicit `width`/`height` (viewBox aspect × 160 px) so
gpui's SVG rasteriser renders it crisply in the toolbar; Typst ignores it when
`image()` is given an explicit box.

Consumed by: `dtb-ke-export` (header emblem, selected per competition from
`OrganizationDTO` via `world.set_emblem`) and `dtb-ke-ui`'s `OrgEmblem` toolbar
component.
