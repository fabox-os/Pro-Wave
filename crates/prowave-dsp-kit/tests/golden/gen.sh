#!/bin/sh
# Regenerates the golden files with the original program, so the Rust port can be compared with
# it sample by sample. Usage: gen.sh /path/to/dsp-source-dir
#
# It builds the original without its optional libraries (the effects here need none), makes
# the input signal, and runs each line of cases.txt through `dsp` writing raw 32-bit float.
set -e
SRC="$1"; HERE="$(cd "$(dirname "$0")" && pwd)"; WORK="$(mktemp -d)"
[ -n "$SRC" ] || { echo "usage: $0 /path/to/dsp" >&2; exit 2; }
cp -r "$SRC" "$WORK/dsp" && cd "$WORK/dsp"
./configure --disable-sndfile --disable-ffmpeg --disable-fftw3 --disable-zita-convolver \
  --disable-alsa --disable-ao --disable-pulse --disable-ladspa_dsp --disable-ladspa-host >/dev/null
make -j4 >/dev/null
python3 "$HERE/make_input.py" "$HERE/input.f32"
grep -v '^#' "$HERE/cases.txt" | while IFS='|' read -r name fs words; do
  name="$(echo "$name" | tr -d ' ')"; fs="$(echo "$fs" | tr -d ' ')"
  # shellcheck disable=SC2086
  ./dsp -q -I -t pcm -e float -r "$fs" -c 2 "$HERE/input.f32" -o -t pcm -e float "$HERE/$name.f32" $words 2>/dev/null
done
rm -rf "$WORK"
