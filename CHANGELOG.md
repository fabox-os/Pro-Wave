# Changelog

What changed in each Pro Wave release. The release workflow copies the section of the version
being released into the GitHub Release notes, so keep one `## <version>` heading per release.

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
