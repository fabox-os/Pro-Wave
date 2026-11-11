# Hosted instrument plug-ins (VST3, CLAP, AU on macOS)

Status: approved design, implementation in progress (2026-11-11).

## Goal

Load an instrument plug-in on an instrument/MIDI track and play it from the timeline and live
MIDI. The plug-in runs out of process in the existing `prowave-plugin-host` helper, like hosted
effects. VST3 and CLAP work wherever the helper runs; Audio Units additionally on macOS.

Scope for the first version: one instrument per track, stereo output, MIDI channel 1, no
multi-timbral or multi-output routing, no MIDI learn.

## Why instruments need their own path

A hosted effect is a `HostedEffect`: stereo in, stereo out. An instrument has no audio input
bus, and it needs MIDI. Asking for a stereo main *input* bus is what produced
`the plugin has no main audio bus for the requested layout`. The instrument path configures no
audio input (`Layout::None`), a stereo output, and the chain's event input, and adds note
events.

## Components

### `prowave-plugin-host`

- `Catalog::instantiate_instrument(key, sample_rate, quantum, state)`: same chain/helper setup
  as `instantiate`, with `Layout::None` input, `[Layout::Stereo]` output and event routing.
- `RealtimeInstrument` implements `prowave_engine::HostedInstrument`:
  - one adapter quantum of preallocated stereo output, an `Events` buffer for the current
    quantum and one for the next;
  - `note_on`/`note_off` push `Event::note_on/note_off` at the sample offset already collected
    in the current quantum (or offset 0 of the next quantum when it is full), so note timing is
    sample-accurate within the helper's 128-frame block;
  - `process()` submits the quantum (empty audio input) and plays the output from `latency`
    quanta ago, with the same fallback and resync behaviour as effects;
  - parameter automation reuses the effect adapter's pending/automation mechanism;
  - `all_notes_off()` sends note-offs for the notes it started; `set_bypass` makes it silent;
  - reported latency is the helper latency plus one adapter quantum.

### `prowave-engine`

- New `HostedInstrument` trait (Send): `process`, `note_on`, `note_off`, `all_notes_off`,
  `set_param_norm`, `set_bypass`, `latency_samples`. Audio-thread contract: no allocation,
  lock, block or I/O.
- `EngineCore` holds one `Option<Box<dyn HostedInstrument>>` per track. New commands set and
  clear it; replaced boxes go through the existing garbage queue and are dropped on the UI
  thread.
- Note routing precedence: external MIDI → hosted instrument → SoundFont → built-in synth.
- A fixed `HostedNote` table tracks active notes. Timeline notes end at their end beat, live
  notes on note-off (with the same safety timeout as SoundFont notes), and stop/seek/cycle jumps
  send all-notes-off.
- The per-sample render adds the instrument's stereo output to the track input, gated by
  audibility, and its latency joins the strip's delay compensation.

### `prowave-app`

- `Track.hosted_instrument: Option<HostedInsert>`, saved with the project and separate from the
  effect `hosted` field.
- `Plugins` gains an instrument role: targets are (strip, effect | instrument), with separate
  control chains, parameter windows, restore, save and automation.
- The scan publishes effect and instrument lists separately.
- The mixer channel strip gets an Instrument slot above Audio FX on instrument/MIDI tracks. It
  opens the instrument picker (only instrument plug-ins), shows parameters on double-click or
  Open, and offers Replace/Remove/Bypass. The search field is focused when the picker opens,
  and clicking it does not close the menu (same fixes as the effect picker).
- `engine_sync` sends the instrument and its latency to the engine; project save/load includes
  its class key and state.

## Testing

- Engine unit tests with a fake hosted instrument: precedence over SoundFont and synth, note
  on/off for timeline and live notes, output routing into the strip, latency, all-notes-off on
  stop and seek, and no allocation in the render path.
- App tests: the instrument picker lists instrument plug-ins only; a project round-trip keeps
  `hosted_instrument`; effect hosting is unchanged.
- Host tests: event offset/latency unit tests, plus a manual probe that loads a real instrument
  (Surge XT) and checks that it produces audio.
