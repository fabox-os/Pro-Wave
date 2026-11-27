//! Take folders, as in Logic: recording with the Cycle on makes a new take on every pass, and
//! the passes are kept together in one region. A comp says which take plays in each stretch
//! (the last take everywhere to begin with); the notes of the comp are copied into the
//! region's own `notes`, so the engine, the Piano Roll and the Event list need no special case.

use crate::model::{CompSegment, Control, Note, Take, TakeFolder};

const EPS: f32 = 1e-4;

impl TakeFolder {
    /// A folder of `takes` over `len` beats whose comp plays the last take throughout.
    pub fn new(takes: Vec<Take>, len: f32) -> Self {
        let last = takes.len().saturating_sub(1);
        Self {
            comp: vec![CompSegment {
                start: 0.0,
                end: len,
                take: last,
            }],
            takes,
            len,
            open: false,
        }
    }

    /// The notes and controllers the comp plays: each take contributes what starts inside
    /// the stretches that use it.
    pub fn flatten(&self) -> (Vec<Note>, Vec<Control>) {
        let mut notes = Vec::new();
        let mut controls = Vec::new();
        for seg in &self.comp {
            let Some(take) = self.takes.get(seg.take) else {
                continue;
            };
            let inside = |start: f32| start >= seg.start - EPS && start < seg.end - EPS;
            notes.extend(take.notes.iter().filter(|n| inside(n.start)).copied());
            controls.extend(take.controls.iter().filter(|c| inside(c.start)).copied());
        }
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        controls.sort_by(|a, b| a.start.total_cmp(&b.start));
        (notes, controls)
    }

    /// Copies the comp into a region's own notes and controllers.
    pub fn apply(&self, notes: &mut Vec<Note>, controls: &mut Vec<Control>) {
        (*notes, *controls) = self.flatten();
    }

    /// Split at Playhead on a take folder, `at` beats from its start: each take is cut as a MIDI
    /// region is (see `region_edit::cut_notes`) and the comp is cut too, so the two folders play
    /// the same passage on either side of the cut.
    pub fn split(&self, at: f32) -> (TakeFolder, TakeFolder) {
        let at = at.clamp(0.0, self.len);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for take in &self.takes {
            let (ln, lc, rn, rc) = crate::region_edit::cut_notes(&take.notes, &take.controls, at);
            left.push(Take {
                notes: ln,
                controls: lc,
            });
            right.push(Take {
                notes: rn,
                controls: rc,
            });
        }
        let (mut left_comp, mut right_comp) = (Vec::new(), Vec::new());
        for seg in &self.comp {
            if seg.start < at - EPS {
                left_comp.push(CompSegment {
                    end: seg.end.min(at),
                    ..*seg
                });
            }
            if seg.end > at + EPS {
                right_comp.push(CompSegment {
                    start: seg.start.max(at) - at,
                    end: seg.end - at,
                    take: seg.take,
                });
            }
        }
        (
            TakeFolder {
                takes: left,
                comp: left_comp,
                len: at,
                open: self.open,
            },
            TakeFolder {
                takes: right,
                comp: right_comp,
                len: self.len - at,
                open: self.open,
            },
        )
    }

    /// Joins `right` after `left`, where the two are next to each other: take N of `right`
    /// follows take N of `left`, and the comp follows too (neighbours of one take join up). Only
    /// folders with the same number of takes join; `None` for any other.
    pub fn joined(left: &TakeFolder, right: &TakeFolder) -> Option<TakeFolder> {
        if left.takes.len() != right.takes.len() {
            return None;
        }
        let off = left.len;
        let takes = left
            .takes
            .iter()
            .zip(&right.takes)
            .map(|(a, b)| Take {
                notes: a
                    .notes
                    .iter()
                    .copied()
                    .chain(b.notes.iter().map(|n| Note {
                        start: n.start + off,
                        ..*n
                    }))
                    .collect(),
                controls: a
                    .controls
                    .iter()
                    .copied()
                    .chain(b.controls.iter().map(|c| Control {
                        start: c.start + off,
                        ..*c
                    }))
                    .collect(),
            })
            .collect();
        let mut comp = left.comp.clone();
        for seg in &right.comp {
            let seg = CompSegment {
                start: seg.start + off,
                end: seg.end + off,
                take: seg.take,
            };
            match comp.last_mut() {
                Some(prev) if prev.take == seg.take && (seg.start - prev.end).abs() < EPS => {
                    prev.end = seg.end;
                }
                _ => comp.push(seg),
            }
        }
        Some(TakeFolder {
            takes,
            comp,
            len: left.len + right.len,
            open: left.open,
        })
    }

    /// Logic's swipe on a take: the stretch `a..b` (beats) now plays `take`. The rest of the
    /// comp keeps what it had, and neighbouring stretches of the same take join up.
    pub fn swipe(&mut self, take: usize, a: f32, b: f32) {
        if take >= self.takes.len() {
            return;
        }
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        let (a, b) = (a.clamp(0.0, self.len), b.clamp(0.0, self.len));
        if b - a < EPS {
            return;
        }
        let mut out: Vec<CompSegment> = Vec::new();
        for seg in &self.comp {
            if seg.start < a - EPS {
                out.push(CompSegment {
                    end: seg.end.min(a),
                    ..*seg
                });
            }
            if seg.end > b + EPS {
                out.push(CompSegment {
                    start: seg.start.max(b),
                    ..*seg
                });
            }
        }
        out.push(CompSegment {
            start: a,
            end: b,
            take,
        });
        out.sort_by(|x, y| x.start.total_cmp(&y.start));
        let mut merged: Vec<CompSegment> = Vec::new();
        for seg in out {
            match merged.last_mut() {
                Some(prev) if prev.take == seg.take && (seg.start - prev.end).abs() < EPS => {
                    prev.end = seg.end;
                }
                _ => merged.push(seg),
            }
        }
        self.comp = merged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f32) -> Note {
        Note {
            start,
            len: 0.5,
            pitch: 60,
            vel: 100,
        }
    }

    fn folder() -> TakeFolder {
        // Take 0 plays at 0, 2, 5; take 1 at 1, 3, 6. Eight beats long.
        let take = |starts: &[f32]| Take {
            notes: starts.iter().map(|&s| note(s)).collect(),
            controls: Vec::new(),
        };
        TakeFolder::new(vec![take(&[0.0, 2.0, 5.0]), take(&[1.0, 3.0, 6.0])], 8.0)
    }

    fn starts(f: &TakeFolder) -> Vec<f32> {
        f.flatten().0.iter().map(|n| n.start).collect()
    }

    #[test]
    fn the_last_take_plays_everywhere_to_begin_with() {
        let f = folder();
        assert_eq!(f.comp.len(), 1);
        assert_eq!(starts(&f), vec![1.0, 3.0, 6.0]);
    }

    #[test]
    fn a_swipe_takes_over_only_its_stretch() {
        let mut f = folder();
        f.swipe(0, 1.5, 5.5);
        assert_eq!(
            f.comp
                .iter()
                .map(|s| (s.start, s.end, s.take))
                .collect::<Vec<_>>(),
            vec![(0.0, 1.5, 1), (1.5, 5.5, 0), (5.5, 8.0, 1)]
        );
        assert_eq!(starts(&f), vec![1.0, 2.0, 5.0, 6.0]);
    }

    #[test]
    fn swiping_backwards_and_past_the_ends_is_fine() {
        let mut f = folder();
        f.swipe(0, 20.0, -3.0);
        assert_eq!(f.comp.len(), 1);
        assert_eq!(f.comp[0].take, 0);
        assert_eq!(starts(&f), vec![0.0, 2.0, 5.0]);
    }

    #[test]
    fn neighbouring_stretches_of_one_take_join_up() {
        let mut f = folder();
        f.swipe(0, 2.0, 4.0);
        f.swipe(0, 4.0, 6.0);
        assert_eq!(f.comp.len(), 3);
        assert_eq!((f.comp[1].start, f.comp[1].end), (2.0, 6.0));
        // Swiping the same take back over everything leaves one stretch.
        f.swipe(1, 0.0, 8.0);
        assert_eq!(f.comp.len(), 1);
    }

    #[test]
    fn a_tiny_or_unknown_swipe_changes_nothing() {
        let mut f = folder();
        let before = f.clone();
        f.swipe(0, 3.0, 3.0);
        f.swipe(9, 0.0, 4.0);
        assert_eq!(f, before);
    }

    #[test]
    fn controllers_follow_the_comp_too() {
        let mut f = folder();
        f.takes[0].controls.push(Control {
            start: 0.0,
            num: 64,
            val: 127,
        });
        f.takes[1].controls.push(Control {
            start: 0.0,
            num: 64,
            val: 0,
        });
        assert_eq!(f.flatten().1[0].val, 0);
        f.swipe(0, 0.0, 4.0);
        assert_eq!(f.flatten().1[0].val, 127);
    }

    #[test]
    fn a_split_folder_plays_each_side_of_the_cut_and_joins_back_to_the_same_folder() {
        let mut f = folder();
        f.swipe(0, 1.5, 5.5);
        let (left, right) = f.split(4.0);
        assert_eq!((left.len, right.len), (4.0, 4.0));
        // The comp is cut: take 1 then take 0 on the left, take 0 then take 1 on the right.
        assert_eq!(
            right
                .comp
                .iter()
                .map(|s| (s.start, s.end, s.take))
                .collect::<Vec<_>>(),
            vec![(0.0, 1.5, 0), (1.5, 4.0, 1)]
        );
        assert_eq!(starts(&left), vec![1.0, 2.0]);
        assert_eq!(starts(&right), vec![1.0, 2.0]);
        let joined = TakeFolder::joined(&left, &right).unwrap();
        assert_eq!(joined, f, "the two pieces make the folder again");
    }

    #[test]
    fn folders_join_only_with_the_same_number_of_takes() {
        let f = folder();
        let (left, _) = f.split(4.0);
        let mut other = f.clone();
        other.takes.pop();
        assert!(TakeFolder::joined(&left, &other).is_none());
        assert!(TakeFolder::joined(&other, &left).is_none());
    }
}
