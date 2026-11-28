//! The chain's optimiser (`ec_optimize` of the original): an effect that can be merged
//! (gain and mult, add, delay, the biquads, dither, the reverse IIR) takes in the next
//! effects of its kind, as long as nothing between them touches the same channels in a way
//! that depends on the order. The biquads can also be merged across effects that move
//! channels, following the channels through them.

use crate::align::{find_input_deps, query_deps};
use crate::effect::{Effect, Merge};

/// `ec_optimize`: merges until nothing more can be merged.
pub(crate) fn optimize(effects: &mut Vec<Box<dyn Effect>>) {
    loop {
        let mut modified = false;
        let mut i = 0;
        while i < effects.len() {
            if try_merge(effects, i) > 0 {
                modified = true;
            }
            i += 1;
        }
        if !modified {
            break;
        }
    }
}

/// `OPT_CH_SET`
fn opt_set(e: &dyn Effect, k: usize) -> bool {
    e.opt_selector()
        .is_none_or(|s| s.get(k).copied().unwrap_or(false))
}

/// `ec_optimize_build_ch_map`: follows the channel map through `src`, whose dependencies are
/// in `deps`; channels that `src` mixes with others it is not mapped with are lost (-1).
fn build_ch_map(ch_map: &mut [i32], deps: &[Vec<bool>], src: &dyn Effect) {
    let (n_in, n_out) = (src.istream().channels, src.ostream().channels);
    for k in 0..n_in {
        if ch_map[k] < 0 {
            continue;
        }
        let in_deps = find_input_deps(k, deps, n_in, n_out);
        if (0..n_in).any(|i| in_deps[i] && ch_map[i] != ch_map[k]) {
            for j in 0..n_in {
                if in_deps[j] {
                    ch_map[j] = -1;
                }
            }
        }
    }
    let tmp: Vec<i32> = ch_map[..n_in].to_vec();
    for i in 0..n_out {
        ch_map[i] = deps[i][..n_in]
            .iter()
            .position(|b| *b)
            .map_or(-1, |k| tmp[k]);
    }
}

/// The part of `ec_optimize_next_src` that pins down the channels of an effect that may not
/// be moved.
fn mark(ch_map: &mut [i32], src: &dyn Effect) {
    if src.reorderable() {
        return;
    }
    let n = src.istream().channels;
    for (k, m) in ch_map.iter_mut().enumerate().take(n) {
        if opt_set(src, k) {
            *m = -1;
        }
    }
}

/// `ec_optimize_try_merge`: merges the effects after `dest` into it. Returns how many.
fn try_merge(effects: &mut Vec<Box<dyn Effect>>, dest: usize) -> usize {
    if effects[dest].merge_id().is_none() {
        return 0;
    }
    let max_ch = effects
        .iter()
        .map(|e| e.istream().channels.max(e.ostream().channels))
        .max()
        .unwrap_or(1);
    let mut deps = vec![vec![false; max_ch]; max_ch];
    let mut ch_map: Vec<i32> = (0..max_ch as i32).collect();
    let mut n_merge = 0;
    let mut s = dest + 1;
    while s < effects.len() {
        let (left, right) = effects.split_at_mut(s);
        let (d, src) = (&mut left[dest], &mut right[0]);
        if !query_deps(src.as_ref(), false, &mut deps) {
            if !d.allow_remap() {
                return n_merge;
            }
            build_ch_map(&mut ch_map, &deps, src.as_ref());
        }
        let mergeable = src.merge_id() == d.merge_id()
            && src.istream().fs == d.istream().fs
            && src.ostream().fs == d.ostream().fs;
        let r = if mergeable {
            if !d.allow_remap()
                && (0..src.istream().channels).any(|k| opt_set(src.as_ref(), k) && ch_map[k] < 0)
            {
                return n_merge;
            }
            d.merge(src.as_mut(), &ch_map)
        } else {
            Merge::None
        };
        if r != Merge::None {
            n_merge += 1;
        }
        // `ec_optimize_next_src`
        if r != Merge::Full && !d.reorderable() {
            return n_merge;
        }
        mark(&mut ch_map, src.as_ref());
        if r == Merge::Full {
            effects.remove(s);
        } else {
            s += 1;
        }
    }
    n_merge
}
