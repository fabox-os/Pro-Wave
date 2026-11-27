"""A third input for the multiband decoder: 48000 stereo frames (1 s at 48 kHz, 32-bit float)
in which the channels of an event are out of step, as with spaced microphones.

Five bursts of a few partials each (so that several bands see them): centre with the right
channel 10 frames late, hard left, centre with the left channel 7 frames late, out of phase
with a delay of 12 frames, and a noise-like burst in the right channel with a delayed copy
in the left, over a quiet bed of noise.
"""
import math
import struct
import sys

n = 48000
seed = 4242
partials = [(300, 1.0), (880, 0.8), (1700, 0.6), (3100, 0.5), (6200, 0.3)]
out = bytearray()


def lcg():
    global seed
    seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
    return seed / 0x7FFFFFFF - 0.5


noise = [lcg() for _ in range(n)]


def tone(t):
    if t < 0:
        return 0.0
    env = math.exp(-t / 2500.0) * min(1.0, t / 60.0)
    return 0.35 * env * sum(a * math.sin(2 * math.pi * f * t / 48000) for f, a in partials)


def burst(t):
    if t < 0:
        return 0.0
    return 0.5 * noise[t] * math.exp(-t / 1500.0) * min(1.0, t / 30.0)


for i in range(n):
    bed = lcg() * 0.01
    l = r = bed
    # centre, right 10 frames late
    l += tone(i - 2000)
    r += tone(i - 2010)
    # hard left
    l += tone(i - 12000)
    # centre, left 7 frames late
    l += tone(i - 7 - 22000)
    r += tone(i - 22000)
    # out of phase, right 12 frames late
    l += tone(i - 32000) * 0.7071
    r -= tone(i - 12 - 32000) * 0.7071
    # noise burst in the right channel, delayed copy at the left
    r += burst(i - 41000)
    l += burst(i - 5 - 41000) * 0.8
    out += struct.pack("<ff", l, r)
open(sys.argv[1], "wb").write(out)
