# Issue #146 — reference projects and licences

Survey of the links in issue #146 ("Synthesizers and effects parts for pro wave"). Licences come from
GitHub labels, README and COPYING text, or SourceForge/Launchpad metadata. Raw LICENSE files were not
all read; check each before copying any code.

Reuse rules for Pro Wave: the project is GPL-3.0-or-later today. Permissive code can be reused under
either licence. GPL-3 code can be reused only if Pro Wave stays GPL-3. GPL-2-only code cannot be combined
with GPL-3. Anything AGPL, or with conflicting licence terms, needs a licence review first. Ideas and
designs can always be learned from.

| Project | What it is | Licence | Reuse status |
|---|---|---|---|
| surge (surge-synthesizer/surge) | Surge XT hybrid synth | GPL-3.0 | GPL-3 only |
| shortcircuit-xt | sample instrument (JUCE) | MIT source; binaries AGPL-3.0+; `libs/` GPL/AGPL | Source outside `libs/` only |
| fsynth (grz0zrg) | browser pixel/GLSL synth | BSD-2-Clause | Permissive |
| spectmorph (swesterfeld) | spectral morphing synth | LGPL-2.1 | Weak copyleft |
| xsynth-dssi (theabolton) | VCO-VCF-VCA synth, DSSI | GPL-2.0-or-later | GPL-2 family |
| whysynth (theabolton) | multi-mode softsynth, DSSI | GPL-2.0-or-later | GPL-2 family |
| Teliasopia (SourceForge) | LV2 plugin collection | GPL-2.0 | GPL-2 only |
| dragonfly-reverb (michaelwillis) | reverbs, LV2/VST | GPL-3.0 | GPL-3 only |
| Pitchtune (SourceForge) | GTK tuner program | GPL, version not stated | Unresolved |
| rakarrack (SourceForge) | multi-effects for JACK | unknown | Unresolved |
| fat1.lv2 (x42) | AutoTune LV2 | GPL-2.0 | GPL-2 only |
| Invada Studio (Launchpad) | LV2 effects | GPL v2 | GPL-2 only |
| Jali (hergezod) | Karplus-Strong instrument, VST3 | MIT | Permissive |
| halden6 (ThorstenWeberGER) | Juno-60-style polysynth | AGPL-3.0 | AGPL, review first |
| LibreAudio/Suite | plugin toolbox (pre-release) | GPL-3.0 | GPL-3 only |
| setBfree (pantherb) | tonewheel organ, LV2/JACK | GPL-2.0 | GPL-2 only |
| hexter (keithadler) | DX7 modelling synth | GPL-2.0-or-later | GPL-2 family |
| jack-meterbridge (stephanb2) | JACK meter bridge | GPL-2.0 (COPYING) vs GPL-3.0 (README) | Conflict, check COPYING |
| IEMPluginSuite (IEM) | plugin suite | not read (HTTP 403) | Unknown |
| Bristol (SourceForge) | vintage synth emulations | GPL-2.0 | GPL-2 only |
| ToneWerk (Michi71) | JUCE electric keyboards | GPL-3.0-or-later | GPL-3 family |
| fil4.lv2 (x42) | 4-band parametric EQ | GPL-2.0 | GPL-2 only |
| modu-SYNTH (AdamUllmann) | JUCE polysynth | Apache-2.0 | Permissive |
| BYOD (Chowdhury-DSP) | guitar distortion chain | GPL-3.0 | GPL-3 only |
| chow-tape-model (studiorack) | tape model, VST/AU/LV2 | GPL-3.0 | GPL-3 only |
| glame (SourceForge) | audio editor, LADSPA effects | GPL-2.0 | GPL-2 only |
| element (kushview) | plugin host | unknown | Unresolved |

## Summary

- Permissive (usable in an MIT or Apache-2.0 build): Jali, fsynth, modu-SYNTH, shortcircuit-xt source outside `libs/`.
- Weak copyleft: spectmorph.
- GPL-3 family (usable only if Pro Wave stays GPL-3): surge, dragonfly-reverb, LibreAudio/Suite, ToneWerk, BYOD, chow-tape-model.
- GPL-2 family (usable only with a GPL-2-compatible licence, not GPL-3-only): xsynth-dssi, whysynth, hexter, Teliasopia, fat1, Invada Studio, setBfree, Bristol, fil4, glame.
- AGPL-3.0: halden6, and shortcircuit-xt binaries.
- Unresolved: jack-meterbridge, Pitchtune, rakarrack, element, IEMPluginSuite.

## Open decisions

1. Licence: stay GPL-3.0-or-later, or dual-license under MIT/Apache-2.0 as the issue suggests.
2. Which features to pursue: LV2 hosting, a standalone build, new instruments or effects, branding.
