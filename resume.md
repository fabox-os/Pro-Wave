# Pro Wave — Status Summary

_Last updated: 2026-10-19_

Pro Wave is a pure-Rust (egui + cpal) DAW whose look and workflow follow Apple Logic Pro.
It runs on Windows and is written to be portable to macOS and Linux. Everything is drawn from
scratch (vector shapes, own patch names); no Apple artwork or code is copied.

## Where we are in the roadmap (short version)

| Phase | Status |
|---|---|
| **0 — Foundation** | **Mostly done.** CI is green on Linux, Windows and macOS. Missing: triple-buffer/`basedrop`, latency target (<20 ms) on Windows |
| **1 — Engine core + tracks** | **About half done.** Tempo map (engine), audio/MIDI regions, streaming and `.prowave` files work. Missing: the 24-track DoD test sign-off |
| 2 — MIDI & sequencer | Started early: Piano Roll editing, quantize, velocity. Everything else missing |
| 3 – 9 | Not started (some UI from Phase 4/8 — mixer look, Library — is drawn, without DSP) |

Details per item are in [Phase status](#phase-status) below.

## How to run

```bash
cargo run --release -p prowave-app                 # the app
cargo run --release -p prowave-app -- song.wav      # import audio files on launch
cargo run --release -p prowave-app -- --play        # start playing on launch
cargo run --release -p prowave-app -- --piano       # open the Piano Roll on launch
cargo xtask check                                   # all quality checks (see below)
cargo run -p prowave-engine --example beep          # latency test
```

## Builds for other systems

| System | Status |
|---|---|
| **Windows** x86_64 | ✅ `cargo build --release -p prowave-app` (tested daily) |
| **Linux** x86_64 | ✅ Built on 2026-10-19: `dist/prowave-linux-x86_64.tar.gz` (glibc 2.31+: Ubuntu 20.04+, Debian 11+). Also as `.deb`, `.rpm`, AppImage and Flatpak (GitHub release workflow, v0.1.1). **Launches under a virtual display on GitHub's runner; not yet run on real Linux hardware** |
| **macOS** (Apple Silicon + Intel) | ❌ **Cannot be built on this Windows PC.** Building for macOS needs Apple's SDK (AppKit, CoreAudio, Metal), which only comes with Xcode on a Mac. Options: build on a Mac (`cargo build --release -p prowave-app`), or push the project to GitHub and run `.github/workflows/release.yml`, which builds a universal `Pro Wave.app` on GitHub's macOS runners |

How the Linux build was made from Windows (cross-compiling, no WSL/Docker needed):
- Tools: Zig (`pip install ziglang`) + `cargo install cargo-zigbuild`.
- `cpal` links ALSA, so the build needs Linux's `libasound.so`: taken from Debian 11's official
  `libasound2` / `libasound2-dev` packages, plus a stand-in `pkg-config` that points at it
  (`PKG_CONFIG=<script>`, `PKG_CONFIG_ALLOW_CROSS=1`).
- `cargo zigbuild --release -p prowave-app --target x86_64-unknown-linux-gnu.2.31`
- Linux specifics in the code: the audio thread asks for real-time priority (cpal `realtime`
  feature, Linux only; needs an rtprio limit/rtkit, otherwise normal priority + a warning);
  the window class is `prowave` (so `.desktop` files / icons match under X11 and Wayland).
  There is no other platform-specific code: file paths, temp folders, dialogs (rfd uses the
  desktop portal, zenity fallback) and media names are all portable.
- Linux robustness (2026-10-19): audio start tries the default audio system's default device, then every output
  device, then every other available audio system, retries with the default buffer size if the requested one is refused, and accepts
  i8/u8/i16/u16/i32/u32/f32/f64 samples; "No sound" is shown in the menu bar if all fail.
  The "save changes?" prompt is an in-app modal (no zenity/system message box needed).
  Checked: only `libasound.so.2` + libc/libm/libpthread/libdl are linked (windowing, GPU,
  fonts, dialogs are loaded at run time); every UI glyph is in egui's bundled fonts (no
  dependence on Windows system fonts); clippy `-D warnings` and test compilation pass for
  `x86_64-unknown-linux-gnu` (`cargo clippy --workspace --all-targets --target
  x86_64-unknown-linux-gnu`, same env as the build). Tests were compiled, not run, on Linux.
- Not done for Linux: native PipeWire/JACK back-ends (audio is ALSA, which PipeWire and
  PulseAudio emulate), an AppImage/.deb/Flatpak package, a Linux test run. The tarball has `install.sh` (user install: menu entry, SVG icon, `.prowave` file type) — never run on Linux yet.
- Runtime libraries the binary needs: `libasound.so.2`, `libxkbcommon(-x11)`, `libwayland-client`,
  `libfontconfig`, `libdbus-1` (see `dist/README-linux.txt`).

## Workspace

| Crate | What it is |
|---|---|
| `crates/prowave-engine` | Real-time audio engine: transport, synth, disk streaming, cpal output |
| `crates/prowave-app` | Desktop app (egui): all the UI |
| `crates/prowave-project` | `.prowave` project file format (zip with `project.json` + `media/`) |
| `xtask` | `cargo xtask check / wasm-check / rt-check` |

---

## ✅ Done (verified by tests and/or screenshots)

### Engine (`prowave-engine`)
- **Transport:** play, pause, stop, locate, tempo, cycle (loop), metronome, master gain.
- **Lock-free design:** UI → engine only through `Command`s on an `rtrb` queue; engine → UI
  through atomics. Old songs are freed off the audio thread (garbage queue).
- **No allocations on the audio thread**, enforced by `assert_no_alloc` in debug builds and tests.
- **Built-in synthesizer:** 2 band-limited oscillators (sine/saw/square/triangle), noise,
  ADSR, low/high-pass filter with envelope, vibrato, pitch drop; drum-kit mode (kick, snare,
  toms, hats) for acoustic and electronic kits. 48 voices.
- **Per-track synth patches** (`SetTrackPatch`): Library patches change the sound.
- **Live notes** (`NoteOn`/`NoteOff`): playing the Piano Roll keys works while stopped.
- **Mixer in the engine:** per-track volume, mute, solo, equal-power pan, post-fader stereo meters.
- **Audio regions:** WAV/AIFF/FLAC via symphonia, streamed from disk by a background thread,
  resampled to the device rate, no underruns in tests.
- **cpal output** with buffer-size request and output-latency reporting.
- **Tempo map** (`tempo_map.rs`): tempo + time-signature changes, beat⇄seconds, bars/beats.
  The clock follows tempo changes to the exact sample; the metronome follows the signature.
  The app sends its single tempo/signature as a one-event map (`Command::SetTempoMap`).
- **Solo:** soloing a track plays only the soloed tracks until every S is released.
- Builds for `wasm32-unknown-unknown` (`cargo xtask wasm-check`).
- 32 engine tests pass (36 app tests, 8 project-file tests).

### App UI (`prowave-app`), styled after Logic Pro
- **Control bar:** view buttons, transport (Play turns green), Logic-style LCD (SMPTE,
  bars/beats, locators, tempo and time signature — drag to change, MIDI in/out, CPU/HD), count-in,
  metronome, master volume (stereo meters + glassy knob), right-hand icon buttons.
- **Menu bar:** File / Edit / Track / Navigate / Record / Mix / View / Window / Help.
  Entries drawn like Logic/macOS (check marks, key commands on the right, greyed entries);
  View menu matches Logic 11 (Y / I / X toggle Library / Inspector / Mixer). The Smart
  Controls (sun) button switches the menu bar and menus to the light (white) look.
- **Track icons:** instrument tracks show Logic's green ♫ tile until a Library patch is loaded.
- **Tracks area:** toolbar (Edit, Functions, View, tools, Snap, Drag, zoom sliders), ruler
  with yellow cycle band (drag to set), playhead, track headers (number, realistic
  instrument icon, name, M/S/R/I, volume, pan), regions (MIDI notes / real waveforms).
  **Individual track heights:** drag a track's lower edge (header or lane) to resize it,
  double-click the edge to reset; saved in `.prowave` (factor of the vertical zoom, 0.5–6×).
  **Solo:** when any track is soloed, the other tracks' regions turn gray (like Logic).
  **Playhead:** drag the playhead's head in the ruler anywhere in the song (stops at bar 1).
  Select / move regions with snap, Delete, Repeat (Ctrl+R), Set Locators by Regions, Catch,
  scroll and Ctrl+wheel zoom, add / duplicate tracks.
- **Mixer:** Single / Tracks / All, strip-type filters, row labels, channel strips (Setting,
  EQ, Input/instrument, Audio FX, Sends "Bus N" with knob + pre-fader, Output, Group, Read,
  pan knob, dB + peak hold, fader, LED meters, M/S, coloured name). Stereo Out / Master.
- **Inspector:** Region (Mute, Quantize, Transpose, Velocity) and Track (Transpose, Velocity,
  Key Limit, Vel Limit) — applied non-destructively on playback; two channel strips.
- **Library:** All Sounds, big icon, Search Sounds, Category › Patch columns, Revert.
  Loading a patch renames the track and changes instrument, effects, icon **and sound**.
- **Piano Roll:** tabs, toolbar (Pointer/Pencil, Snap, zoom), inspector (Time Quantize with
  Strength/Swing + Q, Scale Quantize + Q, Velocity), keyboard (click to play), grid fitted
  to the region, scrollbars, velocity lane (drag stems).
  Editing: click to add, drag empty space to select, drag to move, right edge to resize,
  Delete / right-click to delete, arrows to nudge, Q to quantize.
- **Audio import:** drag-and-drop or command line; waveform overview computed in background.
- **Track icons:** shaded vector drawings (drum kit, Drummer tile, guitars, synth on stand,
  vintage keys, piano, condenser mic, waveform, violin).

### Diagnostics (roadmap Phase 0)
- **CPU meter** in the LCD shows the audio callback's DSP load (peak-held, red at ≥90%,
  percentage on hover). Measured in `prowave-engine` output.rs, published lock-free.
- **Logging:** `tracing-subscriber`, set with `RUST_LOG` (default `warn,prowave=info`).
- **Crash reports:** `human-panic` writes a report to the temp folder in release builds.
- HD meter is still empty (disk load not measured).

### Tooling (roadmap Phase 0)
- `cargo xtask check`: fmt, clippy `-D warnings`, tests, no-alloc check, cargo-deny
  (licences/advisories), wasm32 build. Passes.
- GitHub Actions CI file for Windows / macOS / Linux / wasm (`.github/workflows/ci.yml`).
- Release workflow (`.github/workflows/release.yml`): Linux tarball and a universal macOS
  `Pro Wave.app` (ad-hoc signed, not notarized), plus the `.deb`, `.rpm`, AppImage and Flatpak
  packages. Ran on GitHub for v0.1.1: every job passed (packages install and remove cleanly on
  Ubuntu and Fedora; the tarball and AppImage stay up for 15 s under Xvfb). Results are workflow
  artifacts; there is no public Release page yet.
- `beep` example measures output latency.

---

## ✅ `.prowave` project files — Open / Save (roadmap Phase 1)

- `crates/prowave-project`: zip with `project.json` + `media/` (round-trip, version check,
  zip-slip protection, atomic save).
- `prowave-app/src/project_io.rs`: app model ⇄ document; audio files embedded once each.
- `prowave-app/src/file_commands.rs`: File › New… / Open… / Open Recent / Save / Save As… /
  Save A Copy As… / Revert to Saved with native dialogs (rfd), Ctrl/Cmd+N, O, S, Shift+S (shown
  in the menu), "save changes?" and "revert?" prompts (playhead/selection don't count as
  changes), window title `<project> - Tracks`.
- `prowave-app/src/recent_files.rs`: the Open Recent list (last 10 projects opened or saved,
  newest first, missing files dropped) in `recent.txt` in the config folder (`%APPDATA%\prowave`,
  `~/Library/Application Support/prowave`, `~/.config/prowave`). Listed flat in the File menu,
  with Clear Menu. Save A Copy As… writes a copy and keeps the current file, title and unsaved
  state; Revert to Saved reloads the file after asking.
- `prowave song.prowave` opens a project from the command line (audio args import into it).
- Verified: unit tests + launched the app with a project path (title, tempo, tracks loaded).
  The dialogs themselves have not been clicked through by hand yet.

---

## 🔒 Hardening (review of 2026-10-19)

- `.prowave` loading treats the file as untrusted: `project.json` capped at 64 MiB and media
  at 16 GiB (zip bombs), media paths must be plain `media/…` (a crafted project can no longer
  read other audio files on disk and smuggle them into the next save), and every value is
  brought into range (signature, pitch/velocity, key/velocity limits, region and note
  lengths, quantize grid, cycle) so editors cannot divide by zero, overflow or panic.
- Unknown plug-in names from files are leaked once per session, not once per open.
- A failed save removes its `.prowave.tmp`; extracted media lives in
  `<temp>/prowave-media/<pid>/`, removed on exit (crash leftovers swept after 7 days).

---

## ⚡ Performance (review of 2026-10-19)

`cargo run --release -p prowave-engine --example dsp_bench` renders 20 s of a dense
arrangement (48 busy voices, filtered saw/square + vibrato, drums, metronome, tempo change):

| | Average DSP load (one core) |
|---|---|
| Before | 35.8 % |
| After | 4.1 % |

- Synth modulation (vibrato, pitch drop, filter envelope and coefficient) runs at a 16-sample
  control rate; per-note constants (osc B ratio, envelope steps, decay factors) are computed
  once at note-on; no `fmodf`/`floorf` calls left in the per-sample path.
- MIDI edits while playing swap only the notes (`Command::SetNotes`); audio files are no
  longer reopened and re-streamed (which caused dropouts) unless the audio layout changes.
- The disk thread sleeps until a clip is registered instead of waking every 2 ms.

---

## ⚠️ Known limitations / honest notes

- **Latency on Windows:** ~32 ms with WASAPI shared mode; the roadmap target (<20 ms / <10 ms)
  needs ASIO (cpal `asio` feature), not wired yet.
- **CI** runs on every push to `main` (Ubuntu, Windows, macOS) and is green.
- **macOS:** the app compiles and bundles on GitHub's macOS runner (universal `.app`), but it
  has never been launched on a Mac, so Mac-only problems (Metal, CoreAudio, menus, Cmd
  shortcuts) are untested.
- **Linux:** the binary starts and keeps running under a virtual display with software
  OpenGL on GitHub's runner. Sound and a real GPU have not been tried.
- **More than 1024 tracks:** the engine mixes 1024 tracks (1000 streaming audio tracks tested on
  Windows); tracks after that are kept in the project but not played.
- **Sends** are stored and shown but not routed in the engine (no buses yet).
- **Plug-in slots** (EQ, Compressor, …) are labels only — no DSP effects yet.
- The synth approximates instrument families; there are no sampled instruments.
- **Fader cap:** you said it still differs from Logic — waiting for a zoomed Logic reference.
- Undo/Redo, Cut/Copy/Paste and many menu items are shown **disabled** (not implemented).
- The Score tab (notation: clefs, key signatures, rests, ties, bar-carried accidentals; not beams), the Step Sequencer tab (Pattern regions) and the Drummer Editor tab work; the Smart Tempo tab is a placeholder.
- Cargo-deny ignores 4 "unmaintained" advisories (transitive via winit/egui on Linux,
  no fix available); documented in `deny.toml`.

---

## Phase status

✅ done · 🟡 partly done · ❌ not started

### Phase 0 — Foundation (mostly done)

| Roadmap item | | What's there / what's missing |
|---|---|---|
| `cargo xtask` + CI for mac/win/linux + wasm | 🟡 | `cargo xtask check` passes on Windows, and CI runs it on Linux, Windows and macOS for every push to `main` (green). The release workflow builds Linux tarball/.deb/.rpm/AppImage/Flatpak and the macOS `.app` on GitHub; **macOS cannot be built on Windows** (needs a Mac or the GitHub macOS runner) |
| Lock-free primitives (`rtrb`, triple-buffer, `basedrop`) | 🟡 | `rtrb` command queue + garbage queue + `assert_no_alloc` done. **Missing:** triple-buffer, `basedrop` |
| Logging, crash handler, DSP-load metrics | ✅ | `tracing`, `human-panic`, CPU meter in the LCD |
| `cpal` playback + `beep` example | ✅ | Plays 440 Hz and reports output latency |
| **DoD:** beep <20 ms on all OSes | ❌ | ~32 ms on Windows (WASAPI shared). Needs ASIO. macOS/Linux not measured |

### Phase 1 — Engine core + track system (about half done)

| Roadmap item | | What's there / what's missing |
|---|---|---|
| Transport: sample-accurate clock, tempo map, SMPTE sync | 🟡 | Engine tempo map with tempo + signature changes ✅. App: one tempo and one editable time signature (LCD drag), saved in `.prowave`, bars/beats/regions/grid/snap follow the signature (6/8 and 7/8 match the engine) ✅. Tempo track with curves, Signature track, SMPTE display via `TempoMap` with frame rate and start time, MTC out and chase ✅ (2026-10-21). **Missing:** MMC, trying MTC with hardware |
| Track types: Audio, Instrument, Aux, Master, VCA, Folder/Summing Stacks, External MIDI | 🟡 | Audio, Instrument, Drummer (as instrument), Stereo Out/Master ✅. Aux tracks (Track › New Aux Track, mixer strip, bus label) and Folder tracks (Create/Release Track Stack, fold arrow, folder M/S) ✅ in the app and `.prowave`. VCA tracks and External MIDI tracks ✅ (2026-10-21). **Missing:** real bus routing (engine work); Folder summing is UI-only |
| Regions: audio (wav/aiff/caf/mp3/flac), MIDI, Pattern, Drummer | 🟡 | Audio WAV/AIFF/FLAC/MP3/CAF + MIDI regions ✅. Pattern regions and Drummer regions ✅ (2026-10-21; no per-step Chance/Tie/Repeat, no Groove Track) |
| Non-destructive editing: clip gain, fades, crossfades, reverse, normalize | ❌ | Only MIDI region parameters (quantize, transpose, velocity). **Missing:** all audio-region editing listed |
| Disk streaming: memory-mapped + prefetch, 1000-track test | 🟡 | Background prefetch thread with per-clip rings ✅, 1000-track streaming test ✅ (2026-10-21). Memory-mapped WAV/AIFF ✅ (2026-10-21, no speed gain, less private memory) |
| Project file `.prowave` (zip, `project.json`, `media/`, `bounces/`) | 🟡 | Zip + JSON + `media/`, Open/Save/Save As, Open Recent, Save A Copy As, Revert to Saved, hardened loader ✅. Uses JSON, not `rkyv`. **Missing:** `bounces/` |
| **DoD:** import 24 tracks 48 kHz/24-bit, play without dropouts | ❌ | Not tested yet |

## 📋 Still to do (later phases)

### Phase 2 — MIDI & sequencer
- [ ] MIDI input (hardware keyboards), MIDI file import/export, recording
- [ ] Undo/Redo, Copy/Paste
- [ ] Score Editor (v0 ✅ 2026-10-21), Step Sequencer ✅, Event List ✅, MIDI Transform, MIDI FX (Transposer, Velocity, Scale, Chord, Repeater, Arpeggiator, Randomizer ✅; MIDI import/export ✅)
- [ ] Drummer track generator

### Phase 3 — Flex & Smart Tempo
- [ ] Smart Tempo, Flex Time, Flex Pitch, Groove Track

### Phase 4 — Mixer & channel strip DSP
- [ ] Real effects: Channel EQ (8-band + analyzer), Compressor models, Gate, Limiter…
- [ ] Sends/buses/aux routing, groups, VCA
- [ ] Automation (Read/Touch/Latch/Write), plug-in delay compensation

### Phase 5–6 — Instruments & effects suite
- [ ] Sampler / Quick Sampler (SFZ), Drum Machine Designer, ES2-style synth, Vintage keys…
- [ ] Reverbs (Space Designer, ChromaVerb), delays, modulation, Pedalboard, Amp Designer
- [ ] Mastering Assistant

### Phase 7 — Plug-in hosting
- [ ] CLAP / VST3 (AU on macOS) in a sandboxed process

### Phase 8 — UI polish & Live Loops
- [ ] Smart Controls, Key Commands, Live Loops grid, Loop Browser, Video track

### Phase 9 — Web & export
- [ ] WASM + AudioWorklet build, Bounce/Export (WAV, MIDI, stems)

### `.logicx` import (separate crate in the spec)
- [ ] `prowave-logicx-import` per `LOGICX-FORMAT-SPEC.md`
