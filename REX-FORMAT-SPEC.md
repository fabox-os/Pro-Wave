# REX2 File Format — Notes for `prowave-rex`

What a ReCycle/Reason `.rx2` (and the older `.rex`) loop contains, and what Pro Wave reads.
The implementation is `crates/prowave-rex`; this file records the byte layout and the decoder
rules it is written from, so the crate can be checked without a copy of the format notes.

Sources: the REX2 notes published with the `schwung-rex` reader
(<https://github.com/charlesvestal/schwung-rex/blob/main/REX2_FORMAT.md>) and the VelociLoops
format documentation (<https://velociloops.readthedocs.io/en/latest/format.html>). Both describe
the same container; where they differ, the first was followed (see "Open questions" below).

## Container

REX2 is an IFF-style container, **big-endian throughout**:

```
CAT REX2
├── HEAD           magic 0x490CF18D and a version record
├── GLOB           tempo, bars, sensitivities, processing gain
├── RECY           the tempo the loop was recorded at
├── CAT SLCL
│   └── SLCE × N   one per slice (or transient marker)
├── SINF           channels, bit depth, sample rate, frame count, loop region
└── SDAT           one DWOP bitstream for the whole waveform
```

A normal chunk is:

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | Four-byte ASCII id |
| 4 | 4 | Payload size, `uint32` big-endian |
| 8 | N | Payload |

Chunks are padded to even offsets; the pad byte is not counted in the size. `CAT ` (and
`FORM`) chunks start their payload with a four-byte type (`REX2` at the top level) and then
hold more chunks. Pro Wave accepts no more than 8 levels of nesting or 8192 chunks, and refuses
any chunk whose size runs past the end of its parent.

### GLOB (at least 20 bytes used)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | Slice count |
| 4 | 2 | Bars |
| 6 | 1 | Beats |
| 7 | 1 | Time-signature numerator |
| 8 | 1 | Time-signature denominator |
| 9 | 1 | Analysis sensitivity |
| 10 | 2 | Gate sensitivity |
| 12 | 2 | Processing gain (1200 is unity in the SDK model) |
| 14 | 2 | Pitch |
| 16 | 4 | Preview tempo, BPM × 1000 |
| 20 | 1 | Transmit-as-slices flag (optional) |
| 21 | 1 | Silence-selected flag (optional) |

### RECY (15 bytes)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 8 | Version/flags (`BC 02 00 00 00 01 00 00`) |
| 8 | 4 | Original tempo, BPM × 1000 |
| 12 | 2 | Reserved |
| 14 | 1 | Unit byte, written as 8 |

### SINF (10 bytes minimum, 18 in practice)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 1 | Channels: 1 or 2 |
| 1 | 1 | Format code: 1 = 8-bit, 3 = 16-bit, 5 = 24-bit, 7 = 32-bit float |
| 2 | 4 | Sample rate in Hz (zero means 44100) |
| 6 | 4 | Total frames per channel in the decoded waveform |
| 10 | 4 | Loop start frame, inclusive (when present) |
| 14 | 4 | Loop end frame, exclusive (when present) |

The two published layouts disagree on bytes 2–5: one reads a `uint16` sample rate at offset 4
and calls offset 2 unknown, the other reads a `uint32` at offset 2. They agree whenever the
upper half is zero, which is true for every rate up to 65535 Hz, so the `uint32` reading is
used.

### SLCE (11 bytes; 8 is accepted)

| Offset | Size | Field |
|---:|---:|---|
| 0 | 4 | Slice start frame in the decoded waveform |
| 4 | 4 | Slice length in frames; ≤ 1 marks a transient marker |
| 8 | 2 | Analysis points (0x7FFF when written) |
| 10 | 1 | Flags: bit 0 muted, bit 1 locked, bit 2 selected |

Slices do not carry audio: they are ranges of the one decoded waveform. Pro Wave keeps the
entries with a length above 1 as `RexSlice`s and the others as marker positions in frames.

## DWOP audio codec

`SDAT` is the bitstream itself, with no header. Bits are read MSB-first from the first byte.
Readers consume it in 32-bit words, and every real file's payload is a multiple of four bytes,
so the writer pads the last word with zero bits; a reader that drops a trailing partial word
would otherwise lose the loop's final samples. The reader is shared by the channels; each
channel keeps its own predictor and range state and starts from the same values:

| Variable | Initial value |
|---|---:|
| `S[0..4]` | 0 |
| `e[0..4]` | 2560 (`0xA00`) |
| `rv` | 2 |
| `ba` | 0 |

Per sample:

1. **Predictor.** `p` is the index of the smallest `e` (ties go to the lowest index). The
   prediction order *is* `p`: the value decoded is the sum of `S[0..p]` plus the residual.
2. **Scale.** `step = (e[p] * 3 + 36) >> 7`.
3. **Unary part.** Read bits until a 1. Each 0 adds the current `cs` to `acc`; `cs` starts at
   `step` and quadruples after every 7 zeros. (A step of zero is treated as 1 so a damaged
   file cannot divide the range arithmetic by nothing.)
4. **Range part.** With `nb = ba`:

   ```
   if cs >= rv:  while cs >= rv { rv *= 2; nb += 1 }
   else:         nb += 1; t = rv
                 loop { rv = t; t /= 2; nb -= 1; if cs >= t { break } }

   ext = read_bits(nb)          // 0 when nb <= 0
   co  = rv - cs
   if ext < co:  rem = ext
   else:         rem = co + (ext - co) * 2 + read_bit()

   val = acc + rem
   ba  = nb
   ```

5. **Zigzag.** `d = val XOR -(val AND 1)`: 0→0, 1→-2, 2→2, 3→-4, 4→4, … All `d` are even.
6. **Predictor update.** The energy index maps to a *switch case* through
   `[0, 1, 4, 2, 3]`; the cases are:

   | Case | New `S` |
   |---:|---|
   | 0 | `S0 = d; S1 = d - old0; S2 = S1 - old1; S3 = S2 - old2; S4 = S3 - old3` |
   | 1 | `S0 = old0 + d; S1 = d; S2 = d - old1; S3 = S2 - old2; S4 = S3 - old3` |
   | 4 | `S1 = old1 + d; S0 = old0 + S1; S2 = d; S3 = d - old2; S4 = S3 - old3` |
   | 2 | `S2 = old2 + d; S1 = old1 + S2; S0 = old0 + S1; S3 = d; S4 = d - old3` |
   | 3 | `S3 = old3 + d; S2 = old2 + S3; S1 = old1 + S2; S0 = old0 + S1; S4 = d` |

7. **Energy update**, for every `i`:
   `absS = S[i] XOR (S[i] >> 31)` (arithmetic shift: `|S|-1` when negative),
   `e[i] = e[i] + absS - (e[i] >>> 5)`.
8. **Output.** `sample = S0 >> 1` (arithmetic).

**Stereo.** The left channel is coded directly; the right channel codes `right - left`. Per
frame the decoder reads left, then right, and writes `[left, left + right_delta]` interleaved.

### Encoder (used by the crate's writer)

The encoder is the exact inverse and keeps the same state, so it selects the same predictor the
decoder will. The residual is `d = target - sum(S[0..p])`; `val` is `d` for `d >= 0` and
`-d - 1` otherwise. It scans unary zero counts from 0 upwards and takes the first one whose
`(cs, rv, nb)` can code `val - acc` with the range coder, then updates its state exactly as the
decoder does.

## What Pro Wave reads

Read: `CAT REX2`, `HEAD` (only the magic is looked for), `GLOB` (tempo), `RECY` (original
tempo), `SINF` (channels, format, rate, frames, loop region), `SLCE` (slices and markers),
`SDAT` (DWOP).

Not read: `CREI` (creator strings), the `CAT DEVL` chunks (`TRSH`, `EQ `, `COMP`), the PPQ
length field, and the transient **Stretch** tails a REX player synthesises at render time —
those change how a slice sounds when it is cut short, not the audio in the file.

A 24-bit file's DWOP stream holds 24-bit samples (a real 24-bit fixture decodes to the full
−8388608…8388607 range). Because `RexFile` stores `i16`, the crate shifts those samples down by
8 bits: the waveform is right, the low 8 bits are gone, and `warnings` says so. 8-bit (1) and
32-bit float (7) files are decoded through the same 16-bit path and reported as such. Format
codes are otherwise refused, as are more than 2 channels. The decoded waveform is capped at 50
million samples.

## Open questions

- The two sources disagree on the `SINF` layout before offset 6 (see above) and on the exact
  reading of `HEAD`. Pro Wave does not depend on any `HEAD` field after the magic.
- 24-bit storage: the public notes call the codec 16-bit, but a real 24-bit file decodes to
  full 24-bit samples (confirmed against VelociLoops). Pro Wave shifts them to 16 bits and says
  in `warnings` that the low 8 bits were dropped.
- Tempo-to-PPQ mapping is derived (REX uses 15360 ticks per 4/4 bar, 3840 per quarter note).
  `RexFile::slice_beat` exposes it for a project's grid; the file itself stores frames only.
