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
# The original sizes the channel-dependency rows by the widest *input* it knew before it adds
# `align` effects; a decoder that widens the stream (matrix4: 2 -> 4+ channels) then gets an
# `align` effect with more input channels than the rows hold, and writes past them. The copy
# built here gets rows as wide as the widest stream; nothing else is changed.
sed -i 's/NEW_SELECTOR(state->max_in_ch)/NEW_SELECTOR(state->max_ch)/' effects_chain.c
# matrix4 passes no delay-estimator history (NULL) but reads it when an event ends, which
# crashes the original; the value is only used with the estimator, so it is left out here.
python3 - <<'PY'
p = "matrix4_common.c"
s = open(p).read()
old = "(ewma_get_last(&ev->align_avg)+ewma_get_last(&hist->align_avg))*0.5;\n\t\t\tdouble ev_dt"
new = "(hist) ? (ewma_get_last(&ev->align_avg)+ewma_get_last(&hist->align_avg))*0.5 : 0.0;\n\t\t\tdouble ev_dt"
assert old in s
open(p, "w").write(s.replace(old, new))
PY
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
python3 "$HERE/make_long_input.py" "$HERE/long.f32"
python3 "$HERE/make_spaced_input.py" "$HERE/spaced.f32"
grep -v '^#' "$HERE/cases.txt" | while IFS='|' read -r name fs words; do
  name="$(echo "$name" | tr -d ' ')"; fs="$(echo "$fs" | tr -d ' ')"
  # shellcheck disable=SC2086
  in="$HERE/input.f32"; case "$name" in long_*) in="$HERE/long.f32";; spaced_*) in="$HERE/spaced.f32";; esac
  # `stats_*` cases also keep what the effects print (the statistics tables).
  err=/dev/null; case "$name" in stats_*) err="$HERE/$name.txt";; esac
  # Run from the golden directory: effects files (`@effects/...`) are found from there.
  (cd "$HERE" && "$WORK/dsp/dsp" -q -I -t pcm -e float -r "$fs" -c 2 "$in" -o -t pcm -e float "$HERE/$name.f32" $words 2>"$err")
  # keep only the tables: the program's own notes start with its name
  [ "$err" = /dev/null ] || sed -i '/^[^ ]*dsp: /d' "$err"
done
rm -rf "$WORK"
