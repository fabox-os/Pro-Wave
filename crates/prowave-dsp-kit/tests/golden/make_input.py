"""The input of the golden tests: 4800 stereo frames at 48 kHz, 32-bit float, little endian.

Left: two sines (440 Hz and 5 kHz). Right: a sine sweep-ish pair, a click at frame 100 and a
little deterministic noise. The two channels differ so that channel selectors and mid/side
show up in the comparison.
"""
import math
import struct
import sys

n = 4800
seed = 12345
out = bytearray()
for i in range(n):
    seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
    noise = (seed / 0x7FFFFFFF - 0.5) * 0.1
    left = 0.5 * math.sin(2 * math.pi * 440 * i / 48000) + 0.25 * math.sin(2 * math.pi * 5000 * i / 48000)
    right = 0.4 * math.sin(2 * math.pi * 1000 * i / 48000) + 0.2 * math.sin(2 * math.pi * 120 * i / 48000) + noise
    if i == 100:
        right += 0.8
    out += struct.pack("<ff", left, right)
open(sys.argv[1], "wb").write(out)
