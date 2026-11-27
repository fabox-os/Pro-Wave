//! The chain's optimiser (`ec_optimize` of the original): effects that can be merged
//! (`reverse_iir` here) are joined into one with the next ones of their kind, as long as nothing
//! between them touches the same channels in a way that depends on the order.

use crate::align::query_deps;
use crate::effect::Effect;

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

fn opt_set(e: &dyn Effect, k: usize) -> bool {
    e.opt_selector()
        .is_none_or(|s| s.get(k).copied().unwrap_or(false))
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
    // channels that an effect in between pins down are marked -1
    let mut ch_map: Vec<i32> = (0..max_ch as i32).collect();
    let mut n_merge = 0;
    let mut s = dest + 1;
    while s < effects.len() {
        // The destination does not allow remapping: stop at an effect that moves channels.
        if !query_deps(effects[s].as_ref(), false, &mut deps) {
            return n_merge;
        }
        let (left, right) = effects.split_at_mut(s);
        let (d, src) = (&mut left[dest], &mut right[0]);
        let mergeable = src.merge_id() == d.merge_id()
            && src.istream().fs == d.istream().fs
            && src.ostream().fs == d.ostream().fs;
        let mut merged = false;
        if mergeable {
            if (0..src.istream().channels).any(|k| opt_set(src.as_ref(), k) && ch_map[k] < 0) {
                return n_merge;
            }
            if let Some(data) = src.merge_take() {
                merged = d.merge_put(data);
            }
        }
        let src_reorderable = src.reorderable();
        if merged {
            n_merge += 1;
            if !src_reorderable {
                mark(&mut ch_map, effects[s].as_ref());
            }
            effects.remove(s);
        } else {
            if !d.reorderable() {
                return n_merge;
            }
            if !src_reorderable {
                mark(&mut ch_map, effects[s].as_ref());
            }
            s += 1;
        }
    }
    n_merge
}

fn mark(ch_map: &mut [i32], src: &dyn Effect) {
    let n = src.istream().channels;
    for (k, m) in ch_map.iter_mut().enumerate().take(n) {
        if opt_set(src, k) {
            *m = -1;
        }
    }
}
