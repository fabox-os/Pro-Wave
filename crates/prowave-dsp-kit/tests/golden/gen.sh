#!/bin/sh
# Regenerates the golden files with the original program, so the Rust port can be compared with
# it sample by sample. Usage: gen.sh /path/to/dsp-source-dir
#
# It builds the original without its optional libraries, makes the input signal, and runs each
# line of cases.txt through `dsp` writing raw 32-bit float. The FFT effects (fir, resample…)
# need FFTW: where it is not installed, the stand-in in fftw_shim/ is used instead.
set -e
SRC="$1"; HERE="$(cd "$(dirname "$0")" && pwd)"; WORK="$(mktemp -d)"
[ -n "$SRC" ] || { echo "usage: $0 /path/to/dsp" >&2; exit 2; }
cp -r "$SRC" "$WORK/dsp" && cd "$WORK/dsp"
if ! pkg-config --exists fftw3; then
  mkdir -p "$WORK/shim" && cc -O2 -c "$HERE/fftw_shim/fftw3.c" -o "$WORK/shim/fftw3.o" \
    && ar rcs "$WORK/shim/libfftw3.a" "$WORK/shim/fftw3.o"
  cp "$HERE/fftw_shim/fftw3.h" "$WORK/shim/"
  printf 'Name: fftw3\nDescription: stand-in\nVersion: 3.3\nCflags: -I%s\nLibs: -L%s -lfftw3 -lm\n' \
    "$WORK/shim" "$WORK/shim" > "$WORK/shim/fftw3.pc"
  export PKG_CONFIG_PATH="$WORK/shim${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
fi
./configure --disable-sndfile --disable-ffmpeg --disable-zita-convolver \
  --disable-alsa --disable-ao --disable-pulse --disable-ladspa_dsp --disable-ladspa-host >/dev/null
make -j4 >/dev/null
python3 "$HERE/make_input.py" "$HERE/input.f32"
grep -v '^#' "$HERE/cases.txt" | while IFS='|' read -r name fs words; do
  name="$(echo "$name" | tr -d ' ')"; fs="$(echo "$fs" | tr -d ' ')"
  # shellcheck disable=SC2086
  ./dsp -q -I -t pcm -e float -r "$fs" -c 2 "$HERE/input.f32" -o -t pcm -e float "$HERE/$name.f32" $words 2>/dev/null
done
rm -rf "$WORK"
