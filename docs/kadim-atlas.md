# Kadim Atlas — CrStudio visual concept

The channel's single visual identity for long videos and Shorts, chosen on 2026-10-22 from three bases (satellite, bronze relief and the mix) and two title faces. The reference style frame is "karma, Efsaneler Başlık" (three caves, Ephesus → Tarsus → Afşin).

## Rules

| Element | Rule |
|---|---|
| Base map | The real elevation (SRTM via Mapzen terrain tiles) carries the NASA Blue Marble NG photo (public domain). The photo is desaturated to ~35 % and multiplied with bronze (1.0, 0.64, 0.34). The sea is near-black navy. The light comes from the north-west and sits fairly high. The camera never sees a horizon or sky. Outside the satellite mosaic (0–60°E, 20–50°N) the land falls back to a bronze height ramp. |
| Colour | Dark base, cream type (≈ #F5E9D0) and one accent, amber (≈ #F2A22E). No other hues. |
| Coast | A thin amber line, faint glow. |
| Title | The channel's own face **Efsaneler Başlık (Sülüs)**, all caps, cream. |
| Tag | A small amber parallelogram above the title, set in dark Barlow Condensed Bold: chapter, date or place ("MS 250 · EFES"). |
| Body | One or two lines of Barlow SemiBold in cream. The scene's highlight phrase is set in amber. |
| Backing | Every block of type sits on a soft, feathered dark plate. Map lines never run through letters. |
| Places | Each place gets a medallion: dark disc, double amber ring, amber eight-pointed star (or a count number when listing 3+ places). A tick under the disc points at the place, a thin amber stem runs to the ground, and a pulse ring spreads when it appears. The label goes underneath on a plate: the city in the channel face, what is there in amber Barlow Condensed. |
| Routes | An amber dashed line draws itself between places and ends in an arrow head, with a spark leading it. |
| Motion | The map follows the narration (`crstudio::mapstory`). The camera arrives at a place just before the narrator names it, hops between places (lift off, dive in), and closes with a slow push-in. Type is placed where the places are not, judged on the last frame. |
| Credit | A small source line, bottom-left: "Zemin: NASA Blue Marble · Yükseklik: SRTM (Mapzen) · Şematik anlatım". |

## Where it lives

- `crates/engine/src/crstudio/blender.rs`
  - Concept `atlas`, the default.
  - Fonts: from `~/.crstudio/fonts`. These are the channel faces plus Barlow (OFL). Fonts are never kept in this repo.
  - `atlas_imagery()`: downloads and cuts the Blue Marble mosaic once, using macOS `sips`.
- `crates/engine/src/crstudio/mapstory.rs`: which places the narration names, and when.
- `tools/blender/crstudio_shot.py`
  - `atlas_land_material`, `medallion_atlas`, `dashes` and `atlas_caption` (the flat overlay scene, composited over the 3D picture).
- `tools/blender/styleframes.py`: still frames used to explore and compare looks.
