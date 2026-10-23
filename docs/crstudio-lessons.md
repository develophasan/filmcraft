# CrStudio — lessons from the first Kadim Atlas video

What went wrong while making "Yedi Uyurlar Efsanesi" (October 2026), why it happened, and what now
stops it from happening again. Read this before changing the voice, Blender or assembly code.

| Symptom | Cause | Fix and guard |
|---|---|---|
| The narration stops mid-scene in every cut. Only music plays after that. | A narration re-synthesized under the same file name reused the project item imported earlier. The old take's length cut the new clip. | `assemble::import_path` reuses an item only when the file's identity (size + fingerprint) still matches; otherwise it imports again. Test: `a_rewritten_narration_file_is_imported_again`. The cut also checks itself: `Kurgu denetimi` logs every narration placed shorter than its file. |
| "Gençler" spoken as "Gesar". | `eleven_v3` at low stability sometimes slurs or misreads an ordinary word. | Every ElevenLabs take is transcribed (Scribe STT) and compared with the script (`voicecheck`). A misread ordinary word triggers a new take: at most 3 takes, and the best one is kept. Proper names are reported, not retried. The Seslendirme stage shows "dinlendi ✓" or the differences. |
| Map shots "freeze" for 1–2 s at every stop and before every cut. | The camera eased to zero velocity: smoothstep in and out, plus a still final push-in. | The `shot_journey` camera has a constant slow orbit and push under every move. The last move eases in but never out. Title cards drift linearly. Check: frame-difference scan, with no run of near-identical frames ≥ 0.25 s. |
| A shot holds its last frame. | Renders were capped at 20 s, while scenes ran longer. | The render length is the scene length (up to 60 s) + 0.8 s handle. When the long cut's pause makes a slot longer than the render, the clip is slowed (≥ 75 %) instead of held. `Kurgu denetimi` flags a shot that still cannot fill its slot. |
| The caption body is cut at "Kazılarda 5." | An ordinal ("5. ve 6. yüzyıl") was taken as a sentence end. | `first_sentence` skips ellipses and ordinals followed by a lower-case word. Test: `ordinals_and_ellipses_do_not_end_the_sentence`. |
| The medallion stem runs through the label, and the medallion leaves the frame. | The stem and label are in the same plane, and the camera aimed at the ground. | The label is moved toward the camera, so the stem passes behind its plate. The camera aims between the ground and the medallion. The place sits right of centre, and the caption owns the left. |
| The script-writer agent set cheap synthesized SFX on its own, and new projects opened with the synthesized score. | The default `none` only covered empty cues, and `crstudio.new` set the tone's score. | Synthesized cues are cut in only with `synthSfx` on. A new project uses the music library whenever one exists. |
| The narration was flat. The dramatic tags were hand-written for the first video. | The script agent writes plain text only. | The **Ses Yönetmeni** step runs after the script, and on demand with `crstudio.directVoice`. It adds v3 tags and pauses. A directed text whose words differ from the script is rejected. |
| The listening check re-read "Camasb" (a name opening a sentence) and spent credits. | A capitalized word at a sentence start counted as an ordinary word. | `voicecheck::proper_names` collects every word written mid-sentence with a capital anywhere in the script. Those words are names everywhere. |
| The background "effects" sound cheap. | Synthesized SFX cues and a synthesized score. | SFX default to `none` (`"auto"` brings the old cues back on purpose). Music comes from a licensed library: `<project>/muzik`, else the shared `~/.crstudio/muzik`, in file order. Each file has a `.attribution` sidecar, and its credit goes in the video description. |
| Scenes rush into each other. | Each slot was the narration plus 0.35 s. | The long cut pauses 0.7 s after each scene and 1.8 s before a new chapter (`audio::scene_pause`). Music is ducked −10 dB under the voice with 0.5 s fades, so it rises in the pauses. |
| The Shorts end card text overflows its button. | The fixed-size pill was too small for "DEVAMI KANALDA". | The card uses "ABONE OL", and the note reads "DEVAMI UZUN VİDEODA". |
| Playback in the app stutters. | The debug build was decoding 1080p ProRes 422 HQ Blender shots. | Run the release build, bundled as `~/Applications/CrStudio.app` (`tools/macos/crstudio-app.sh --release`). |
| The app shows "filmcraft", and the macOS menus switch back to English. | The binary ran unbundled. The native-menu updater did not know Turkish and fell back to English. | `tools/macos/crstudio-app.sh` builds the bundle (CrStudio name, `tr` region, control port 9876). `native_menu.rs` maps `app.language.turkish`. |
| Clicking a stage while a job ran jumped back. | `poll` replaced the whole pipeline with the worker's copy, stage included. | The stage on screen is kept unless the job itself moves on. Test: `stage_navigation_sticks_while_a_job_runs`. |

## Checks before an export

1. **Seslendirme:** every scene shows "dinlendi ✓", or its differences are only names. Play them all with "▶ Tümünü dinle".
2. **Kurgu:** the log shows `Kurgu denetimi · Seslendirmeler tam, çekimler sahneleri dolduruyor`.
3. **Studio:** play the long cut once in the release build.
4. **Export.** Afterwards, scan the file: frame-difference runs, and the voice RMS reaching each scene's end.

## Still open

- Shorts show 16:9 Blender shots letterboxed. Vertical renders are needed for readable captions and visible motion.
- The satellite mosaic covers only 0–60°E. Places west of 0° (Tours, Spain) show a seam. The wider mosaic code (B1 + C1 tiles, 20°W–90°E) is in git history (`sync 13:09`) and needs the extra ~80 MB tile.
- Chapter-transition sounds (ney, frame drum, gong) should come from a licensed library like the music, not from synthesis.
