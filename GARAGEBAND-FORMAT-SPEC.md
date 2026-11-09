# Pro Wave — GarageBand Project Support Spec

**Status: research, not implemented.** This document is the plan for issue
[#35](https://github.com/fabox-os/Pro-Wave/issues/35), "Add support for opening GarageBand
projects". It complements [`LOGICX-FORMAT-SPEC.md`](LOGICX-FORMAT-SPEC.md): the newest GarageBand
projects use the *same* binary project format as Logic Pro, so one parser serves both.

Nothing here is Apple code or Apple artwork; every detail below comes from published
reverse-engineering and from files a user supplies. Pro Wave ships no GarageBand content.

## 1. What a GarageBand project actually is

A `.band` is not a single file. On macOS it is a **bundle** (a directory Finder shows as one
item); on iPad and iPhone it is exported as a `.band` file that is really a **zip**. Pro Wave
must accept both the directory and the zip, and both save formats GarageBand allows.

Two families exist, and they differ completely:

| Family | Where | Project data | Media |
|---|---|---|---|
| GarageBand 10.x (2014 and later, macOS) | `.band/Alternatives/000/ProjectData` | the Logic Pro binary chunk format | `Media/Audio Files/` (AIFF/CAF/M4A) |
| GarageBand 6 and earlier, GarageBand for iOS | `.band/projectData` (in the zip on iOS) | an XML property list holding a base64 blob | `Media/` (AIFF) |

A bundle can contain both files: GarageBand 10 keeps an XML `projectData` at the root and the
real arrangement in `Alternatives/000/ProjectData`. The root file is an archive of the project's
document state (NSKeyedArchiver), the alternative is the arrangement.

### 1.1 GarageBand 10.x bundle

```
MySong.band/
├── Alternatives/
│   └── 000/
│       ├── ProjectData            # arrangement: tracks, regions, notes, mixer (chunk format)
│       ├── MetaData.plist         # tempo, key, time signature, sample rate (XML or binary plist)
│       ├── DisplayState.plist     # window layout, not needed
│       ├── Previews/Output.aif    # mix preview, not needed
│       └── Undo Data.nosync/      # undo history, not needed
├── Media/
│   └── Audio Files/               # the source audio
├── Output/Output.aif              # last export, not needed
├── Resources/ProjectInformation.plist
└── projectData                    # XML + base64 archive; see 1.2
```

### 1.2 GarageBand 6 / iOS bundle

```
MySong.band/
├── projectData                    # XML plist with one large <data> element (base64)
└── Media/                         # source AIFF files
```

The `<data>` text is base64 over an Apple binary property list (starts `bplist00`, an
NSKeyedArchiver object graph). Decoded, it still looks binary, but the instrument and region
names are plain strings, which is how the community first inspected it.

## 2. The two `projectData` formats

### 2.1 GarageBand 10.x — the Logic Pro chunk format (mostly understood)

`Alternatives/000/ProjectData` starts with the 24-byte header of a `.logicx` `ProjectData`:

- magic `23 47 C0 AB`, version bytes at `0x04..0x06`, and the total size as a 64-bit
  little-endian value at the end of the header;
- then chunks, each with a 36-byte header: a **reversed** four-byte code (`karT` is "Trak",
  `qeSM` is "MSeq", `qSvE` is "EvSq", `gRuA` is "AuRg", `lFuA` is "AuFl", `gnoS` is "Song"),
  metadata fields, and the chunk's length as a 64-bit little-endian value;
- the file is read by scanning chunks linearly; there is no index table.

The chunk layouts known from the Logic Pro work (`Song`, `Trak`, `MSeq`, `EvSq`, `AuRg`,
`AuFl`, `Envi`, `Layr`, `AuCO`) are described in
[`LOGICX-FORMAT-SPEC.md`](LOGICX-FORMAT-SPEC.md) sections 2.2 to 2.4. GarageBand writes the same
chunks with a smaller set of track types.

**Known:** header and chunk framing; the chunk names above; where the fader value sits in
`AuCO`; that MIDI regions are `MSeq` chunks.
**Unknown until we have sample projects:** which subset GarageBand writes, whether track order
and region start bars are in `Trak`/`AuRg` exactly as Logic stores them, and which version bytes
GarageBand uses.

### 2.2 GarageBand 6 / iOS — XML plist with a base64 archive (less understood)

`projectData` is an XML property list. One `<data>` element holds a large base64 string. Decoded,
the string is a binary property list (an NSKeyedArchiver graph). Reading it needs three steps:

1. **Parse the XML plist** (`plist` crate, MIT/Apache-2.0, pure Rust).
2. **Base64-decode** the `<data>` value (`base64` crate, MIT/Apache-2.0).
3. **Un-archive the object graph**: a `$objects` array, a `$top` root, UID references, and
   objects that may point at each other in cycles.

The archived object graph holds the song (tempo, time signature, key), the track list, the
regions with their start bars and lengths, and the notes of MIDI regions. The exact class names
and key paths are **not documented publicly** in the sources found so far; they must be learned
from a sample project. The first working step (listing instruments and region names) can be done
without knowing the schema, by walking every string and dictionary in the graph.

## 3. How the data maps onto Pro Wave

| GarageBand | Pro Wave | Where it comes from |
|---|---|---|
| Tempo, time signature, key | `Project` tempo map, signature, key | `MetaData.plist`; the archive in the old format |
| Tracks (audio, instrument, drummer) | `Track` of the same kind | `Trak` chunks / archive |
| Audio regions and their files | `Region::Audio` with `Media/Audio Files/…` | `AuRg` / archive |
| MIDI regions and notes | `Region::Midi` with `Note`s | `MSeq` / archive |
| Volume, pan, mute, solo | `Track` mixer fields | `AuCO`, `TrackMix` in the archive |
| Instruments (Apple stock and AU) | Pro Wave's built-in synth or a SoundFont | `InSt` / AU component triples; unknown names map to a default instrument and are listed in a warning |
| Effects (Channel EQ, Compressor…) | matching Pro Wave effect, else left out with a warning | stock plug-in names (`GAME` anchor) and AU triples |
| Project name | `Project::name` | `Resources/ProjectInformation.plist`, folder name |

Import is **read-only**: Pro Wave opens `.band` projects into a `.prowave` project. Writing
`.band` files is out of scope (see section 8).

## 4. Crate and API plan

Two crates, sharing the chunk reader:

- `prowave-logicx-import` (planned in the Logic spec) — the chunk reader, the plists and the
  mapping code for the chunk format.
- `prowave-garageband-import` — a small crate in front of it: bundle and zip detection, the
  GarageBand 10 bundle layout, and the old XML + base64 + NSKeyedArchiver reader.

Sketch:

```rust
/// A project bundle found on disk: the file or folder the user picked, and its kind.
pub enum Bundle {
    GarageBand10(PathBuf),   // a .band folder (or unzipped iOS 10+ export)
    GarageBandZip(PathBuf),  // an iOS .band zip
    LegacyBand(PathBuf),     // projectData is the XML + base64 form
}

pub fn detect(path: &Path) -> Result<Bundle, Error>;

/// Reads a project into Pro Wave's model (audio files are only listed, not decoded).
pub fn read(bundle: &Bundle) -> Result<ImportedProject, Error>;

pub struct ImportedProject {
    pub name: String,
    pub tempo: f64,
    pub signature: (u8, u8),
    pub sample_rate: u32,
    pub tracks: Vec<ImportedTrack>,
    /// Things the import could not map, for the "imported with warnings" dialog.
    pub warnings: Vec<String>,
}
```

The app side: `File > Open…` accepts `.band` (folder) and `.band.zip`; the result fills the
normal project model, so everything else (editing, playback, `.prowave` saving) works unchanged.

## 5. Phases (each one is a pull request)

1. **Bundle inspector.** Detect the family and version, open `.band` folders and iOS zips
   safely, read `MetaData.plist` (tempo, signature, key, sample rate) and list the media files.
   No notes yet. Tests use bundles this repository creates in code.
2. **Audio import.** Tracks and audio regions from the media, with names and, when the chunk
   reader can find them, start bars and lengths.
3. **MIDI import.** `MSeq` regions to `Region::Midi` with their notes (chunk format first, then
   the archived old format).
4. **Mixer and names.** Volumes, pans, mute/solo, track and region names, colours.
5. **Instruments and effects.** Map the known Apple stock plug-ins to Pro Wave's effects; map
   unknown ones to a default instrument with a warning.
6. **Legacy reader.** The old XML + base64 + NSKeyedArchiver format, behind the same
   `ImportedProject` API.

Phase 1 is useful on its own: it tells a user what is inside a project and how much of it Pro
Wave can play, without promising an import it cannot finish.

## 6. Security

`.band` files are untrusted, like `.prowave` files:

- A zip is unpacked in memory (or into a temporary folder that is removed afterwards) with
  **zip-slip protection**: entry names must stay below the bundle root, and the number of
  entries and the unpacked size are capped.
- The XML plist is capped in size; the base64 blob is capped after decoding; the binary plist
  reader caps nesting, object count and reference cycles (a hostile `$objects` graph must not
  loop or allocate without a bound).
- Chunk lengths are checked against the file: a chunk that runs past the end, or a size that
  would make the reader allocate more than the file, is an error, not a panic.
- Media files are capped like any other audio file; they are decoded by the existing streaming
  code, which already validates formats.
- Every failure is an error with a message, never a panic (`#![deny(clippy::unwrap_used)]`).

## 7. Testing plan

- **Synthetic GarageBand 10 bundles** built in test code: a chunk writer (the inverse of the
  reader) produces `ProjectData` with `Song`, two `Trak`s, one `MSeq` and one `AuRg`, plus a
  `MetaData.plist` and a tiny WAV. Tests read it back and compare the imported model.
- **Synthetic legacy bundles**: an XML plist with a base64-encoded binary plist built in test
  code, holding a small object graph.
- **Golden files from real projects** are welcome but cannot be committed without the author's
  permission; the tests above must pass without them.
- **Fuzz corpus**: truncated and mutated `ProjectData` and plist inputs, checked for errors
  rather than panics.

## 8. Open questions

The import cannot be finished correctly without at least one real project of each family. We
need:

1. A **GarageBand 10.x** project (`.band` folder or zip) with one software-instrument track with
   a few MIDI notes, one audio track, a tempo and a name; macOS version noted.
2. A **GarageBand for iOS** project with the same content (share the project to Files, then zip
   it), iOS version noted.
3. Permission to use them as fixtures, or failing that, answers to the field questions below.

Field questions for a sample: the byte at `Alternatives/000/ProjectData[0..8]`, the chunk names
and their order, and whether the track names appear in `Trak` data as plain strings.

## 9. Sources

- MultimediaWiki, "GarageBand" — bundle layouts for versions ≤ 6 and 10.0.3, the XML + base64
  `projectData`, and the `strings` extraction trick:
  <https://wiki.multimedia.cx/index.php/GarageBand>
- `fastfourier666/cigol` — reverse engineering of the Logic/GarageBand 10 binary chunk format
  (header, 36-byte chunk headers, reversed four-character codes):
  <https://gitlab.com/fastfourier666/cigol>
- "Exporting MIDI from iOS GarageBand projects" — an iOS `.band` as a zip with an XML
  `projectData` and a base64 payload: <https://groundjuggling.wordpress.com/export-midi-from-ios-garage-band-projects/>
- `larkob/GB2MIDI` — a related tool (it extracts MIDI from Apple loop files, not from projects),
  useful only as background: <https://github.com/larkob/GB2MIDI>
- This repository's [`LOGICX-FORMAT-SPEC.md`](LOGICX-FORMAT-SPEC.md) — the chunk format shared with
  GarageBand 10.
