# Pro Wave — Status Summary

_Last updated: 2026-11-03 · version 0.2.1 plus the Amp Designer (unreleased)_

Pro Wave is a pure-Rust (egui + cpal) DAW whose look and workflow follow Apple Logic Pro.
It runs on Windows, Linux and macOS. Everything is drawn from scratch (vector shapes, own patch
names); no Apple artwork or code is copied.

## Where we are in the roadmap (short version)

| Phase | Status |
|---|---|
| **0 — Foundation** | **Done.** CI is green on Linux, Windows and macOS; lock-free engine, logging, crash reports, CPU meter, 24- and 1000-track tests. ASIO is built but no ASIO driver has been tried; latency is measured on Windows only |
| **1 — Engine core + tracks** | **Done.** Tempo/signature maps, audio/MIDI/Pattern/Drummer regions, disk streaming, recording, `.prowave` files, aux/bus/sends, VCA, folder and External MIDI tracks. Missing: `bounces/` in the project file |
| **2 — MIDI & sequencer** | **Mostly done.** Piano Roll with quantize, velocity and MIDI Transform; the MIDI FX chain; MIDI file import/export; Event List; Step Sequencer; Score v0; Drummer and Pattern regions. Missing: MPE, SysEx, MIDI 2.0, articulation IDs, score beams/voices/printing/MusicXML, Drummer Track v0 |
| **3 — Flex & Smart Tempo** | **Mostly done.** Smart Tempo detection and editor, Adapt Project Tempo, Flex Time modes, Flex Pitch (correct to key, make a MIDI track), Groove Track. Missing: dragging Flex markers, per-note Flex Pitch (drift, vibrato, formant), a non-steady tempo grid in the editor |
| **4 — Mixer & channel strip DSP** | **Mostly done.** Twelve effects, buses and sends, automation, mix groups, 64-bit summing, delay compensation. Missing: automation of sends and the Output slot, two-handle Bezier curves, plug-in and instrument latency in the PDC, a multi-band compressor |
| **5–6 — Instruments & effects** | **Started.** The built-in synth with acoustic and electronic drum kits, and the **Amp Designer** (2026-11-03). Missing: Sampler/Quick Sampler, Drum Machine Designer, ES2, the vintage keyboards, the reverb/delay suites, Pedalboard, Mastering Assistant |
| **7–9** | **Not started:** plug-in hosting, Live Loops and the rest of the UI polish, the WASM build and the rest of Bounce/Export (a real-time Bounce Project or Section and MIDI export exist) |

## How to run

```bash
cargo run --release -p prowave-app                 # the app
cargo run --release -p prowave-app -- song.wav      # import audio files on launch
cargo run --release -p prowave-app -- --play        # start playing on launch
cargo run --release -p prowave-app -- --piano       # open the Piano Roll on launch
cargo xtask check                                   # all quality checks (see below)
cargo run -p prowave-engine --example beep          # latency test
```

`cargo run --release -p prowave-app -- --layout-shots <folder>` opens the demo song and saves a
picture of the window at four sizes (down to 1024 x 700), which is how the layout is checked on
a build server.

## Builds for other systems

| System | Status |
|---|---|
| **Windows** x86_64 | ✅ `cargo build --release -p prowave-app` (tested daily) |
| **Linux** x86_64 | ✅ Built and tested: `dist/prowave-linux-x86_64.tar.gz` (glibc 2.31+), and as `.deb`, `.rpm`, AppImage and Flatpak (release workflow, v0.1.1). **Launches under a virtual display on GitHub's runner; not yet run on real Linux hardware** |
| **macOS** (Apple Silicon + Intel) | ✅ The release workflow builds a universal `Pro Wave.app` (ad-hoc signed, not notarized) on GitHub's macOS runner. **Launched on a Mac on 2026-11-03** (`--layout-shots`: the window drew on Metal and the audio device opened); sound, the system menu bar, Cmd shortcuts and recording have not been clicked through by hand |

How the Linux build was made from Windows (cross-compiling, no WSL/Docker needed):
- Tools: Zig (`pip install ziglang`) + `cargo install cargo-zigbuild`.
- `cpal` links ALSA, so the build needs Linux's `libasound.so`: taken from Debian 11's official
  `libasound2` / `libasound2-dev` packages, plus a stand-in `pkg-config` that points at it
  (`PKG_CONFIG=<script>`, `PKG_CONFIG_ALLOW_CROSS=1`).
- `cargo zigbuild --release -p prowave-app --target x86_64-unknown-linux-gnu.2.31`
- Linux specifics in the code: the audio thread asks for real-time priority (cpal `realtime`
  feature, Linux only; needs an rtprio limit/rtkit, otherwise normal priority + a warning);
  the window class is `prowave-studio` (so `.desktop` files / icons match under X11 and
  Wayland). There is no other platform-specific code: file paths, temp folders, dialogs (rfd
  uses the desktop portal, zenity fallback) and media names are all portable.
- Linux robustness: audio start tries the default audio system's default device, then every
  output device, then every other available audio system, retries with the default buffer size
  if the requested one is refused, and accepts i8/u8/i16/u16/i32/u32/f32/f64 samples; "No sound"
  is shown in the menu bar if all fail. The "save changes?" prompt is an in-app modal.
  When OpenGL cannot draw to the window, Pro Wave tries Vulkan, then X11 under Wayland, then
  software OpenGL before giving up.
- Checked: only `libasound.so.2` + libc/libm/libpthread/libdl are linked (windowing, GPU,
  fonts, dialogs are loaded at run time); every UI glyph is in egui's bundled fonts (no
  dependence on Windows system fonts); clippy `-D warnings` and test compilation pass for
  `x86_64-unknown-linux-gnu`.
- Not done for Linux: native PipeWire/JACK back-ends (audio is ALSA, which PipeWire and
  PulseAudio emulate), a Linux test run. The tarball has `install.sh` (user install: menu
  entry, SVG icon, `.prowave` file type) — never run on Linux yet.
- Runtime libraries the binary needs: `libasound.so.2`, `libxkbcommon(-x11)`, `libwayland-client`,
  `libfontconfig`, `libdbus-1` (see `dist/README-linux.txt`).

## Workspace

| Crate | What it is |
|---|---|
| `crates/prowave-engine` | Real-time audio engine: transport, synth, disk streaming, effects, routing, cpal output |
| `crates/prowave-app` | Desktop app (egui): all the UI |
| `crates/prowave-project` | `.prowave` project file format (zip with `project.json` + `media/`) |
| `xtask` | `cargo xtask check / wasm-check / rt-check / bench / status` |

---

## ✅ Done (verified by tests and/or screenshots)

### Engine (`prowave-engine`)
- **Transport:** play, pause, stop, locate, tempo, cycle (loop), count-in, metronome, master gain.
- **Lock-free design:** UI → engine only through `Command`s on an `rtrb` queue; engine → UI
  through atomics and one shared state block per callback. Old songs are freed off the audio
  thread (garbage queue).
- **No allocations on the audio thread**, enforced by `assert_no_alloc` in debug builds and tests.
- **Tempo map:** tempo changes and curves, time-signature changes, beat⇄seconds, bars/beats,
  and SMPTE timecode (24, 25, 29.97 drop / non-drop, 30 fps) with a start time.
- **MIDI Time Code in and out:** the transport starts, jumps and stops with incoming MTC, and
  Pro Wave sends MTC to a port.
- **Built-in synthesizer:** 2 band-limited oscillators (sine/saw/square/triangle), noise,
  ADSR, low/high-pass filter with envelope, vibrato, pitch drop; drum-kit mode (kick, snare,
  toms, hats) for acoustic and electronic kits. 48 voices. Per-track synth patches.
- **Live notes** (`NoteOn`/`NoteOff`): playing the Piano Roll keys works while stopped.
- **Mixer in the engine:** per-track volume, mute, solo, equal-power pan, post-fader stereo
  meters, 64-bit summing, aux buses and up to four sends per strip (pre or post fader), VCA
  and mix groups, and delay compensation for effects that look ahead.
- **Automation in the engine:** volume, pan and every effect parameter, in Read/Touch/Latch/Write,
  sample-accurate within the block and with curved stretches.
- **Effects (`fx.rs`):** Channel EQ, Compressor, Space D reverb, Tape Delay, Chorus, Phaser,
  Overdrive, Filter, Noise Gate, Limiter, Multi-Band EQ and the **Amp Designer**. All are
  allocation-free in `process`, keep their memory when a knob moves (no clicks), clamp every
  parameter so a damaged project cannot blow up, and turn a non-finite sample into silence.
- **Amp Designer:** five amplifier models with four valve stages each, an asymmetric soft
  clip run at twice the sample rate through a 31-tap half-band filter, model-specific input
  and inter-stage filters, a bass/mid/treble/presence tone stack, three speaker cabinets (or
  none) and a microphone position. No look-ahead latency.
- **Audio regions:** WAV/AIFF/FLAC/MP3/CAF/OGG/M4A/MP4 input via symphonia, streamed from disk
  by a background thread, resampled to the device rate, with gain, fades, equal-power/linear/
  S-curve crossfades, reverse and normalize. Memory-mapped WAV/AIFF.
- **Recording:** several inputs at once, input monitoring through the track's channel strip,
  count-in, take folders with comping, capture recording, and a warning if the input is lost.
- **Flex:** Flex Time modes (Slicing, Rhythmic, Monophonic, Polyphonic, Speed, Tempophone),
  Flex Pitch detection, pitch correction and MIDI extraction; the recording is never changed.
- **Tempo analysis:** onsets and a tempo estimate for audio regions, one region or multitrack.
- **cpal output** with buffer-size request and output-latency reporting; lower latency on
  Windows (about 12 ms on the test PC) with a fallback when the driver cannot do it.
- **1000 tracks** (1000 streamed audio tracks tested over 20 s without dropouts) and 24 tracks
  of 48 kHz / 24-bit audio without dropouts.
- Builds for `wasm32-unknown-unknown` (`cargo xtask wasm-check`).
- About 540 automated tests across the workspace.

### App UI (`prowave-app`), styled after Logic Pro
- **Control bar / LCD:** SMPTE, bars/beats, locators, tempo and signature (drag to change), MIDI
  in/out, CPU/HD, count-in, metronome, master volume with stereo meters, and the icon buttons.
- **Menu bar:** File / Edit / Track / Navigate / Record / Mix / View / Window / Help, with Logic
  submenus, check marks, key commands and greyed-out entries; the macOS system menu bar uses
  the same commands. 17 interface languages (Help > Language).
- **Undo / Redo** (⌘Z / ⇧⌘Z, up to 100 steps) for edits, including recording takes.
- **Tracks area:** toolbar, ruler with cycle band, playhead, track headers, per-track heights,
  regions with MIDI notes or real waveforms, region gain/fades/crossfades/reverse/normalize,
  solo dimming, snap, repeat, locators by regions, catch, zoom.
- **Track types:** Audio, Instrument, Drummer, Aux (with a bus), Master/Stereo Out, VCA,
  Folder (track stacks) and External MIDI, in the app, the engine and `.prowave`.
- **Tempo, Signature and Marker tracks**, and the List Editors' Event list (MIDI events of the
  selected region, following the playhead).
- **Mixer:** Single/Tracks/All, channel strips with Setting, EQ, Input/instrument, Audio FX
  slots with real effects, Sends, Output, Group, Automation, pan, dB + peak hold, fader, LED
  meters, M/S; Stereo Out and Master; Mix Groups; VCA; Sends on Faders.
- **Effect windows:** sliders for every setting (automatable), Bypass and Reset, the EQ curve,
  the gain-reduction meter, and the Amp Designer faceplate with its six knobs, cabinets and
  microphone.
- **Inspector:** Region (Mute, Quantize, Transpose, Velocity, Gain, Fade In/Out, Curve, X-Fade,
  Reverse) and Track (Transpose, Velocity, Key Limit, Vel Limit) — applied non-destructively.
- **Library:** All Sounds, Search, Category › Patch columns, Revert; loading a patch renames the
  track and changes its instrument, effects, icon and sound, with a *Guitar Amp* set since
  2026-11-03.
- **Editors:** Piano Roll (editing, quantize with strength/swing, scale quantize, velocity lane,
  MIDI Transform), Score (clefs, key and time signatures, accidentals, rests, ties), Step
  Sequencer (Pattern regions with Chance, Repeat, Tie, Gate and play direction), Drummer editor,
  Smart Tempo editor, Flex editor, Event list, Automation editor, MIDI FX chain window.
- **Panes:** Quick Help, Toolbar, Note Pad, Loop Browser, All Files, Start screen.
- **Audio:** import by drag-and-drop or command line, waveform overview in the background, and
  recording with take folders.
- **Updates:** check GitHub for a newer release, show what's new, download, verify the SHA-256
  and install the right package (Windows zip, macOS app, AppImage, tarball, .deb, .rpm, Flatpak).

### Diagnostics (roadmap Phase 0)
- **CPU meter** in the LCD shows the audio callback's DSP load (peak-held, red at ≥90 %).
- **Logging:** `tracing-subscriber`, set with `RUST_LOG` (default `warn,prowave=info`).
- **Crash reports:** `human-panic` writes a report to the temp folder in release builds.
- HD meter is now measured: the LCD's HD meter shows how busy the disk streaming thread is
  (how much of the last fifth of a second it spent decoding and copying audio regions).

### Tooling (roadmap Phase 0)
- `cargo xtask check`: fmt, clippy `-D warnings`, tests, no-alloc check, miri on the engine's
  `unsafe` code, cargo-deny (licences/advisories), wasm32 build.
- GitHub Actions CI for Windows / macOS / Linux / wasm (`.github/workflows/ci.yml`), plus the
  lightweight targets (`cargo xtask bench`) and layout screenshots on macOS and Windows.
- Release workflow (`.github/workflows/release.yml`): Linux tarball, `.deb`, `.rpm`, AppImage and
  Flatpak, and a universal macOS `Pro Wave.app`. Ran on GitHub for v0.1.1.

---

## ✅ `.prowave` project files — Open / Save (roadmap Phase 1)

- `crates/prowave-project`: zip with `project.json` + `media/` (round-trip, version check,
  zip-slip protection, atomic save).
- `prowave-app/src/project_io.rs`: app model ⇄ document; audio files embedded once each.
- `prowave-app/src/file_commands.rs`: File › New… / Open… / Open Recent / Close Project /
  Save / Save As… / Save A Copy As… / Revert to Saved with native dialogs (rfd), Ctrl/Cmd+N, O,
  S, Shift+S, "save changes?" and "revert?" prompts, window title `<project> - Tracks`.
- `prowave-app/src/recent_files.rs`: the Open Recent list (last 10 projects opened or saved,
  newest first, missing files dropped) in `recent.txt` in the config folder
  (`%APPDATA%\prowave`, `~/Library/Application Support/prowave`, `~/.config/prowave`).
- `prowave song.prowave` opens a project from the command line.
- Verified: unit tests + launched the app with a project path. The dialogs themselves have not
  been clicked through by hand yet. The file has no `bounces/` folder yet.

---

## 🔒 Hardening

- `.prowave` loading treats the file as untrusted: `project.json` capped at 64 MiB and media
  at 16 GiB (zip bombs), media paths must be plain `media/…`, and every value is brought into
  range (signature, pitch/velocity, key/velocity limits, region and note lengths, quantize grid,
  cycle) so editors cannot divide by zero, overflow or panic.
- MIDI file import is bounded (128 MB, 4096 tempo and 1024 signature changes, tracks up to the
  engine's 1024, regions inside bar 9999, note pairing in linear time); pattern regions stop at
  250 000 notes and a MIDI FX chain at 1 000 000; the MIDI and MTC input queues are bounded.
- Flex and Smart Tempo keep their working files in the user's config folder, clear them after
  two weeks, and bound the number of markers and concurrent analyses.
- Every effect clamps its settings and turns a non-finite sample into silence and a reset.
- Unknown plug-in names from files are leaked once per session, not once per open.
- A failed save removes its `.prowave.tmp`; extracted media lives in
  `<temp>/prowave-media/<pid>/`, removed on exit (crash leftovers swept after 7 days).

---

## ⚡ Performance

`cargo run --release -p prowave-engine --example dsp_bench` renders 20 s of a dense
arrangement (48 busy voices, filtered saw/square + vibrato, drums, metronome, tempo change):

| | Average DSP load (one core) |
|---|---|
| Before the 2026-10-19 review | 35.8 % |
| After | 4.1 % |

- Synth modulation runs at a 16-sample control rate; per-note constants are computed once at
  note-on; no `fmodf`/`floorf` calls are left in the per-sample path.
- MIDI edits while playing swap only the notes (`Command::SetNotes`); audio files are not
  reopened unless the audio layout changes.
- The disk thread sleeps until a clip is registered instead of waking every 2 ms.
- Audio-region read-ahead follows the size of the song; memory-mapped WAV/AIFF; reverse playback
  reuses decoded audio between blocks.
- 24 stereo 48 kHz / 24-bit tracks: 0 dropouts, 1.6 % CPU (Windows); 1000 streamed tracks: 0
  dropouts on the test PC.

---

## ⚠️ Known limitations / honest notes

- **Latency:** about 12 ms on Windows on the test PC; ASIO is built but no ASIO driver has been
  tried. macOS and Linux latency are not measured.
- **macOS:** the app builds and bundles on GitHub's runner and now launches on a Mac
  (2026-11-03, screenshot run), but sound, the system menu bar and Cmd shortcuts have not been
  tried by hand.
- **Linux:** the binary starts and keeps running under a virtual display on GitHub's runner.
  Sound, a real GPU and the GPU fallbacks have not been tried by a tester.
- **More than 1024 tracks:** the engine mixes 1024 tracks; tracks after that are kept in the
  project but not played.
- **Plug-in slots** (CLAP / VST3 / AU) are not hosted yet; the Audio FX slots hold Pro Wave's
  own effects.
- The synth approximates instrument families; there are no sampled instruments, and the
  Amp Designer is a model, not a captured amplifier (no impulse-response cabinets yet).
- Cut/Copy/Paste of regions and many menu items are still shown **disabled** (not implemented).
- Score: no beams, voices, chord symbols, printing or MusicXML. MIDI files: no SMPTE-timed
  files, program changes, other controllers, pitch bend, lyrics, markers, MPE or SysEx.
- Cargo-deny ignores 4 "unmaintained" advisories (transitive via winit/egui on Linux, no fix
  available); documented in `deny.toml`.

---

## Phase status

✅ done · 🟡 partly done · ❌ not started

### Phase 0 — Foundation

| Roadmap item | | What's there / what's missing |
|---|---|---|
| `cargo xtask` + CI for mac/win/linux + wasm | ✅ | `cargo xtask check` passes on Windows, Linux and macOS; CI runs it for every push and pull request. The release workflow builds the Linux packages and the macOS `.app` |
| Lock-free primitives (`rtrb`, triple-buffer, `basedrop`) | 🟡 | `rtrb` command queue + garbage queue + shared state block + `assert_no_alloc` done. **Missing:** `basedrop` |
| Logging, crash handler, DSP-load metrics | ✅ | `tracing`, `human-panic`, CPU meter in the LCD |
| `cpal` playback + `beep` example | ✅ | Plays 440 Hz and reports output latency |
| **DoD:** beep <20 ms on all OSes | 🟡 | ~12 ms on Windows on the test PC. ASIO built, untested. macOS/Linux not measured |

### Phase 1 — Engine core + track system

| Roadmap item | | What's there / what's missing |
|---|---|---|
| Transport: sample-accurate clock, tempo map, SMPTE sync | ✅ | Tempo map with curves and signature changes ✅, SMPTE display, MTC in and out ✅. **Missing:** MMC, trying MTC with hardware |
| Track types: Audio, Instrument, Aux, Master, VCA, Folder/Summing Stacks, External MIDI | 🟡 | All in the app, the engine and `.prowave`; aux buses and sends route in the engine ✅. Folder tracks group the tracks inside them and their M and S reach them, but a folder has no channel strip or summing yet |
| Regions: audio (wav/aiff/caf/mp3/flac/ogg/m4a), MIDI, Pattern, Drummer | ✅ | All four kinds, with the Drummer editor and Pattern grid. **Missing:** per-step Chance/Tie/Repeat are there; Groove Track is on MIDI and audio ✅; Apple Loops metadata is missing |
| Non-destructive editing: clip gain, fades, crossfades, reverse, normalize | ✅ | Region Gain, Fade In/Out with a curve, X-Fade, Reverse, Normalize, applied on playback |
| Disk streaming: memory-mapped + prefetch, 1000-track test | ✅ | Background prefetch with per-clip rings; 1000-track streaming test; memory-mapped WAV/AIFF |
| Project file `.prowave` (zip, `project.json`, `media/`, `bounces/`) | 🟡 | Zip + JSON + `media/`, Open/Save/Save As, Open Recent, Save A Copy As, Revert, hardened loader ✅. **Missing:** `bounces/` |
| **DoD:** import 24 tracks 48 kHz/24-bit, play without dropouts | ✅ | 24 stereo tracks: 0 dropouts, 1.6 % CPU (Windows) |

### Phase 2 — MIDI & sequencer

| Roadmap item | | What's there / what's missing |
|---|---|---|
| MIDI 1.0 parser, MPE, Articulation ID, SysEx | 🟡 | Standard MIDI files import and export (round-trip tests; damaged files refused) ✅. **Missing:** SMPTE-timed files, program changes, pitch bend, lyrics, markers, MPE, SysEx, MIDI 2.0 |
| Piano Roll: full Logic parity | 🟡 | Editing, quantize (time and scale) with strength/swing, velocity lane, MIDI Transform, key commands ✅. **Missing:** brush, time handles, CC MIDI Draw, flam, note colour parity |
| Score Editor v0 | 🟡 | Clefs, key and time signatures, accidentals, rests, ties, click to add, right-click to delete ✅. **Missing:** beams, chords' stems, voices, printing, MusicXML |
| Event List, Step Editor, MIDI Transform, MIDI FX chain | 🟡 | Event list (MIDI events), Step Sequencer (Pattern regions), MIDI Transform, and the MIDI FX chain (Transposer, Velocity, Scale, Chord Trigger, Repeater, Arpeggiator, Randomizer, Modulator, Scripter) ✅. **Missing:** the Marker/Tempo/Signature tabs of the Event list, per-event editing, the channel-strip MIDI FX slot |
| Drummer Track v0 | 🟡 | Drummer regions with Rock/Pop/Funk/Jazz/Hip Hop/Electronic, complexity/loudness pad, fills, Convert to MIDI ✅. **Missing:** named drummers, Follow Track, a dedicated Drummer Track UI |
| **DoD:** Drummer + Bass + Keys, quantize, bounce | 🟡 | The song can be made and quantized, and **File > Bounce > Project or Section…** writes the mix to a 32-bit float WAV, at playing speed; **Export > Track as Audio File…** does one track; **Bounce > Regions in Place…** turns a MIDI region into audio. Missing: an offline (faster than real-time) bounce |

### Phase 3 — Flex & Smart Tempo

| Roadmap item | | What's there / what's missing |
|---|---|---|
| Smart Tempo Editor | 🟡 | Detect tempo of a region, ×2 / ÷2, move the downbeat, Adapt Project Tempo, multitrack analysis, Region Follows Project Tempo ✅. **Missing:** Adapt Region Tempo while recording, a non-steady tempo grid in the editor |
| Flex Time: 5 modes | 🟡 | Slicing, Rhythmic, Monophonic, Polyphonic, Speed, Tempophone; Quantize Audio ✅. **Missing:** dragging Flex markers with the mouse |
| Flex Pitch: per-note editing | 🟡 | Pitch detection, Correct Pitch to Key, Create MIDI Track ✅. **Missing:** per-note gain, drift, vibrato, formant |
| Groove Track | 🟡 | Set a Groove Track and make MIDI tracks follow its timing; audio tracks can be the source ✅. **Missing:** audio tracks following a Groove Track |
| Audio Track Editor integration | 🟡 | The Flex editor edits a region's beats and pitch. **Missing:** a full Audio Track Editor pane |

### Phase 4 — Mixer & channel strip DSP

| Roadmap item | | What's there / what's missing |
|---|---|---|
| Real effects | 🟡 | Channel EQ (3-band), Multi-Band EQ (6 bands), Compressor, Noise Gate, Limiter, Space D reverb, Tape Delay, Chorus, Phaser, Overdrive, Filter, Amp Designer ✅. **Missing:** 8-band Channel EQ with FFT analyzer, six compressor models, convolution reverb, Delay Designer, modulation suite, distortion suite, pedal board, Mastering Assistant |
| Sends / buses / aux, groups, VCA | ✅ | Up to four sends per strip (pre/post), Bus 1-8 with Aux strips, Mix Groups, VCA tracks |
| Automation, plug-in delay compensation | 🟡 | Read/Touch/Latch/Write for the fader, pan and every effect parameter, curved stretches, sample-accurate ✅; PDC for look-ahead effects ✅. **Missing:** automation of sends and Output, trim, two-handle Bezier, latency of plug-ins and instruments (not hosted yet) |
| **DoD:** 24 tracks with EQ/Comp on each, automation, PDC null test | 🟡 | 24 tracks and automation are tested; a null test is covered by the PDC tests for the effects |

### Phases 5–9 (later)

- **Phase 5–6:** built-in synth and drum kits ✅; **Amp Designer** ✅ (2026-11-03). Missing: Sampler, Quick Sampler, Drum Machine Designer, Drum Kit Designer, ES2, Vintage B3/Clav/E-Piano, Retro Synth, Sculpture, Alchemy-Lite, Ultrabeat, Studio Strings/Horns, the full FX suite, Pedalboard, Mastering Assistant.
- **Phase 7:** CLAP/VST3/AU hosting and the out-of-process sandbox — not started.
- **Phase 8:** Smart Controls, the full Key Commands window, Live Loops, Video track — not started (Quick Help, Toolbar, Note Pad, Loop Browser and All Files exist).
- **Phase 9:** WASM/AudioWorklet build; Bounce Project or Section ✅, Export Track as Audio File ✅, Bounce in Place ✅ and Edit > Bounce and Join ✅ (real time, to a WAV); Export All Tracks, MusicXML/AAF/ADM BWF — not started (MIDI export exists).
- **`.logicx` import** (`prowave-logicx-import` per `LOGICX-FORMAT-SPEC.md`) — not started.

## 📋 Still to do next (from the 0.2.x releases)

- **Recording:** cycle takes for audio, punch in and out, guitar and bass pedals before the amp.
- **Amp Designer:** a pedal board, impulse-response cabinets, and a listen with real guitar and
  bass recordings.
- **Mixer:** automation of sends and the Output slot, two-handle Bezier curves, plug-in
  (CLAP / VST3 / AU) hosting, a multi-band compressor.
- **Flex Time:** dragging Flex markers with the mouse; per-note Flex Pitch (drift, vibrato,
  formant); audio tracks following a Groove Track.
- **Smart Tempo:** Adapt Region Tempo while recording, a non-steady tempo grid in the editor.
- **Bounce / Export:** the offline bounce (faster than real time), and `bounces/` in the project file.
- **Apple Loops:** reading their tags and tempo (the audio formats they use now open).
- **Linux / macOS:** sound, recording and latency on real hardware; the NVIDIA graphics
  fallbacks confirmed by a tester.
