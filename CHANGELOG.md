# Changelog

What changed in each Pro Wave release. The release workflow copies the section of the version
being released into the GitHub Release notes, so keep one `## <version>` heading per release.

## Unreleased

### All systems (Windows, macOS, Linux)

✅ **15 effects per strip and 8 sends:** an Audio FX chain now holds 15 effects and a channel strip 8 sends, Logic's limits (they were 8 and 4). Older projects load unchanged; the mixer grows the insert and send rows as they are filled.

✅ **Time stretching and pitch shifting:** the new `prowave-stretch` crate changes a region's duration without changing its pitch (WSOLA with a waveform-similarity search and Hann overlap-add) and shifts pitch without changing duration (stretch by the pitch ratio, then resample back). It works on interleaved audio with any channel count, using one alignment for all channels so stereo stays in step. Tests cover length and pitch on sine tones, stereo channels, and bounded output. It is an offline algorithm for now; Flex Time and the app are not wired to it yet.

✅ **Vintage B3 organ:** the Library's Vintage B3 Organ patches now play an additive drawbar organ — nine drawbars (16' to 1'), percussion on the second or third harmonic, a key click and a Leslie rotor with chorale and tremolo speeds — instead of the two-oscillator approximation. Not done yet: a dedicated B3 panel, separate horn and rotor speeds, distortion, and drawbar automation.
✅ **Rename Project…:** **File > Project Management > Rename Project…** now opens Project Settings on its General tab, where the project's name is edited — the name the window title and a Save As default to. The entry used to be greyed out.
✅ **GarageBand projects (audio):** **File > Import > Other Projects…** takes a GarageBand `.band` bundle (the macOS folder) and adds an audio track for each of its audio files, read from the project's `Media/` folder. The project's tempo, signature and arrangement (MIDI regions, where each audio region sits) are not read yet — `GARAGEBAND-FORMAT-SPEC.md` has the plan — and an iPad/iPhone `.band` zip is recognised but says to unzip it first.
✅ **Space D is now Logic's Space Designer:** a convolution reverb with its own window (light grey, dark top bar, Sampled IR and Synthesized IR tabs, the response drawn with its Volume Envelope, and the orange controls below). Open it from the Audio FX slot or with **Mix > Space Designer > Show**. It has six built-in impulse responses (03.0s Long Reverb is the default), Volume Env and Filter Env, Init Level, Attack, Decay with LIN or EXP, End Level (drag the three blue handles or the values), IR Offset, Reverse, Quality, Predelay, Length, Size, X-Over with Lo and Hi Spread, Input, Dry and Wet, Output EQ, Compare, Copy, Paste, Undo, Redo and View (75 to 150 %). The reverb reports its 512-sample delay for compensation.

✅ **Space Designer for mastering:** Length now cuts the response and Size stretches it, as in Logic. The Filter Env is a real low pass with a Start and End cut-off and Resonance (click its name to edit it in the display). Output EQ is a 100 Hz low shelf and a 10 kHz high shelf with their gains (-2 and -1 dB at first), on the reverb only. Hi Spread goes to 200 %. The Synthesized IR tab has Density, Diffuse and Early Reflections. The Input fader has a level meter. New responses: 01.2s Medium Hall, 00.5s Small Room, Plate, Church and 02.0s Hall. Three mastering presets (Glue - Small Room, Plate - Vocal Sheen, Wide Hall) are in the preset menu and in **Mix > Space Designer**. Projects with the old four-slider Space D open with the same balance of dry and reverb.

✅ **Effects on the Stereo Out strip (the master bus):** the Stereo Out strip now has real Audio FX slots, with the same menu as a track strip (add, replace, bypass, move, remove; double-click opens the window). The whole mix goes through them before the master fader, and they are saved with the project. **Mix > Space Designer > Mastering Glue - Small Room** (and the other two presets) puts a Space Designer there and opens it. Gain reduction and Space Designer input meters work for it. The master effects have no automation lanes yet.

✅ **Record menu:** three entries that were greyed out now work. **Record Toggle** starts and stops recording (the same as the control bar's button), **Record Enable** arms or disarms the selected track's R button, and **Record Project Settings** opens Project Settings on its Recording tab.

✅ **Mix > Edit Groups…:** a window with every mix group in one list, where each group can be renamed and its links chosen (Volume, Pan, Mute, Solo). It shows how many tracks each group has, adds groups (up to 64, numbered automatically) and deletes them; the mixer strip's Group popup stays the way to put a track into a group.

✅ **Bounce:** **File > Bounce > Project or Section…** plays the song — or the cycle section, when the cycle is on — through the engine and writes what is heard into a 32-bit float WAV. Every effect (the master bus's included), send, automation lane, audio region and instrument is in the file, because the engine hands over its finished master frames. A bounce runs at playing speed; the transport and the cycle are put back where they were afterwards, and stopping the transport cancels it without writing a file.

✅ **Export > Track as Audio File…:** the selected track is bounced alone into a WAV. The other tracks are soloed off while it runs — the track keeps its effects, its sends and the buses they feed — and everyone's solo states are put back when it ends. It is the same real-time bounce as above, so it takes the song's length; a track with no regions is not exported.

✅ **Bounce in place:** **File > Bounce > Regions in Place…** (and **Record > Bounce In Place**) renders the selected MIDI region with its track's instrument, effects and sends, over the region's own span, and replaces the region with the audio where it was. It is one undo step, and the audio lives in the session's media folder until the project is saved, when it travels inside the `.prowave`.

✅ **Bounce and Join:** **Edit > Bounce and Join** renders all the selected regions of one track — from the earliest to the latest, the gaps included — and puts the audio back as one region, named after the earliest. Like Bounce in Place it is one undo step and the audio travels inside the `.prowave`.

✅ **Mix and Record menu entries:** **Mix > Show Automation** opens the Automation editor on the selected track, **Mix > Delete Automation** clears the lane it shows, and **Record > Record / Record Repeat** records from the cycle start when the cycle is on (otherwise from the playhead). All three were greyed out.

✅ **Hydrogen drum kits:** an instrument track can now play a free Hydrogen drum kit. In the Inspector, **Choose Hydrogen Drum Kit…** takes the kit's `drumkit.xml`; each instrument of the kit plays on its own MIDI note with its velocity layers and gain, and the samples (WAV, AIFF, FLAC or OGG) are read from the kit's folder. The kit is saved in the project by its path, like a SoundFont. Kits that name no note number their instruments from C1, as Hydrogen does.
✅ **Edit > Undo History…:** the Edit menu's entry opens a window listing the edits this session can go back to (Undo 1, Undo 2…) and the ones it can redo; click one to jump there. The entry was greyed out.

✅ **Send automation:** every send level now has an automation lane, like the fader, the pan knob and the effect parameters. Choose **Send 1**, **Send 2**… in the Automation editor's Parameter menu, draw its points there or record them in Write mode, and the lane plays back in Read and is saved in the project. Automation of the Output slot and two-handle Bezier curves are still to come.
✅ **Show in File Manager:** **File > Show in File Manager** (enabled once the project has been saved) opens Explorer with the project file selected, Finder with it revealed, or the folder that holds it with `xdg-open` on Linux.
✅ **File > Import > Audio File…:** the Import menu's audio entry now works — the same import that dropping a file on the window does. It adds a track with the file as a region at the playhead (WAV, AIFF, FLAC, MP3, CAF, OGG or M4A), computes the waveform in the background, and is one undo step.
✅ **SMPTE-timed MIDI files:** a MIDI file that counts time in SMPTE frames (24, 25, 29.97 or 30 frames a second, with its ticks a frame) now imports instead of being refused. Its absolute times become bars and beats at the project's tempo, so the recording lands where it sounds. Files with a broken frame rate or no ticks a frame are still refused.

✅ **MIDI markers:** a MIDI file's markers now import onto the Marker track at their bars, and exporting a project writes its markers back into the file. Names are kept up to 64 characters, and a file with more than 1024 markers keeps the first 1024.

✅ **The HD meter measures disk load:** the LCD's HD meter, empty until now, shows how much of the time the disk streaming thread spends decoding and copying audio regions (it turns red from 90 %, like the CPU meter). It reads zero when the project has no audio regions.
✅ **Command line help:** `prowave --help` now lists the options (`--play`, `--piano`, `--bench`, `--layout-shots`, `--version`) and `prowave --version` prints the version — both without opening a window. Previously the options were only in the source.

✅ **Multi-Band Compressor:** a new effect for any Audio FX slot. It splits the sound into Low, Mid and High with two Linkwitz-Riley crossovers (Low/Mid 250 Hz and Mid/High 3 kHz at first, both adjustable), and compresses each band with its own Threshold, Ratio and make-up Gain; Attack and Release are shared. With no band compressing the three bands add back to the original sound. The strip's gain-reduction meter shows the busiest band, and every parameter can be automated.
✅ **File > Export > Score as MusicXML…:** writes the project's notes as MusicXML 4.0, one part for each track that has notes, with measures following the time-signature map, the key, the starting tempo, chords, rests that fill each bar and ties for notes that cross a bar line. The entry is enabled when the project has notes. MusicXML import, beams and voices are not done.
✅ **MIDI program changes:** a MIDI file's program changes are kept. On import the last program of each channel goes to the track's MIDI Program; on export an External MIDI track writes its program at the start, so a round trip no longer loses it.
✅ **Edit > Paste Replace:** pastes the copied regions at the playhead like Paste, after removing the regions on the selected track that the pasted span covers. A region that only overlaps the span partly is removed whole. The entry was greyed out.

### Fixed

✅ **Mixer on short windows:** on a 768-point-tall screen the Mixer panel was too short for a channel strip and cut off everything under the Pan knob. The panel is now never shorter than one whole strip (it follows the project's Audio FX slots and sends), and still no taller than the room there is.

### Still to do or test

❌ MusicXML import; GarageBand tempo, signature and arrangement (see `GARAGEBAND-FORMAT-SPEC.md`); Hydrogen kits and the new menu entries (Import Audio File, Show in File Manager, Undo History) were tested by automated tests on Linux only, not on Windows or macOS

❌ Space Designer: loading your own impulse response files (the six responses are made in code, none is shipped as a WAV); tempo-synced Predelay (the note button is only drawn); Quality Low cuts the highs above 8 kHz and the silent tail, it does not run at half the sample rate

## 0.2.2

Tested on Ubuntu 26.04.1 LTS (x86-64, kernel 7.0.0-42): the full project check (formatting,
clippy with warnings as errors, all tests, real-time allocation check, miri, cargo-deny and the
wasm build), the layout screenshots, and the MIDI tests through real ALSA ports. No audio device
and no MIDI hardware were used. Windows and macOS builds are not tested in this release.

### New: instruments and effects

✅ **Amp Designer:** a guitar and bass amplifier in any Audio FX slot. Five models (Clean Combo, Tweed, British Crunch, Plexi Lead, Bass Amp), tone stack, three speaker cabinets or none, and a microphone position from the edge of the cone to its centre. It reports its delay to plug-in delay compensation.
✅ **SoundFont (.sf2) instruments:** an instrument track can play a preset from a SoundFont file (General MIDI and drum kits), chosen in the Inspector. Synthesis is pure Rust (no native library).
✅ **Apple Loops tags:** the Loop Browser reads the category, genre, key and beat count of Apple Loops (`.caf`) and shows them under each name; search finds loops by them.

### New: editing

✅ **Cut, Copy and Paste regions:** Edit menu. Paste puts a copy on the selected track at the playhead, keeping the distances between several regions. Audio regions paste only onto audio tracks.
✅ **Split at Playhead:** cuts every selected MIDI region under the playhead. A note that crosses the cut ends there.
✅ **Select All and Join:** Edit > Select All selects every region. Shift or Command click adds or removes one, and dragging a selected region moves the whole selection. Join merges the selected MIDI regions of one track into one region, keeping every note in place.
✅ **Functions menu:** Move to Playhead, Quantize (the notes of selected MIDI regions go to the region's Quantize grid, 1/16 when Off), and Folder (puts the selected track into a track stack).
✅ **Track menu:** Rename Track, Assign Track Color (16 swatches), Assign Track Icon (15 icons or Automatic), Configure Track Header (hide the number, icon, name, M/S/R/I buttons, volume slider or pan knob), and Show Master Track (a Master strip with the master fader under the tracks).

### New: project and transport

✅ **Project Settings** (File > Project Settings): General (project name), Synchronization (SMPTE frame rate, start time, MIDI Time Code and MIDI Clock ports), Metronome (click volume and accent of the first beat), Recording (1 to 4 bars of count-in), Tuning (reference pitch A4 from 427 to 453 Hz), Audio (output device and sample rate, read only), MIDI (input port for live notes, and Send MIDI Panic). All of it is saved with the project.
✅ **Follow MIDI Clock:** Synchronization > Follow MIDI Clock: the song starts, continues, stops and jumps where a MIDI clock input says, and its tempo is the clock's.

### New: MIDI

✅ **MIDI outputs:** an external MIDI track sends its bank and program when it is routed (Inspector: MIDI Program, MIDI Bank). Synchronization > Send MIDI Clock sends Start, Continue, Stop, Timing Clock and Song Position to a port. Send MIDI Panic silences every note on every output the tracks use.
✅ **Pitch bend, channel pressure and SysEx in regions:** a keyboard's pitch bend and channel pressure are recorded; MIDI files keep them and their SysEx messages, both ways; external MIDI tracks send them (bend at 14 bits, each SysEx message whole).
✅ **Event list:** shows pitch bend with its full value, channel pressure as Aftertouch, and each SysEx message as one row. **SysEx…** adds a message typed in hex at the playhead.

### New: metronome and tuning

✅ The metronome level and accent, the count-in bars and the reference pitch are now real engine settings (`SetClick`, `SetTuning`), not only displayed.

### Fixed

✅ **Denormal numbers on the audio thread** are flushed to zero on x86-64, so a fading reverb or filter tail no longer causes CPU spikes (contributed by SakaBoluwatife).
✅ **Missing interface texts:** ten texts of the Amp Designer, the Guitar Amp category and the SoundFont inspector were missing from every translation, so the translation test failed. Added to all 16 languages.
✅ **Stuck MIDI notes:** a note still sounding when its track was moved to another port or channel was never released; it is now released on the port it started on.
✅ **Missing clock port:** a followed MIDI clock port that is not there is looked for every few seconds, not every frame; clearing it no longer keeps its name for the next choice.
✅ **Capture Recording** keeps the sustain pedal, pitch bend and channel pressure (it used to drop the pedal).
✅ **Master strip** no longer shares a widget ID with the transport fader.

### Known limitations

❌ **Not tested with hardware:** no real MIDI synth, clock source, audio interface or plug-in was used. The MIDI tests use ALSA virtual ports.
❌ **Plug-ins:** CLAP, VST3 and AU hosting does not exist yet. The Plug-in Manager is greyed out.
❌ **Bounce:** Bounce in Place and Bounce and Join need offline rendering, which the engine does not have yet.
❌ **Convert to New Sampler Track:** needs a sampler instrument, which the project does not have yet.
❌ **Chord and Arrangement tracks:** Show Chord Track and Show Arrangement Track need data that does not exist yet.
❌ **Split and Join on audio, Pattern, Drummer and take-folder regions:** not done. Audio needs a start offset into the file in the engine.
❌ **Folder:** the Functions menu's Folder creates a track stack, our reading of Logic's "Folder".
❌ **Audio settings:** the output device and buffer size cannot be chosen yet. Project Settings > Audio shows them read only.
❌ **MIDI:** poly aftertouch is not read; SysEx cannot be edited byte by byte and is not made for audio, Pattern or Drummer regions; pitch bend and SysEx do not reach the internal instruments; a seek while stopped sends no clock, and while playing it sends a Song Position; a MIDI clock output on the same device as the followed input would echo back.
❌ **Follow MIDI Clock:** the Play and Stop buttons still work next to a followed clock; a sudden tempo change takes about two beats to follow.
❌ **Master strip:** it has no automation lane yet.
❌ **Apple Loops:** the older AIFF-based loop format is not read; the tag reader was built from community notes on the format, not from Apple's files.
❌ **Translations:** the texts added in this release are machine-written and need a native-speaker review.
❌ **Linux graphics fallbacks:** still to be confirmed by a tester with an NVIDIA card; audio recording and sound on real Linux and macOS hardware.

## 0.2.1

### All systems (Windows, macOS, Linux)

✅ **More audio formats:** Pro Wave now opens Ogg Vorbis (`.ogg`, `.oga`), M4A and MP4 (AAC and ALAC) files, and ADPCM audio, besides WAV, AIFF, FLAC, MP3 and CAF.
✅ **Recording tells you when the input fails:** if the microphone or audio interface is unplugged or taken away by the system while you record, the take now says "Part of the recording was lost" instead of ending quietly.

### Linux

✅ **Graphics problems at launch:** when the window can't be drawn, Pro Wave now tries up to three more ways by itself before giving up: Vulkan, then the X11 window system when running under Wayland, then software OpenGL. This is for NVIDIA cards (reported on Ubuntu 26.04 with an RTX 5060) and other machines where OpenGL starts but can't draw to the window.
✅ **Wayland:** the window now has the same name as its menu entry (`prowave-studio`, or the Flatpak's own name inside a Flatpak), so the desktop shows Pro Wave's icon and groups its windows.

### Still to do or test

❌ Linux: the graphics fallbacks are written but not yet confirmed by a tester with an NVIDIA card; audio recording and sound on real Linux and macOS hardware
❌ Audio recording: cycle takes, punch in and out, amp and pedal effects for guitar and bass
❌ Apple Loops: reading their tags and tempo (the audio formats they use now open)

## 0.2.0

### All systems (Windows, macOS, Linux)

✅ **Audio recording:** record your voice or an instrument. Click the Input slot of an audio track's channel strip to pick the input (mono or a stereo pair) and, under *Input Device*, the audio interface or microphone. Press the track's R button (its meter then shows the input level), turn Record on and press Play: a red region grows while you record and becomes the waveform when you stop. Several armed tracks record their own inputs at the same time. The recording is saved inside the project, and it is lined up with the song by the audio delay the system reports.

✅ **Hear your input:** the I button on an audio track plays its input through the track's channel strip, with its effects. Use headphones to avoid feedback.
✅ **Count-in works:** with Count-in and Record on, Play gives one bar of clicks and then starts recording.
✅ **Capture Recording:** the button next to Record turns what you just played on a MIDI keyboard while the song was running into a region, without having recorded it.
✅ **The R button on instrument tracks** now picks the track MIDI records on.
✅ **Mixer:** the Setting slot copies, pastes or resets a channel strip, and the Output slot sends a track to Stereo Out or to a bus.
✅ **New panes:** Quick Help, a Toolbar, a Note Pad saved with the project, a Loop Browser for a folder of your choice, and All Files. Double-click a file in a browser to add it.
✅ **Window menu:** the Record entry is gone; it switched Record on instead of opening a window.

✅ **No more demo project:** Pro Wave now always starts with a clean project, and Demo Song is gone from Project Templates.

### Linux

✅ **Fixed a crash at launch** with the error "no graphics adapter can draw to this window", reported on Ubuntu 26.04 with an NVIDIA card: when OpenGL can't draw to the window, Pro Wave now restarts itself on Vulkan.

✅ **A project's audio file that is cut shorter while it plays** now carries on from the exact place, not from the start of the block it was in.

### Still to do or test

❌ Audio recording: cycle takes, punch in and out, amp and pedal effects for guitar and bass
❌ Mixer: automation of sends, Bezier curves with two handles, plug-in (CLAP / VST3 / AU) hosting, a multi-band compressor
❌ Flex Time: dragging Flex markers with the mouse, Flex Pitch per note (drift, vibrato, formant), Groove Track for audio tracks to follow
❌ Smart Tempo: Adapt Region Tempo while recording, a tempo that is not steady in the Editor's beat grid

## 0.1.10

### All systems (Windows, macOS, Linux)

✅ **Detect Tempo:** right-click an audio region > *Smart Tempo > Detect Tempo* says its tempo, *Adapt Project Tempo to Region* makes it the project's tempo. The file is read in the background and the onsets (where hits and notes start) are found along the way, ready for Flex Time.

✅ **Smart Tempo Editor:** right-click an audio region > *Smart Tempo > Smart Tempo Editor…* shows the beats found over the waveform; ×2 and ÷2 fix a tempo read at half or double, the arrows move the downbeat. *Adapt Project Tempo* makes the project follow the beats, including tempo changes inside the recording, and keeps every audio region at the same time. *Adapt Project Tempo to All Audio Regions* hears all the audio together (multitrack), and *Region Follows Project Tempo* moves the region's beats onto the project's.
✅ **Flex Time:** right-click an audio region > *Flex Time*: pick a mode (Slicing, Rhythmic, Monophonic, Polyphonic, Speed, Tempophone), *Quantize Audio to 1/16 or 1/8* puts every hit on the grid, *Remove Flex Edits* goes back. The recording is never changed; the edit is saved with the project.
✅ **Flex Pitch:** *Flex Pitch > Correct Pitch to Key* moves every note it hears to the nearest note of the project's key, and *Create MIDI Track from Flex Pitch* writes the notes it hears on a new software instrument track.
✅ **Groove Track:** right-click a track > *Set as Groove Track*, then *Follow Groove Track* on MIDI tracks: they take its timing (swing and push), from its notes or, for audio, from the hits in its recordings.

✅ **Real effects:** each Audio FX slot of a channel strip now holds an effect that changes the sound: Channel EQ, Compressor, Space D (reverb), Tape Delay, Chorus, Phaser, Overdrive and Filter. Click a slot to add, replace, move, bypass or remove one; double-click it for its window (sliders, the EQ curve, the compressor's gain reduction). The strip's EQ thumbnail and Gain Reduction meter show what the effects do.
✅ **Buses and sends:** a strip has up to four sends (pre or post fader, with a level knob) to Bus 1-8, and Aux strips play the bus of their number (Aux 1 plays Bus 1). *Sends on Faders* makes the faders set the sends to one bus. *Edit > Copy/Paste Channel Strip*, and *Options > Create New Auxiliary Channel Strip* and *Create New VCA*.
✅ **Automation:** the strip's Automation slot picks Off, Read, Touch, Latch or Write for the fader and the pan. Move a control while the project plays to write it; Read plays it back (the fader moves by itself). *Show Automation* opens the editor: click to add a point, drag to move, double-click to delete. Saved in the project.

✅ **Noise Gate, Limiter and Multi-Band EQ:** three more effects for the Audio FX slots. The Limiter never passes its ceiling (it looks 2 ms ahead); the Multi-Band EQ has six bands (low shelf, four bells, high shelf) and draws its curve in its window and in the strip's EQ thumbnail; the Gain Reduction meter shows the Gate and the Limiter too.
✅ **Delay compensation:** an effect that looks ahead (the Limiter) no longer puts its track out of time: every other strip, and what is sent to buses, is delayed by the same amount.
✅ **Better sums:** the strips and the buses are added in 64 bits, so many tracks add up without losing quiet detail to rounding.
✅ **Automation of effect settings and curves:** move any slider of an effect while the project plays to write it (Touch, Latch, Write), and in Read mode it moves by itself; pick the setting in the Automation editor's *Parameter* menu. A handle in the middle of each stretch of a lane bends it (double-click to straighten).
✅ **Mix Groups:** the strip's Group slot makes or picks a group (*Options > Create Group…* too). Tracks in a group move their faders and pans together by the same amount, and Mute and Solo follow, as the group's settings say. Saved in the project.

### Still to do or test

❌ Mixer: automation of sends, the Output slot, Bezier curves with two handles, plug-in (CLAP / VST3 / AU) hosting, a multi-band compressor
❌ Flex Time: dragging Flex markers with the mouse, Flex Pitch per note (drift, vibrato, formant), Groove Track for audio tracks to follow
❌ Smart Tempo: Adapt Region Tempo while recording, a tempo that is not steady in the Editor's beat grid

## 0.1.9

### All systems (Windows, macOS, Linux)

✅ **Flatpak can reach the network:** Check for Updates failed with "failed to lookup address information" in the Flatpak, whose sandbox had no network access.
✅ **Close Project:** *File > Close Project* now closes the project (after asking about unsaved changes) and leaves a new, empty one.
✅ **Safer and lighter Flex and Smart Tempo:** the sounds Flex makes are kept in your own config folder (not a shared temporary one), cleared after two weeks, and made again if the recording was replaced; a project file can no longer ask for an unbounded number of Flex markers; at most four Flex sounds are made at once; the same loop used in many regions is analysed once.
✅ **Check for Updates on slow connections:** the check now tries three times, 30 seconds each, before it reports a timeout.

✅ **Several time signatures per song:** *Track > Show Signature Track*. Double-click to add a change at a bar, double-click a block to set it, drag it to another bar, right-click to delete it. The ruler, the grids, the Piano Roll, the Event list, the display and recording follow every change, and so does the engine (the metronome accents each bar). The display shows the signature at the playhead and edits it.
✅ **Fade tool and crossfades:** the *Fade Tool* in the tool menu: drag the left half of an audio region for a fade in, the right half for a fade out. *Drag: X-Fade* (or the Inspector's *X-Fade* box) crossfades a region with the audio region it overlaps.
✅ **VCA tracks:** *Track > New VCA Track*. A VCA moves the level of the tracks you give it in their *Group* slot, and its Mute and Solo reach them too.
✅ **External MIDI tracks:** *Track > New External MIDI Track*. Pick a MIDI port and channel in the Inspector and the track's regions, and notes you play, go out to your synthesizer.
✅ **MP3 and CAF audio:** drag them onto the window like WAV, AIFF and FLAC.
✅ **1000 tracks:** the engine plays 1000 tracks (1000 audio tracks streamed for 20 seconds with no dropouts on the test PC), and the interface stays quick with them.
✅ **Lower delay on Windows:** about 12 ms instead of 32 ms between Pro Wave and the speakers on the test PC (a sound card driver with smaller buffers can do better). If it cannot be used Pro Wave falls back to the old way by itself.
✅ **Smoother meters and playhead:** the engine hands the playhead, the transport and every meter to the interface together, after each block of sound.
✅ **Tempo curves:** right-click a point of the Tempo track > *Ramp to Next Tempo Change*. The tempo moves in a straight line to the next point, and the engine follows it.
✅ **SMPTE and MIDI Time Code:** the time in the control bar follows every tempo change and curve; right-click it to pick the frame rate (24, 25, 29.97, 29.97 drop frame, 30), the start time, a MIDI port to *send* MIDI Time Code to, or a port to *follow* it from (the transport starts, jumps and stops with it).
✅ **Pattern regions and Drummer regions:** *Track > New Pattern Region* and *New Drummer Region* (or double-click an empty stretch of a Drummer track). A pattern is a grid of steps with a velocity each; a Drummer plays Rock, Pop, Funk, Jazz, Hip Hop or Electronic, busier or calmer and louder or softer as you drag its pad, with fills and variations. *Convert to MIDI Region* makes them editable note by note.
✅ **Memory-mapped audio files:** WAV and AIFF files are read straight from memory (the same samples as before). 1000 tracks take about a fifth less memory of their own.
✅ **Step Sequencer extras:** each step of a pattern can have a *Chance*, a *Repeat* count and a *Tie*, and the pattern plays *Forward*, *Backward*, *Ping-Pong* or *Random*. The *Edit* box above the grid picks what a click sets.
✅ **MIDI files:** *File > Import > MIDI File…* (or drop a `.mid` on the window) and *File > Export > Selection as MIDI File…*. The tempo changes and signatures go along.
✅ **MIDI FX:** *Track > MIDI FX…*: Transposer, Velocity Processor, Scale Quantizer, Chord Trigger, Note Repeater, Arpeggiator, Randomizer, Modulator and Scripter, in a chain on the selected track.
✅ **Modulator:** a sine, triangle, saw or square wave (its cycle in beats or bars, its depth) that moves a MIDI controller (any number, or pitch bend) of an External MIDI track while the song plays.
✅ **Scripter:** write the processing of the notes yourself in a small script (Rhai). The script is checked as you type, runs in a sandbox and stops if it takes too long.
✅ **MIDI Transform:** *Functions > MIDI Transform…* in the Piano Roll: pick the notes (all of them, or by pitch, velocity, position or length) and what to do to them (transpose, set or add velocity, shift, change length, scale, randomize). Presets: Transpose, Fixed Velocity, Legato, Humanize and more.
✅ **Sustain pedal and controllers to your synthesizer:** the pedal of a MIDI region, and the Modulator, now go out to External MIDI tracks (and are let up when you stop).
✅ **Score:** the *Score* tab of the editor shows a MIDI region as notation, with the treble and bass clefs, the key signature (pick the key at the top; minor keys too), the time signature, accidentals that last to the end of the bar, rests, and ties for notes that cross a bar line or are not one note value long. Click to add a note (in the key), right-click a note to delete it.

✅ **Safer with odd files and devices:** a MIDI file with a huge number of notes, tempo changes or tracks, or with times far past the end of a song, no longer slows Pro Wave down or breaks it (files over 128 MB are refused), very long pattern regions and MIDI FX chains stop at a size limit instead of using all the memory, and a MIDI device that floods a port can no longer fill the memory. The interface does less work each frame for pattern and Drummer regions, the Score and the Tempo track.
✅ **Tested with the pointer:** the Pattern grid, the Drummer pad, the Score and the MIDI FX window are now tried in automatic tests that click, drag and right-click like a person would.

### Still to do or test

❌ Several signatures: the Event list's *Signature* tab
❌ MIDI Time Code has not been tried with real hardware; MIDI Machine Control is not there
❌ Drummer regions: following a Groove Track
❌ Score: beams, voices, chord symbols, printing and MusicXML; MIDI files: program changes, pitch bend and other controllers
❌ External MIDI with a real synthesizer (checked with automated tests only), MIDI clock and program changes
❌ VCA tracks: automation, and a VCA for the Master
❌ Crossfades: a region wholly inside another, and *No Overlap* trimming
❌ ASIO (built, but no ASIO driver to try it with)

## 0.1.8

### All systems (Windows, macOS, Linux)

✅ **Updates from inside Pro Wave:** it looks for a newer release on GitHub when it starts (at most every 6 hours; *Help > Check for Updates…* checks at once, *Check for Updates Automatically* turns it off). A window shows what's new with *Install update*, *Download page*, *Later* and *Skip this version*. Nothing about you is sent.
✅ **Installs the right package for how you installed it:** the Windows zip, the macOS app, the AppImage, the Linux tarball, and for `.deb` and `.rpm` installs that very package, installed with the system's package manager (the system asks for your password). A Flatpak bundle is downloaded and checked, and the command to install it is shown.
✅ **Closes, updates, opens again:** *Close and update* closes Pro Wave, puts the new version in place and opens it. The download is checked against the SHA-256 that GitHub publishes for it, the old version is kept until the new one has been used once, and a failed update goes back to the old one and tells you why.
✅ **MIDI recording:** play a MIDI keyboard with *Record* and *Play* on: a red region grows on the instrument track and the notes appear in it as you play. When you stop it turns into a normal region (the track's colour), ends on a bar line and plays back. The recording is one Undo step.
✅ **Sustain pedal:** recorded with the notes, saved with the project and shown in the Event list as *Control · Sustain*. Every recording starts with where the pedal was.
✅ **Take folders:** record with the Cycle on and every pass is a take. Two or more takes make a take folder: a yellow bar with the name and a triangle, and under it the comp, side by side, with a line where one take gives way to the next. Open it to see a row for each take (latest on top), drag on a take to play it in that stretch, click to pick the whole take. *Flatten Take Folder* turns the comp into an ordinary region. Saved with the project.
✅ **List Editors (Event list):** the icon at the top right, *View > Show List Editors* or **D**. It lists the MIDI events of the selected region: position, note, velocity and length, with the notes of the other regions of the track in grey. A line marks the playhead and moves down the list at the speed of the notes while the song plays. Red while recording.
✅ **Looks like Logic:**
- Regions use Logic's own colours (measured from Logic Pro); a selected region is deeper with a yellow-green outline, and the ruler over it turns gold.
- The playhead is yellow, and red while recording; the control bar's display turns red while recording too.
- Piano Roll: notes coloured by velocity with a light edge, the region bar in the track's colour, a gold ruler, a Logic-style inspector header with the region and track, and the velocity lane drawn as dots with a line along each note.
- Track volume sliders are a dark pill with a thumb that stays inside; the level meter shows inside it. 0 dB sits two thirds up, as in Logic.
- Tracks that make no sound (muted, or another track is soloed) show their instruments, plug-ins, sends and meters in grey.
✅ **The mixer keeps its size:** the mixer panel can no longer be stretched, which only stretched the faders and meters. The Piano Roll can still be made taller.
✅ **The demo project is shown once per version** (the first time you open each new version) and then Pro Wave starts clean.
✅ **Still light:** about 330–420 ms to start and 120 MB of memory at rest (Windows), 15.5 MB program (the update check adds about 1 MB)

### Still to do or test

❌ MIDI recording with real keyboards and the sustain pedal on other computers (checked by automated tests and a simulated recording only)
❌ The pedal is recorded and shown but the engine does not play it yet
❌ Take folders: editing notes needs *Flatten* first; making a take folder from existing regions, crossfades in the comp
❌ Event list: the Marker, Tempo and Signature tabs, event type filters and editing values
❌ The update install on a real Mac, a real Linux machine and with `pkexec`; only Windows was tried end to end
❌ Help > Language texts for the new messages checked by native speakers
❌ Region colours other than green, blue, yellow and purple are an estimate

## 0.1.7

### All systems (Windows, macOS, Linux)

✅ **Tempo track:** *Track > Show Tempo Track*. Double-click to add a tempo change, drag a point to change or move it, double-click or right-click to delete. Saved with the project and undoable; the control bar shows the tempo at the playhead.
✅ **Marker track:** *Track > Show Marker Track*. Name sections (Intro, Verse, Chorus...): double-click to add, click to jump there, drag to move, double-click to rename, right-click for colour or delete. *Navigate > Create Marker*, *Delete Marker*, *Go to Next / Previous Marker*.
✅ **Audio region edits:** Region Inspector *Gain*, *Fade In*, *Fade Out*, *Curve* (Equal Power, Linear, S-Curve) and *Reverse*; right-click *Normalize Region Gain* and *Reverse Region*. The audio file is never changed. Fades show on the region and the waveform follows gain and reverse. Saved with the project.
✅ **Compact menus like Logic:** each menu is as wide as its longest entry instead of stretching across the window. The File menu follows Logic, with ▸ submenus (Open Recent, Revert to, Import, Export, Bounce, Share...), also in the macOS menu bar. Entries that aren't built yet are greyed out.
✅ **24 tracks without dropouts:** 60 seconds of 24 stereo 48 kHz / 24-bit audio tracks: 0 dropouts, 1.6 % CPU (Windows)
✅ **Lighter audio regions:** the read-ahead buffer of each audio region follows the size of the song: at most 0.68 s per region and 8 MiB for all regions together (up to 128 regions), then 0.17 s each. Before it was a fixed 0.5 MB per region (about 12 MB for 24 tracks).
✅ **More efficient reverse playback:** reuses decoded audio between blocks to reduce repeated disk seeks; automated checks cover resampling and jumping within a reversed region
✅ **Playback during large imports:** the disk worker refills active regions between batches of new-file requests and skips canceled regions before opening their files
✅ **Lighter region playback:** reuses calculated region timing between edits and skips unnecessary fade-curve calculations outside the fade
✅ **Lighter mixer updates:** during playback, volume and pan edits recalculate gain targets for the changed channel while still updating mute and solo across channels
✅ **Steadier playback:** broken values (from a damaged project or a bad setting) can no longer make noise or stop the sound
✅ **No freeze when normalizing:** *Normalize Region Gain* reads the file in the background (a long recording used to freeze the window for seconds); the status line shows "Normalizing…".
✅ **SMPTE timecode fix:** a position exactly on a frame was shown one frame early (up to 8 % of frame-exact positions at 25 fps).
✅ **Lighter transport** when the song has tempo changes; tempo ramps avoid repeated logarithm calculations between tempo boundaries
✅ **17 languages:** *Help > Language*: English (default), 简体中文, 日本語, Français, Deutsch, Español, Português (Portugal), Português (Brasil), Filipino, Bahasa Indonesia, हिन्दी, Українська, Norsk, Svenska, Suomi, Polski, Lietuvių. Each language is listed in its own name; the choice switches the whole interface at once (menus, panels, dialogs, messages, also the macOS menu bar) and is kept for the next start. Chinese, Japanese and Hindi use the system's fonts.

✅ **Still light:** about 360 ms to start and 120 MB of memory at rest (Windows), 13 MB program

### Engine groundwork (not in the interface yet)

✅ Tempo ramps (gradual tempo changes)
✅ SMPTE timecode at 24, 25, 29.97 (drop / non-drop) and 30 fps
✅ Windows ASIO for low latency, as an optional build feature

### Still to do or test

❌ Region gain, fades and reverse heard on real recordings (checked by automated tests only)
❌ Dragging fades directly on a region (Fade tool) and crossfades
❌ Tempo ramps and SMPTE display in the interface
❌ Several time signatures in one song (Signature track)
❌ ASIO in the Windows download, and its latency measured on real hardware
❌ macOS menu bar submenus tried on a real Mac
❌ Translations checked by native speakers (written by the AI, verified by automated tests for completeness, placeholders and fonts)

## 0.1.4

### All systems (Windows, macOS, Linux)

✅ **Fixed: piano sounds going to `inf`.** Mellow Grand, the other grands and Chopped Vocal made the filter blow up (meter stuck on `inf`, red clipping). The filter stays stable now and meters always reset.
✅ **Undo / Redo:** Edit menu, ⌘Z / ⇧⌘Z (Ctrl+Z / Ctrl+Y on Windows & Linux), up to 100 steps
✅ **Track > New** now adds a track
✅ **Unfinished menu items are greyed out** instead of doing nothing
✅ **Newly working:** Edit > Delete / Deselect All, Go to End, Locators, Next / Previous Region, Zoom In / Out / to Fit
✅ **Right-click menus** on regions, track headers and empty lanes (Control-click on Mac)
✅ **Small screens (13" MacBook Air):** UI scales to fit, toolbars no longer overlap, View > Bigger / Smaller Interface (⌘ = / - / 0)
✅ **Lighter:** ~100 MB less memory, faster start
✅ **Benchmarks:** 320 ms start, 125 MB idle, 3.5 % CPU for 24 tracks (Windows)

### Windows & Linux

✅ **No title bar.** Minimize / maximize / close are in the menu bar. Drag the bar to move, double-click to maximize, drag the edges to resize.
✅ The tall GNOME / Wayland header on Linux is gone
✅ **Smooth maximize and resize:** the interface no longer rescales on every frame while the window changes size, so the animation stays fluid

### macOS

✅ Menu bar items greyed out when not available yet

### Still to test

❌ Piano meter fix on a real Mac
❌ macOS benchmark
❌ Windows maximized with an always-visible taskbar

## 0.1.3

### All systems (Windows, macOS, Linux)

- **Demo song on the first launch only.** The very first time Pro Wave starts it opens the demo
  song; every later start begins with a clean project (one empty instrument track).
- **Project Chooser.** *File > New from Template…* opens a chooser with New Project, your Recent
  projects and Project Templates: Empty Project, Songwriter, Electronic, Podcast and Demo Song.
- **Window title** is now just "Pro Wave", without the project name and "Tracks".
- **Fixed:** the waveforms of audio tracks without a recording (Electric Bass and Lead Vocal in
  the demo) stayed still while the Tracks area scrolled or zoomed. They now move with their
  regions.

### macOS

- **Menus in the macOS menu bar.** The menus now sit in the menu bar at the top of the screen
  instead of inside the window, with a Pro Wave menu (About, Hide, Quit ⌘Q), the ⌘N, ⌘O, ⌘S and
  ⇧⌘S key commands, check marks in the View menu and an Open Recent submenu. Errors and warnings
  show on a thin line at the top of the window.
- **Fixed:** the Dock icon switched from the Pro Wave icon on its rounded tile to the bare logo
  once the app had started. It now keeps the app's icon.

### Windows

- The README and these notes explain what to do when an antivirus (Norton, Avast, AVG, McAfee)
  blocks `prowave.exe` with "Windows cannot access the specified device, path or file".

### Linux

- No Linux-only changes: the .deb, .rpm, AppImage, Flatpak and tarball include all the changes
  listed under *All systems*.

## 0.1.2

- **Aux and Folder tracks.** *Track > New Aux Track*, and *Create Track Stack…* / *Release Track
  Stack* to group tracks in a folder that folds open and closed, with folder Mute and Solo.
  Routing audio through them needs audio engine work that is not done yet.
- **Editable time signature.** Drag the numerator or the denominator in the display up or down.
- **File menu:** Open Recent, Save A Copy As… and Revert to Saved.
- **Windows zip** added to the release downloads, with the Pro Wave icon on `prowave.exe`.
