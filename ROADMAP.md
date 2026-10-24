# Pro Wave — Complete Logic Pro Clone Roadmap
**Codename: `prowave-studio` | Stack: Pure Rust + WebAssembly | Targets: macOS, Linux, Windows, Web**
> Pro Wave is an independent, clean-room DAW inspired by Apple Logic Pro. Logic Pro is a trademark of Apple Inc. No Apple code, assets, samples, or UI assets are reused. This document is a functional specification for re-implementation.

## 0. Goal

Build a 1:1 functional clone of Logic Pro 11 (as of 2024-2025) in pure Rust + WASM. Every menu, editor, instrument, effect, and workflow must be accounted for so an AI coding agent (Claude) can implement without guessing.

This roadmap is the single source of truth. If it's not in here, it doesn't get built.

---

## 1. Complete Logic Pro Feature Inventory — What We Must Clone

This is exhaustive. Based on Logic Pro 10.7 / 11.

### 1.1 Transport, Timeline & Project

- **Transport:** Play, Stop, Record, Play from Selection, Play from Last Locate, Pause, Cycle/Loop, Count-in, Metronome (Klopfgeist with custom sounds), Varispeed (Speed Only, Varispeed + Pitch)
- **Timeline:** Bars|Beats, Time (SMPTE, HMS, Samples), Tempo Track, Time Signature Track, Beat Mapping Track, Arrangement Track (Intro, Verse, Chorus markers with colors), Marker Track
- **Smart Tempo:** Adapt Project Tempo to imported audio, Adapt Region Tempo, Keep Project Tempo, Automatic, Aligned. Smart Tempo Editor (edit downbeats, beats). Multitrack Smart Tempo analysis
- **Project Management:** Alternatives (like git branches for arrangement), Backups, Project Clean Up, Consolidate, 1000 Audio Tracks, 1000 Software Instrument Tracks, 1000 Aux, 1000 External MIDI, Sample Rate 44.1-192kHz, 64-bit summing
- **Tuning:** Equal Tempered, Hermode Tuning (HMT), User Tuning (Scala .tun), A=440-446 variable, per-project

### 1.2 Track Types & Track Management

- **Audio Track:** Mono, Stereo, Input Monitoring, Record Enable, Input/Output assignment, Freeze, Track Stacks (Folder Stack = grouping, Summing Stack = submix bus with aux), Track Alternatives (playlists), Groove Track (master timing reference), Track Icon, Track Color
- **Software Instrument Track:** MIDI Channel, Layered, Multi-timbral (16 channels per instance)
- **Drummer Track:** Virtual drummer with 28+ drummers (Rock, Hip-Hop, EDM, etc.), X/Y pad Complexity/Loudness, Fill knob, Kit piece mutes, Follow Track (groove), Drum Kit Designer integration, Convert to MIDI region
- **External MIDI / Instrument Track:** Program Change, Bank, External synth routing
- **Aux / Channel Strip / Bus / VCA / Master:** Aux as destination for sends/busses, VCA faders
- **Folder Track:** Pack folders
- **Track Inspector:** Region inspector (Quantize, Velocity, Transpose, Gain, Flex) + Track Inspector (Instrument, MIDI FX, Audio FX chain, Sends, EQ thumbnail)
- **Library & Patches:** Patch = Channel Strip + MIDI FX + Instrument + FX. Full Library with categories (Synth, Piano, Guitar...). User patches

### 1.3 Audio Recording, Editing & Comping

- **Recording:** Punch-in/out, Autopunch, Replace mode, Take Folders (pack takes into folder), Quick Swipe Comping (swipe to select best parts of takes), Comp to new track, Crossfade Tool (equal power, equal gain, custom curve, S-curve), Fade Tool
- **Audio Regions:** Gain tool (clip gain +6/-24dB), Flex Time on region, Gain envelope, Reverse, Trim, Split at Playhead / Silence / Transient
- **Audio Editor:** Audio File Editor (destructive view of file), Audio Track Editor (non-destructive, Flex Time/Pitch), Transient Detection (Sensitivity slider), Strip Silence (threshold, min length)
- **Marquee Tool:** Marquee selection, Marquee + Selection-based processing (Gain, Normalize, Reverse, Silence)
- **File Management:** Project Audio Browser (all audio files), Audio File metadata, Convert sample rate, Copy/Convert to project, Optimize files

### 1.4 Flex Time & Flex Pitch (Critical)

- **Flex Time Modes:** Slicing (drums), Rhythmic, Monophonic (vocals/bass), Polyphonic (chords), Tempophone (creative), Speed (time stretch + pitch shift linked)
- **Flex Time Editing:** Flex markers, Quantize audio (like MIDI), Groove extraction, Move transients, Time stretch region by dragging
- **Flex Pitch:** Per-note pitch editing inside Audio Track Editor, like piano roll for audio. Pitch Drift, Fine Pitch, Gain per note, Vibrato, Formant shift. Scale quantize (snap to scale), Reference Tuning. Create MIDI from Flex Pitch

### 1.5 MIDI Sequencing & Editors

- **Piano Roll:** Quantize (with Swing, Strength, Q-Swing), Velocity tool, Brush tool (paint notes), Time Handles (stretch MIDI), Note Repeat, Humanize, Select Equal Positions/Color/Pitch, MIDI Draw (CC automation per region), Scale Quantize, Flam, Velocity Limiter, Note Color = Velocity/Pitch/Voice
- **Score Editor:** Full notation, Staff Styles (Piano, Guitar Tab, Drum), Score Sets, Brackets, Lyrics, Chord Symbols, Time/Key Signatures per staff, MIDI Meaning -> Notation, Print
- **Step Sequencer:** Pattern Regions, Step Sequencer Grid (like 808), per-step: Note, Velocity, Gate, Tie, Chance, Repeat, Loop direction (Forward, Backward, Ping-Pong, Random), Automation per row, Convert to MIDI region
- **Event List:** Raw MIDI event editor, SysEx
- **Step Editor (Hyper Editor):** Drum lane editor for CCs
- **MIDI Transform:** Fixed transforms (Crescendo, Humanize, Quantize, Transpose, Double Speed, etc.) + Custom Transform with conditions (Select and Operate)
- **MIDI FX Chain (pre-instrument, 4 slots):** Arpeggiator, Chord Trigger, Modulator (LFO/Env to MIDI CC), Note Repeater, Randomizer, Transposer, Scripter (JavaScript MIDI processing - we will use Rhai/WASM scripting in Rust), Velocity Processor
- **MIDI 2.0 & MPE:** MPE editing (per-note Pitch Bend, Pressure, Timbre), MIDI 2.0 High Resolution CC, Per-Note controllers

### 1.6 Instruments — Logic Pro has 40+ — We must clone all categories

**Sampler Family:**
- **Sampler (EXS24 successor):** Multisampled, Zones, Groups, Velocity layers, Round Robin, Filter, LFOs, ADSR, Mod Matrix (10 slots), Import EXS, SFZ, DLS. Visual mapping editor
- **Quick Sampler:** Drag-drop audio -> auto-slice (Transient, Beat, Equal), One-Shot, Classic, Slice mode with 8 slices, Record directly, Trim, Loop, Flex Time integration
- **Drum Machine Designer:** 16/24/48 pads, each pad = Quick Sampler + channel strip with inserts/sends. Pad mute/solo, Q-Sampler controls per pad
- **Drum Kit Designer:** Acoustic kits (20 kits), Producer Kits (multi-mic with bleed control, overhead/room mics), Drum Replacement (replace kit piece)

**Synths:**
- **Alchemy (Flagship):** 4 sources (VA, Additive, Spectral, Granular, Sampler), 3000+ presets, Morph pad X/Y, 16 LFOs, 16 Env, MSEG, Arp, Effects, Granular engine. For v1.0 we will build simplified version: Alchemy-Lite (2 sources, VA + Sampler)
- **ES2:** Vector synth, 3 oscillators, 1 filter, Vector Envelope, Planer Pad, Distortion, Modulation Router (10 slots) - Complex but must be cloned exactly
- **ES1, EFM1 (FM), ES P, ES E, ES M (Monophonic)** — simpler analog/FM synths
- **Vintage Series:** Vintage B3 Organ (drawbars, percussion, Leslie emulation), Vintage Clav (D6), Vintage Electric Piano (Rhodes/Wurli), Vintage Mellotron (Tape playback)
- **Sculpture:** Physical modeling (string, nylon, steel, glass), Object + Material model, Morphing
- **Retro Synth:** 4 engines in one: Analog, Sync, Wavetable, FM
- **Ultrabeat:** 25 voices drum synth (VA, FM, Sample, Model), Step Sequencer built-in
- **Vocal Synth (Vocal Transformer)**
- **Studio Strings, Studio Horns:** Orchestral with Articulation ID (key switching)
- **New Logic 11 Session Players:** Bass Player, Keyboard Player (like Drummer but for bass/keys)

**Implementation Plan for Instruments:** Build trait `Instrument: DspNode` with `nih-plug` compatible. All instruments share common `ModMatrix`, `Lfo`, `Adsr`.

### 1.7 Effects — Logic has 80+ effects — Clone by Category

**Must clone all, with identical controls for preset compatibility:**

**Dynamics:**
- Compressor: Models Platinum Digital, Studio VCA, Studio FET, Vintage VCA, Vintage FET, Vintage Opto. Threshold, Ratio, Knee, Attack, Release, Auto Gain, Sidechain filter, Mix
- Limiter, Adaptive Limiter, Multipressor (4-band compressor), Enveloper (transient shaper), Noise Gate, Expander, DeEsser 2, Surround Compressor

**EQ:**
- Channel EQ: 8-band parametric, Analyzer (FFT), Analyzer Pre/Post, Q-Couple, Gain-Q-Couple, 20Hz-20kHz, 4x oversampling
- Linear Phase EQ (same as Channel EQ but linear phase)
- Vintage EQs: Vintage Graphic EQ (API 560 style), Vintage Tube EQ (Pultec), Vintage Console EQ (Neve)
- Single Band EQs, Match EQ (EQ matching)

**Filter:**
- AutoFilter: LFO, Env Follower, Distortion

**Reverb & Space:**
- Space Designer: Convolution reverb, IRs (1000+ IRs), Length, Filter, Envelope, Direct/Rev
- ChromaVerb: Algorithmic reverb, 14 models (Hall, Room, Plate...), 3-band Decay EQ, Attack/Smooth/Spin
- SilverVerb, PlatinumVerb, EnVerb, Early Reflections

**Delay:**
- Delay Designer: Tap Delay Designer (visual tap programming, 26 taps, pitch/filter per tap)
- Tape Delay: Saturation, Flutter, LFO
- Echo, Stereo Delay, Sample Delay (sample-accurate delay for phase correction)

**Modulation:**
- Chorus, Ensemble, Flanger, Phaser, Tremolo, Auto Pan, Spreader, Microphaser, Ringshifter (frequency shifter), Scanner Vibrato (Leslie vibrato)

**Distortion & Overdrive:**
- Overdrive, Distortion, Bitcrusher, Clip Distortion, Phase Distortion, Distortion II

**Imaging & Panning:**
- Direction Mixer, Stereo Spread, Binaural Post-Processing, Gain (utility), MultiMeter, Loudness Meter, Correlation Meter, Tuner, Test Oscillator

**Pitch:**
- Pitch Correction (auto-tune like), Pitch Shifter

**Special / Utility:**
- Exciter, SubBass (bass synth), Voice Transformer (formant), Limiter, Spectral Gate, EnVerb

**Mastering:**
- Mastering Assistant (AI in Logic 11): Analyze track, suggest EQ, Dynamics, Stereo Spread, Loudness. We will clone as rule-based + ML (ONNX in Rust via `ort`)
- Stem Splitter (Logic 11): Split mix into Drums, Bass, Vocals, Other using ML (Demucs). We will use `tract` + Demucs model

**MIDI FX (already listed) + Audio FX from Logic's Pedalboard:** 35 guitar pedals (Delay, Distortion, Modulation pedals). Amp Designer: Amp + Cab + Mic modeling (100+ amps/cabs)

**Implementation:** All FX implement `DspNode` with latency reporting for PDC. Use `fundsp` + custom. Each FX must have preset serialization JSON compatible with Logic's .pst where possible.

### 1.8 Mixing, Automation & Metering

- **Mixer:** Tracks, Aux, Bus, Output, Master. Fader (0 to +6dB), Pan/Balance/Stereo Pan (with Stereo Pan for stereo tracks showing left/right position), Input, Sends (pre/post, 8 per channel), Inserts (15 per channel), EQ thumbnail (click to open Channel EQ), Sends on Fader, VCA assignment, Group (Mix groups with attributes: Volume, Pan, Mute, Solo, etc.), All Tracks vs Single Track view, Channel Strip Settings (Save/Load)
- **Automation:** Track Automation vs Region Automation (MIDI Draw). Modes: Read, Touch, Latch, Write. Trim. Automation curves: Linear, Exponential, Bezier with 2 handles. Automation Select Tool, Automation Snap, Relative/Absolute, Automation for every plugin parameter (sample-accurate), Automation Event List
- **Metering:** Pre/Post fader metering, Peak, RMS, Loudness (LUFS), True Peak, Clip Indicator, Peak Hold, Level Meter with scale (0dB = -12dBFS option)
- **Latency Compensation:** Plugin Delay Compensation (PDC) full, Low Latency Mode (bypass high-latency plugins when recording), Plug-in Manager showing latency

### 1.9 Browsing, Loops & Sound Library

- **Loop Browser:** Apple Loops (.aiff/.caf with metadata: key, tempo, style, instrument, category), Audio Loops (blue), MIDI Loops (green), Pattern Loops (yellow), Drag-Drop with auto-adaptation to project tempo/key
- **Sound Library:** Downloadable content (70GB), Relocate Sound Library, Logic Content (samples, IRs, Alchemy samples)
- **All Files Browser:** Finder-like browser, Audio files, Movies

### 1.10 Live Loops & Performance

- **Live Loops Grid:** Cells (loops), Scenes (columns), Trigger modes (Start/Stop, One Shot), Quantize Start (1 bar, 2 beats, etc.), Play From Here, Live Loops for Drummer, Step Sequencer patterns inside Live Loops
- **Performance Recording:** Record Live Loops performance into Tracks area (arrangement)
- **Remix FX:** DJ-style effects (Gate, Bitcrusher, Filter, Gater, Repeater, Reverse) with X/Y pad, applied to master or bus

### 1.11 Score, Video & Sync

- **Score Editor:** As above, plus Drum Notation, Tablature, Chord Track -> Score, Export MusicXML, Print
- **Video Track:** Movie Track, Show Movie (floating window), Filmstrip thumbnails, SMPTE Sync, Detect Cuts, Export Audio to Movie, Synchronization Settings (MTC, MMC)
- **Tempo & Time Signature List Editor:** Edit tempo events

### 1.12 Sharing, Bounce & Export

- **Bounce:** Bounce Project/Region/Track in Place, Offline/Real-time, Include/Exclude Automation, Normalize On/Off/Overload Protection, MP3/AAC/WAV/AIFF, 16/24/32-bit, Dither (Apogee UV22HR)
- **Export:** Export All Tracks as Audio Files, Export MIDI, Export Score, Export as AAF (for Pro Tools), Export as MusicXML
- **Share to:** GarageBand, Voice Memos, SoundCloud (deprecated, but we will do generic upload), Dolby Atmos ADM BWF

### 1.13 Control Surfaces & Hardware

- **Smart Controls:** Auto-mapped knobs for each instrument/effect (8 knobs), Mapping editor (Map knob to multiple parameters with range), Smart Controls for whole Patch
- **Controller Assignments:** Learn mode (MIDI Learn), Assignment for Fader, Pan, Plugin Params, Key Commands
- **Control Surface Support:** Mackie Control, Logic Remote (iPad app) - we will build Web Remote instead via WASM/WebSocket
- **Key Commands:** 1200+ key commands, Customizable, Presets (US, etc.), Search, Conflict detection

---

## 2. Technical Architecture for ProWave (Pure Rust + WASM)

```
[ UI Thread: egui ] --(rtrb SPSC: Command)--> [ Engine Thread: prowave-engine ]
         ^                                          |
         |----(rtrb SPSC: Metering/Events)----------|
                                                    v
                                           [ Audio Thread: 1-8ms deadline, no alloc ]
                                                    |
                                    cpal (native) / AudioWorklet (web)
```

**Workspace Structure:**
```
/crates
  /prowave-engine    # lock-free audio graph, transport, PDC
  /prowave-dsp       # all FX: Eq, Comp, Reverb, etc. trait DspNode
  /prowave-instruments # all instruments: Sampler, Quick Sampler, ES2, Sculpture, Vintage...
  /prowave-mixer     # summing, busses, VCA, groups
  /prowave-midi      # piano roll logic, score data, step seq, MPE, MIDI 2.0, MIDI FX
  /prowave-plugin-host # CLAP primary, VST3, AU, sandboxed process
  /prowave-project   # .prowave file (zip: project.json + media/ + irs/ + sampler instruments)
  /prowave-ui        # egui widgets: Tracks, Mixer, Piano Roll, Score, Loop Browser, Library
  /prowave-wasm      # wasm-bindgen glue + AudioWorklet entry + OPFS + Web Remote
/xtask
```

**Critical Rust Rules (for Claude):**
- Audio thread: `#![forbid(alloc)]` in process() path, use `assert_no_alloc`. No mutex, no `println!`, no `Vec::new()`
- All params: `AtomicF32` + 1-pole smoother (`ParamSmoother { target, current, coeff }`)
- Graph: Petgraph-like `AudioGraph { nodes: Vec<Node>, edges: Vec<Edge> }` sorted topologically each time graph changes (on engine thread, not audio thread)
- PDC: Each node reports `latency_samples()`. Engine computes max latency, adds delay compensation to other paths using `rubato` + delay lines
- State: Triple-buffer for UI->Engine. `Command` enum is only way UI mutates engine

---

## 3. Phased Roadmap — Implementation Order for Claude

### Current work split (updated 2026-10-20)

**Claude owns every crate** (`prowave-engine`, `prowave-app`, `prowave-project`, `xtask`,
workflows, docs): all feature work and all engine work. **ChatGPT 6 does small, separate
checks only** and never edits a crate's feature code. Each check ends in a short report and,
where something is wrong, a minimal fix with a test, in its own commit.

#### ChatGPT 6: small tasks (bug and exploit checks)

1. **Hostile project files.** Try to break `.prowave` opening (`prowave-project`,
   `project_io.rs`): zip bombs and huge declared sizes, `../` and absolute paths in media
   names, duplicate and very long names, NaN/inf/huge numbers in `project.json`, deeply nested
   folders, thousands of tracks/regions/notes. Expected: a clear error or clamped values, never
   a crash, a hang, a file written outside the project's media folder, or unbounded memory.
2. **Hostile audio files.** The same for `AudioFile::open` / streaming (`disk.rs`): truncated
   and zero-length files, header lying about its length, absurd sample rates and channel
   counts, 8/16/24/32-bit and float variants, AIFF and FLAC edge cases.
3. **Panic hunt.** Grep non-test code for `unwrap`, `expect`, `[..]` indexing, `as` casts and
   integer arithmetic that can panic or overflow on input from files or the UI; list each with
   the input that triggers it.
4. **`unsafe` and dependencies.** List every `unsafe` block with its safety argument, run
   `cargo deny` / `cargo audit`, and note new advisories.
5. **Translation files.** Read `crates/prowave-app/locales/*.txt` for wrong or unnatural
   wording (they were written by an AI); fix a line only with a reason, keep `{}` placeholders.

Rules: small diffs, `cargo xtask check` green, one commit per finding.

#### Claude: everything else, in this order

1. ✅ Tempo track, Marker track, Region gain / fades / reverse / normalize, compact menus,
   17 languages (see the changelog).
   ✅ 0.1.8: MIDI keyboard recording (with the sustain pedal), take folders from Cycle
   recording, the Event list (List Editors), updates from GitHub (check and install for each
   kind of install), Logic-accurate colours (regions, playhead, Piano Roll, sliders).
2. Engine leftovers (Phase 0): ✅ triple buffer, ✅ Windows latency 32 -> 12 ms (IAudioClient3).
   Left: try ASIO on real hardware, `basedrop` or a documented reason to keep the garbage queue
   (kept for now), PipeWire/JACK.
3. ✅ (2026-10-21) Signature track, Fade tool, crossfades, VCA, External MIDI, MP3/CAF, 1000
   tracks. Tempo curves, SMPTE and MTC, Pattern and Drummer regions, memory-mapped streaming: ✅ (2026-10-21, see below).
   ✅ 0.1.9 (2026-10-21): all of the above plus MIDI Transform, Modulator, Scripter, sustain pedal
   and controllers to External MIDI, and the Score with clefs, key signatures, rests and ties.
   Also: Check for Updates retries on slow connections (3 tries of 30 s).
   Also: File > Close Project, and the Flatpak can reach the network (Check for Updates failed
   there with a name-resolution error).
   ⚠️ CI: `xtask check` fails on Ubuntu and macOS since the 0.1.9 commit (Windows passes); cause
   not found yet, the job logs need a GitHub login to read.
4. Flex / Smart Tempo groundwork (Phases 2-3), real effects and buses.
   ⚠️ Started 2026-10-24: onset detection and tempo estimation of audio files in the engine
   (`analysis.rs`: log energy of the first difference in 2.5 ms blocks, peaks over a moving
   threshold; tempo from the onset strength's autocorrelation, 60-200 BPM, with a check for
   the double tempo), and Detect Tempo / Adapt Project Tempo to Region on audio regions.
   Next: Smart Tempo Editor, tempo maps from audio, Flex markers from the onsets.

Engine items already done by ChatGPT 6 and reviewed on 2026-10-20: region gain, fades and
reverse (`ClipEdit`), `peak_of`, tempo ramps, SMPTE, optional `asio` feature, the 24-track
streaming check, input hardening. Review fixes: SMPTE labels one frame early at frame-exact
positions (fixed, tested), prefetch size now follows the number of clips.

### Phase 0: Foundation (Weeks 1-4)
- [x] `cargo xtask` setup, CI for mac/win/linux + wasm
  - Done: `cargo xtask check|wasm-check|rt-check` (fmt, clippy -D warnings, tests, no-alloc, cargo-deny, wasm32) passes on Windows.
  - CI: `.github/workflows/ci.yml` runs `cargo xtask check` on Ubuntu, Windows and macOS (plus the
    cross-target builds) on every push to `main` and is green on GitHub.
  - Linux: x86_64 release binary cross-builds from Windows (Zig + `cargo-zigbuild`, glibc 2.31+) and is packaged in `dist/prowave-linux-x86_64.tar.gz` with a full user README; audio thread asks for real-time priority (cpal `realtime`); window class set for desktop integration. Audio start falls back across devices/sample formats; the unsaved-changes prompt is in-app (no zenity); clippy `-D warnings` + test compilation pass for the Linux target; real DT_NEEDED is only libasound + libc. The release workflow ran on GitHub (v0.1.1) and the packaged binary stays up for 15 s under a virtual X display with software OpenGL (tarball and AppImage), but it has not been run on real Linux hardware: no audio device or GPU has been tried.
  - macOS: cannot be built on Windows (needs Apple's SDK). `.github/workflows/release.yml` builds a universal ad-hoc-signed `.app` on GitHub's macOS runner (built successfully for v0.1.1; never launched on a Mac).
  - Release packages (v0.1.1, all built and install-checked on GitHub): Linux tarball, `.deb` (installed and removed on Ubuntu), `.rpm` (installed and removed in a Fedora 41 container), AppImage, Flatpak bundle, macOS `.app`. They are workflow artifacts, not yet published as a GitHub Release.
  - Pending: a Windows build in the release workflow; publishing a GitHub Release page; macOS and real-Linux-hardware runs; native PipeWire/JACK back-ends.
- [x] Lock-free primitives: `rtrb` SPSC queue, triple-buffer (`basedrop` not used: the garbage queue is kept, see below)
  - Done: `rtrb` command queue + garbage queue (old songs freed off the audio thread); `assert_no_alloc` guard on `process()` in debug builds and tests.
  - Done (2026-10-21): triple buffer (`triple.rs`) for the engine -> UI state: playhead, transport, master and track meters, block size and output delay are published together after every audio block (`EngineState`, read with `Engine::state()`), so the UI never mixes two blocks. Tested for torn reads across threads and under miri (`cargo xtask check` now runs miri; CI installs nightly + miri).
  - `basedrop`: not used on purpose. The garbage queue returns old songs/notes/tempo maps to the UI thread to be freed, with a credit system so the queue can never overflow; it is tested in `state_reclamation` and `queue_tests`.
  - Checked 2026-10-21: Windows (`cargo xtask check`, all 7 steps; app start and 24-track bench); Linux x86_64 compiles (engine clippy `-D warnings`, whole app via `cargo zigbuild`) but tests/audio not run there; macOS only in CI.
- [x] Logging `tracing`, crash handler `human-panic`, `metrics` for DSP load
  - Done: `tracing` in the engine; `tracing-subscriber` in the app (`RUST_LOG`, default `warn,prowave=info`); `human-panic` crash reports (release builds).
  - DSP load: each audio callback is timed against its real-time budget and the peak is published through a lock-free atomic (`Shared::take_dsp_load`); the LCD CPU meter shows it (red at >=90%, % on hover). Atomics are used instead of the `metrics` crate (no exporter needed yet).
- [x] Basic `cpal` playback: sine wave, measure output latency. `prowave-engine` example `beep` (true round-trip latency still needs a loopback cable)
  - Done: `beep` plays 440 Hz through the engine and reports device output latency (output side only; true RTL needs a loopback).
- **DoD:** `cargo run -p prowave-engine --example beep` plays 440Hz <20ms on all OSes (Windows met; macOS and Linux not measured yet)
  - Status (2026-10-21): MET on Windows with the new low-latency path. Pro Wave opens its own WASAPI shared-mode stream through `IAudioClient3` (`wasapi.rs`, the smallest period the audio engine allows, MMCSS "Pro Audio" thread) and falls back to cpal if the device cannot do it (`PROWAVE_NO_LOW_LATENCY=1` forces cpal). On the development PC: 32 ms -> 12 ms reported output latency, 0 xruns (the driver's smallest period there is 480 frames, so a driver that supports 128 or less will do better). ASIO is still built but untested: no ASIO driver or SDK on the development PC. macOS/Linux not measured yet (Linux/ALSA should reach 128-frame buffers with real-time priority).

### Phase 1: Audio Engine Core + Track System (Weeks 5-10)
- [x] Transport: sample-accurate clock, tempo map (tempo + time signature automation), SMPTE sync
  - Done (engine): `prowave-engine/src/tempo_map.rs` — tempo events (5-990 BPM, quarter-note tempo like Logic) and signature events, beat<->seconds, bars/beats; the transport advances through tempo changes per sample, audio clips are positioned by real time, the metronome follows the signature (accent on bar 1). Sent with `Command::SetTempoMap` (old map freed off the audio thread).
  - App: the project has one tempo and one time signature, both saved in `.prowave`. The LCD edits them (drag the tempo, or the signature's numerator and denominator); bar/beat display, region placement, grid lines and snapping follow the signature in quarter notes, so 6/8 and 7/8 line up with the engine.
  - Tempo track (2026-10-20): Track > Show Tempo Track shows a global Tempo track above the tracks. Double-click adds a tempo change (snapped to the grid), drag a point to change its tempo or move it, double-click or right-click > Delete Tempo Change removes it. Saved in `.prowave` (`tempo_changes`, older files open with none), undoable, sent to the engine as a `TempoMap`; the LCD shows and edits the tempo at the playhead; imported audio gets its length from the tempo map.
  - Marker track (2026-10-20): Track > Show Marker Track; double-click to add, click to jump, drag to move, double-click to rename, right-click for colour and Delete. Navigate > Create Marker (at the nearest bar), Delete Marker, Go to Next / Previous Marker. Saved in `.prowave` (`markers`).
  - Signature track (2026-10-21): Track > Show Signature Track. Double-click adds a change at the nearest bar (with the signature in force there), double-click a block or right-click > Edit Signature… sets numerator and denominator, drag moves it to another bar, right-click > Delete Signature Change. The LCD shows and edits the signature at the playhead. Regions sit on bars and follow the changes (`Project::bar_to_beats` / `beats_to_bar`, tested against the engine's `TempoMap` for mixed signatures); the ruler, grids, Piano Roll bar lines, Event list positions, Go to Next Bar and recording follow them too. Saved in `.prowave` (`sig_changes`, older files open with none), undoable.
  - Tempo curves (2026-10-21): right-click a tempo point > Ramp to Next Tempo Change (or Hold Tempo Until Next Change); the line between the two points is straight and the engine integrates it (`TempoCurve::Linear`). A change added inside a curve continues it; the control bar's tempo shows the value at the playhead. Saved in `.prowave` (`ramp`, `tempo_ramp`).
  - SMPTE (2026-10-21): the control bar's time follows the whole tempo map (changes and curves), a frame rate (24, 25, 29.97, 29.97 drop frame, 30) and a start time (right-click the time). MIDI Time Code out: quarter frames from the audio clock plus a full frame when the playhead jumps, to a chosen MIDI port. Follow MIDI Time Code: the transport starts, jumps (full frame, or more than 0.06 s off) and stops with the time code arriving on a chosen input port ("chase": it is not sample-locked or resampled). Tested with generator-to-decoder round trips; not tried with real hardware. Not done: MIDI Machine Control, MTC with 29.97 non-drop labelled as such (it goes out as 30).
- [x] Pattern and Drummer regions (2026-10-21): Track > New Pattern Region / New Drummer Region, or double-click an empty stretch of a Drummer track. A Pattern region loops a grid of steps (1-64 steps, 1/4 to 1/32 and triplets, swing, gate, up to 16 rows, velocity per step) edited in the bottom panel's Step Sequencer tab. A Drummer region plays Rock, Pop, Funk, Jazz, Hip Hop or Electronic with a Complexity/Loudness pad, swing, fills, kit pieces on or off and variations; it follows any signature. Both make ordinary notes (the same settings give the same notes), remade when the region's length or the bars under it change; Convert to MIDI Region makes them editable in the Piano Roll. Not done: per-step Chance, Tie, Repeat and loop direction, the Drummer following a Groove Track, and per-bar Drummer edits. The Drummer is algorithmic and was judged by its printed grids and tests, not by ear.
- [x] Track types: Audio, Instrument, Aux, Master, VCA, Folder/Summing Stacks, External MIDI
  - VCA (2026-10-21): Track > New VCA Track. A VCA track is a fader with Mute and Solo (purple strip in the Mixer, filter Master/VCA); a track's Group slot picks the VCA that moves it. The engine gets each track's level plus its VCA's, and Mute/Solo from the track or its VCA (`Project::track_mix_of`). Assignments follow tracks that are inserted or deleted; saved in `.prowave` (`vca`).
  - External MIDI (2026-10-21): Track > New External MIDI Track. The track's regions send Note On/Off to a MIDI output port and channel chosen in the Inspector (`midir`, own sending thread, `MidiOutEvent` queue from the audio thread); the engine makes no sound for it, stopping or seeking releases held notes, and live keyboard notes go out too. Saved in `.prowave` (`midi_port`, `midi_channel`). Notes are sent when the thread sees them (about 1 ms after the audio block), not at their exact sample. Not tried with a real synthesizer yet. Not done: MIDI clock, program change, controllers (the sustain pedal is not sent).
- [x] Region system: Audio regions (backed by `symphonia` decoder wav/aiff/caf/mp3/flac), MIDI regions, Pattern regions, Drummer regions
  - Done (2026-10-21): MP3 and CAF decode like the other formats (an MP3 without a length in its header is measured by decoding it once); drag them onto the window. Tests use small files in `crates/prowave-engine/tests/fixtures`. Pattern and Drummer regions: see below.
- [x] Non-destructive editing: clip gain, fade in/out with curves (equal-power, linear, S-curve), crossfade, Reverse, Normalize (non-destructive)
  - Done (engine, 2026-10-20): clip gain, beat-based fades, and reverse playback via `Engine::stream_clip_with`; `peak_of` for Normalize.
  - Region end timing is cached between song/map/transport changes; completed fades skip
    curve evaluation. Tests verify fade output after tempo and song changes without allocation.
  - Done (app, 2026-10-20): Region Inspector Gain, Fade In/Out (1/16-note steps, shown in ms), Curve, Reverse; right-click Normalize Region Gain (non-destructive, peak to 0 dBFS) and Reverse Region; waveform drawn with gain, reversed, fades shaded; saved in `.prowave`. Pending: none from this group (see below).
  - Done (2026-10-21): Fade tool (Tracks area tool menu): drag the left half of an audio region for a fade in, the right half for a fade out, in 1/16 notes. Crossfades: Drag: X-Fade (or the Inspector's X-Fade box): a region dropped on an earlier audio region fades in over the overlap while the earlier one fades out, with the region's curve (`Project::effective_fades`; the engine just plays the longer fades); drawn as fades on both regions; saved in `.prowave` (`crossfade`).
- [x] Disk streaming: memory-mapped + prefetch thread, 1000 tracks streaming test
  - Done (engine): one background decoder/prefetch thread and per-clip lock-free rings; ring
    capacity reduced to about 64 KiB (170 ms at 48 kHz) per clip, down from about 512 KiB.
  - Reverse streaming reuses decoded windows with 8,192 extra source frames of look-behind,
    avoiding decoder seeks on every output block; verified across cache boundaries and seeks
    at matching, higher, and lower output sample rates.
  - Disk registration work is limited to eight requests per worker pass so active streams
    get refills during import bursts; canceled requests are discarded before opening files.
  - Done (2026-10-21): `MAX_TRACKS = 1024` (track numbers are `u16`, per-track state on the heap). `cargo test --release -p prowave-engine --test thousand_tracks -- --ignored --nocapture`: 1000 stereo streams for 20 s on the development PC, 0 underruns, 37% render load (worst callback 3.6 ms of 5.3 ms); the app builds its interface for 1000 tracks in about 1.9 ms per frame (`PROWAVE_BENCH_TRACKS=1000 prowave --bench file`).
  - Memory-mapped PCM (2026-10-21): WAV and AIFF/AIFF-C with 16/24/32-bit integer or 32/64-bit float samples are read straight from a memory map (`mapped.rs`); everything else, and any file with an unusual or unfinished header, goes through the decoder. The samples are bit-identical to the decoder's (tested for every format). Measured on the development PC: no speed gain (10 minutes of 24-bit stereo read in 245 ms either way, random seeks 32 µs against 22 µs), but 1000 streaming tracks use 144 MB of private memory instead of 182 MB (the Windows working set also counts the mapped file pages, 3.8 GB for the test's files, which the system can drop). A clip with nothing to read lets go of its map after 1 s, so Windows lets other programs overwrite the file; a file that shrinks while mapped (a crash on Unix) is noticed within 250 ms and read by the decoder instead. `PROWAVE_NO_MMAP=1` turns it off. The 24-track/60-second stream check passes.
- [ ] Project file `.prowave`: zip with `project.json` (rkyv), `media/`, `bounces/`
- **DoD:** Import 24 tracks 48k/24-bit, play without dropouts

### Phase 2: MIDI & Sequencer (Weeks 11-18)
- [ ] MIDI 1.0/2.0 parser, MPE, Articulation ID, SysEx
  - Standard MIDI Files (2026-10-21): File > Import > MIDI File… (or drop a `.mid` on the window) makes a track for each track and channel (channel 10 becomes a Drummer track) with one region of its notes and the sustain pedal, and takes the file's tempo and signature changes when the project has no regions yet. File > Export > Selection as MIDI File… writes the selected region, or all MIDI, as format 1: a tempo track (tempo curves as a step for each quarter note, signatures) and a track for each MIDI track, with what plays (region transpose, quantize, mute, MIDI FX). Reading and writing round-trip in tests, damaged files are refused without panicking. Not done: SMPTE-timed files, program changes, other controllers, pitch bend, lyrics and markers, MPE, SysEx, MIDI 2.0.
- [ ] Piano Roll: full Logic parity (Quantize, Velocity, Brush, Time Handles, MIDI Draw for CC, Scale Quantize, Humanize, Flam, Note Color)
- [ ] Score Editor v0: render notes with `staff` crate, basic printing, MusicXML export
  - Score v0 (2026-10-21): the editor's Score tab shows an ordinary MIDI region as notation on a grand staff (treble from middle C up, bass below), in C major with sharps, each note as the nearest plain or dotted note value with stem, flag, ledger lines and dots; click to add a quarter note at the nearest eighth, right-click a note to delete it. The clefs are the letters G and F, not the clef shapes (the bundled fonts have no music symbols). Upgraded the same day: drawn clefs, the key signature (a key picker, 30 keys), the time signature, accidentals that carry through a bar, rests, and ties for notes that cross a bar line or are not one note value long. Not done: beams, chords' stem sharing, voices, printing, MusicXML, and editing notes' lengths.
- [x] Hardening pass (2026-10-21) of the newest features: MIDI file import is bounded (128 MB, 4096 tempo and 1024 signature changes, tracks up to the engine's 1024, regions inside bar 9999, note pairing in linear time), pattern regions stop at 250 000 notes and a MIDI FX chain at 1 000 000, MTC positions cannot overflow, the MIDI and MTC input queues are bounded, and generated regions are only compared with their bars when the region or the song changed. Tests: headless pointer tests for the Pattern grid, Drummer pad, Score and MIDI FX window (`ui_test.rs`), a hostile-file test, and Linux and macOS compile checks (the tests themselves run on Windows here and on all three in CI).
- [x] Step Sequencer: Pattern region with rows, per-step Chance, Repeat, Tie, Gate, Loop Direction
  - Done (2026-10-21): Pattern regions with rows, steps, rate, swing, gate and per-step velocity, Chance (percent, the same every time for a seed; New Variation changes it), Repeat (1-4 notes in a step, click to count up) and Tie (carries on the note before it, also over the loop), and a play direction (Forward, Backward, Ping-Pong, Random). The Edit box above the grid picks what a click sets. Pending: automation per row, per-row gate.
- [ ] Event List, Step Editor, MIDI Transform (conditions + operations), MIDI FX chain (Arp, Chord Trigger, Scripter via Rhai)
  - MIDI FX (2026-10-21): Track > MIDI FX… opens a chain of up to 8 effects for the selected track: Transposer, Velocity Processor, Scale Quantizer, Chord Trigger (8 chords), Note Repeater, Arpeggiator (Up, Down, Up and Down, Random, As Played; 1-4 octaves; gate) and Randomizer (pitch, velocity, timing; the same every time). Each can be switched off, moved and removed; they change the notes the engine plays (and what Export MIDI writes) and leave the regions as recorded. Saved in `.prowave`. Added later the same day: Modulator (sine, triangle, saw or square to a MIDI controller or pitch bend of an External MIDI track) and Scripter (Rhai script, sandboxed, with an operation limit). The sustain pedal and the controllers now go out to External MIDI tracks. Not done: the MIDI FX slot in the channel strip, and showing the changed notes in the Piano Roll.
  - MIDI Transform (2026-10-21): Piano Roll > Functions > MIDI Transform… selects notes by pitch, velocity, position or length and operates on them (transpose, velocity, position, length, scale, randomize), with presets (Transpose, Fixed Velocity, Legato, Humanize, ...). Not done: conditions on other regions' notes and saving your own presets.
- [ ] Drummer Track v0: 2 drummers, Complexity/Loudness X/Y, Follow Track, Convert to MIDI
- **DoD:** Create Drummer + Bass (MIDI) + Keys, quantize, bounce

### Phase 3: Flex & Smart Tempo (Weeks 19-24) — Logic's secret sauce
- [ ] Smart Tempo Editor: detect tempo from audio (✅ one region, steady tempo), edit downbeats, multitrack analysis
- [ ] Flex Time: Implement 5 modes - Slicing (use `aubio` onset detection), Rhythmic, Monophonic (use `rubberband` or `signalsmith`), Polyphonic (Elastique-like via `rubberband`), Tempophone, Speed
- [ ] Flex Pitch: Pitch detection (YIN via `aubio` or `pitch-detection` crate), per-note editing, Gain, Drift, Vibrato, Formant
- [ ] Groove Track: master track that other tracks follow (extract groove, apply)
- [ ] Audio Track Editor integration
- **DoD:** Import vocal, Flex Pitch correct to scale, Flex Time quantize to groove

### Phase 4: Mixer & Channel Strip (Weeks 25-30)
- [ ] Mixer UI: faders, pan (stereo pan with left/right puck), inserts (15 slots), sends (8 slots, pre/post), VCA, Groups, Sends on Fader
- [ ] DSP: Channel EQ (8-band with FFT analyzer - use `rustfft` + `egui` plot), Compressor (6 models: Platinum, VCA, FET, Vintage VCA/FET/Opto with sidechain), Gate, Limiter, Overdrive
- [ ] Automation: Track vs Region, Modes Read/Touch/Latch/Write/Trim, Bezier curves, Automation Quick Access, Sample-accurate parameter automation
- [ ] Bus summing: 64-bit double, PDC full, Low Latency Mode
- **DoD:** Mix 24 tracks with EQ/Comp on each, automation, PDC verified null test

### Phase 5: Instruments - Part 1 (Weeks 31-40)
- [ ] Sampler (EXS24 clone): Zone/Group editor UI, Velocity layers, Round Robin, Mod Matrix 10 slots, Filter (LP/HP/BP), LFOs, Import SFZ/EXS
- [ ] Quick Sampler: drag-drop, Slice/One-Shot/Classic, auto-slice by transient/beat, Record, Trim
- [ ] Drum Machine Designer: 48 pads, each pad = Quick Sampler + strip
- [ ] Drum Kit Designer: acoustic kits with multi-mic (Close, Overhead, Room, Bleed controls)
- [ ] ES2: Vector synth clone - 3 osc (Saw, Pulse, Sine with FM), Filter, Distortion, Vector Envelope, Router 10 slots. Must match Logic presets byte-for-byte parameter wise
- [ ] Vintage Series: B3 Organ (drawbars 9 + percussion + Leslie with Horn/Rotor speed, distortion), Vintage Clav (D6 pickup), Vintage E-Piano
- [ ] Retro Synth: 4 engines Analog/Sync/Wavetable/FM in one UI
- **DoD:** Load Sampler instrument, play with MIDI, patch saving

### Phase 6: Instruments - Part 2 & FX Suite (Weeks 41-50)
- [ ] Sculpture: physical modeling (string model with Object, Material, Pick, etc.)
- [ ] Alchemy-Lite: 2 sources (VA + Sampler), Morph pad, MSEG, Arp, Granular basic (for v1.0)
- [ ] Ultrabeat: 25 voices drum synth, built-in step sequencer
- [ ] Studio Strings/Horns: Articulation ID system (key switch, channel, velocity)
- [ ] FX full suite: Space Designer (convolution reverb with IR loading, envelope, filter, length), ChromaVerb (14 models), Delay Designer (visual tap editor, 26 taps, pitch/filter per tap), Tape Delay, Echo, Chorus, Ensemble, Flanger, Phaser, Tremolo, AutoFilter, Ringshifter, Distortion II, Bitcrusher, Overdrive, Pedalboard (35 pedals), Amp Designer (amps + cabs + mics), Match EQ, Multipressor, Exciter, SubBass, Voice Transformer, etc.
- [ ] Mastering Assistant: rule-based chain (EQ -> Comp -> Limiter) + LUFS targeting -14 LUFS, analyze using `ebur128` crate
- **DoD:** A/B test Channel EQ curve vs Logic (within 0.5dB), Space Designer IR load

### Phase 7: Plugin Hosting & Sandboxing (Weeks 51-56)
- [ ] CLAP host full (note, params, latency, state), VST3 host via `vst3-sys`, AU host on macOS via `coreaudio-sys`
- [ ] Out-of-process sandbox: separate process, SHM ring buffer (1ms latency overhead max), seccomp (Linux), seatbelt (macOS), AppContainer (Win). Heartbeat, crash isolation, auto-restart
- [ ] Plugin scanner with quarantine, timeout 5s per plugin, blacklist
- [ ] Plug-in Manager UI showing latency, type, manufacturer
- **DoD:** Load Vital, Serum, Kontakt without crashing main DAW if plugin crashes

### Phase 8: UI - The Logic Look + Live Loops (Weeks 57-66)
- [ ] Main window: Tracks area (with Track Headers, Track Inspector left, Library right), Mixer (separate window or bottom), Editors (Piano Roll bottom, Score, Audio Editor)
- [ ] Library: Patches (Channel Strip + Instrument + FX), save/load, search
- [ ] Smart Controls: 8 knobs per patch, mapping editor (map knob to multiple params with range scaling), Learn
- [ ] Key Commands: 1200+ commands, searchable, customizable, Presets, Conflict detection
- [ ] Live Loops: Grid (Cells, Scenes, Trigger modes, Quantize Start), Record Live Loops performance to Tracks, Remix FX (X/Y pad with Gate, Filter, Repeater, Reverse)
- [ ] Loop Browser: Apple Loops metadata parsing (key, tempo, style), drag-drop with auto tempo/key adaptation, All Files Browser
- [ ] Video Track: Movie Track with filmstrip, SMPTE, Detect Cuts, Export Audio to Movie
- **DoD:** 5 Logic users can do basic tracking/mixing without manual

### Phase 9: WASM, Collaboration & Polish (Weeks 67-74)
- [ ] Compile `prowave-engine` to WASM + AudioWorklet (128-sample blocks, SharedArrayBuffer + Atomics)
- [ ] `prowave-wasm`: OPFS for project storage, Web UI via `egui_web` (same egui code), Web Remote (like Logic Remote but via browser/WebSocket)
- [ ] Collaboration: Project file with Ed25519 signing, simple CRDT for arrangement (optional)
- [ ] Export: Bounce Project/Region/Track in Place (offline/real-time, normalize, dither Apogee UV22HR via `r8brain`), Export All Tracks as Audio, Export MIDI, MusicXML, AAF, ADM BWF (Atmos)
- [ ] Performance: SIMD (`wide`, `std::simd`), benchmarks vs Logic/Reaper
- **DoD:** Open in Chrome/Firefox, play 16-track project <30ms latency, bounce to WAV

---

## 4. Future Options / v2.0+ (Not in v1.0 but architect for it)

- **Dolby Atmos / Spatial Audio:** 7.1.4 beds, 128 objects, ADM BWF export, Binaural monitoring, 3D panner UI (like Logic's Atmos panner)
- **AI Session Players v2:** Bass Player, Keyboard Player, Drummer with style transfer (train small ONNX models in Rust via `burn` or `ort`)
- **Stem Splitter:** Demucs model via `tract` (separate Drums/Bass/Vocals/Other) — Logic 11 feature
- **AI Mastering Assistant v2:** Full AI chain with reference track matching
- **Cloud Collaboration:** Real-time collab with CRDT (y-crdt in Rust), comments, version history
- **Notation Pro:** Full engraving (like Dorico), MusicXML import/export advanced, tablature editor
- **MPE / MIDI 2.0 Full:** Per-note expression editing in Piano Roll, MIDI 2.0 Property Exchange
- **Modular Environment (Logic's Environment):** Visual patching environment for MIDI routing (like Max) — rebuild as node graph with `egui_nodes`
- **MainStage Integration:** Perform mode, Patch switching without glitch
- **Mobile / iPad:** iOS build via `winit` + Metal, touch-optimized UI, Apple Pencil for automation
- **Marketplace:** User patches, instruments, IRs sharing, with Rust-based package manager
- **Scripting:** User scripting via Rhai or WASM (like Scripter MIDI FX but for whole DAW)
- **ARA2 Support:** Celemony Melodyne-style integration for Flex Pitch

---

## 5. Acceptance Criteria for Claude

For each feature, Claude must produce:

1.  **Trait Impl:** `struct ChannelEq { ... } impl DspNode { fn process(&mut self, ...) { ... } fn latency(&self) -> usize { ... } }`
2.  **Param Layout:** Exactly match Logic's parameter names and ranges (e.g., Compressor Threshold -50 to 0 dB, Ratio 1:1 to 20:1). Use `nih-plug` params for compatibility
3.  **Preset Test:** Load Logic preset JSON (we will provide `logic-presets/` corpus) and verify output within 0.5dB or 1 cent pitch
4.  **No Alloc Test:** `#[test] fn no_alloc() { assert_no_alloc::assert_no_alloc(|| { let mut node = ChannelEq::new(); node.process(&inputs, &mut outputs); }); }`
5.  **WASM Build:** `cargo check -p prowave-dsp --target wasm32-unknown-unknown` must pass

---

## 6. Performance Targets

| Metric | Target |
|---|---|
| RTL @ 128 samples / 48kHz | macOS <8ms, Win ASIO <10ms, Linux JACK <8ms |
| 100 tracks + 200 plugins | CPU <70% on M1 Pro / i7-12700 |
| WASM | 32 tracks stereo in Chrome with Worklet |
| Engine alloc on audio thread | 0 per callback |
| PDC accuracy | Sample-accurate, null test -90dB |

---

Next step for Claude: Start Phase 0. `cargo new --lib crates/prowave-engine && cargo xtask init`
Then implement transport clock with `Transport { sample_rate: f64, tempo_map: TempoMap, position: SamplePosition }`


---

## 10. Official Logic Pro .logicx Support (New Requirement)

ProWave MUST open and save official Apple Logic Pro .logicx projects on Linux and macOS, while being super lightweight in pure Rust.

See `LOGICX_FORMAT_SPEC.md` for full byte-level spec for Claude.

**Summary:**
- .logicx is a macOS bundle (directory) with `Alternatives/000/ProjectData` binary blob (magic 23 47 C0 AB, reversed FourCC chunks like karT, gRuA, qeSM, GAME anchor for stock plugins, embedded JSON for Drummer)
- Parse via linear scan, no central header table, guard against false positives (printable ASCII, length caps)
- 5 record shapes: AU triple (3x FourCC), GAME (stock plugins), JSON (Drummer/Bass Player), Channel-strip records (16-byte name + 8-byte descriptor), Region records (0x61 0xFF + 24 NULs + u16 LE len)
- Write via seed synthesis method (from LogicProFormatWriter): embed 62KB donor seeds (audio_base.seed, mixed_base.seed, infra.json.gz), clone and patch records, don't hand-craft
- Linux: No AU, map Alchemy -> ProWave Alchemy-lite, Channel EQ -> ProWave EQ, etc. Show "replaced" UI
- macOS: AU hosting via coreaudio-sys, CoreAudio backend, Metal via wgpu
- Lightweight: <50MB binary, <150MB RAM idle, <400ms cold start vs Logic 1.2GB/1.5GB/3-4s, achieved via mimalloc + LTO + strip + lazy loading + rkyv zero-copy + memmap disk streaming

**Crate:** `crates/prowave-logicx-import` with public API `open_logicx(path) -> ProWaveProject` and `save_logicx(project, path)`

**Tests:** Empty project, Drummer project, audio+MIDI project, round-trip, fuzz
