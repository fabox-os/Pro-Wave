"""A second, longer input for the decoders: 28800 stereo frames (0.6 s at 48 kHz, 32-bit float).

Four note-like bursts with different positions in the stereo image: hard left, centre,
hard right and out of phase (right = -left, what a matrix decoder sends to the surrounds),
over a quiet bed of noise, so that the steering logic sees events of every kind.
"""
import math
import struct
import sys

n = 28800
seed = 777
out = bytearray()
bursts = [(1000, 0.0), (8000, 2.0), (15000, 1.0), (22000, 3.0)]  # start frame, position code
for i in range(n):
    seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
    bed = (seed / 0x7FFFFFFF - 0.5) * 0.02
    l = r = bed
    for start, pos in bursts:
        t = i - start
        if 0 <= t < 5500:
            env = math.exp(-t / 1800.0) * min(1.0, t / 40.0)
            tone = 0.6 * env * (math.sin(2 * math.pi * 880 * t / 48000) + 0.5 * math.sin(2 * math.pi * 2200 * t / 48000))
            if pos == 0.0:
                l += tone
            elif pos == 1.0:
                l += tone * 0.7071
                r += tone * 0.7071
            elif pos == 2.0:
                r += tone
            else:
                l += tone * 0.7071
                r -= tone * 0.7071
    out += struct.pack("<ff", l, r)
open(sys.argv[1], "wb").write(out)
