//! Mix groups: tracks whose Mute and Solo (and, in the Mixer, fader and pan) move together.
//!
//! Mute and Solo can be clicked in several places (the track header, the Mixer strip, a menu),
//! so instead of each place knowing about groups, the app looks once a frame at which tracks'
//! Mute or Solo changed and makes the rest of their group follow ([`propagate`]).
use crate::model::Project;

/// What Mute and Solo were at the end of the last frame.
#[derive(Default)]
pub struct GroupWatch {
    mute: Vec<bool>,
    solo: Vec<bool>,
}

impl GroupWatch {
    /// Forgets what it saw (a project was opened): the next frame only looks.
    pub fn reset(&mut self) {
        self.mute.clear();
        self.solo.clear();
    }
}

/// Makes the tracks of a group follow a track whose Mute or Solo was changed since the last
/// call, for the groups that have that attribute.
pub fn propagate(p: &mut Project, watch: &mut GroupWatch) {
    let n = p.tracks.len();
    if watch.mute.len() != n || watch.solo.len() != n {
        // First look, or tracks came and went: nothing to compare with.
        watch.mute = p.tracks.iter().map(|t| t.mute).collect();
        watch.solo = p.tracks.iter().map(|t| t.solo).collect();
        return;
    }
    if p.groups.is_empty() {
        for (i, t) in p.tracks.iter().enumerate() {
            watch.mute[i] = t.mute;
            watch.solo[i] = t.solo;
        }
        return;
    }
    let mut changes: Vec<(usize, bool, bool)> = Vec::new();
    for i in 0..n {
        if p.tracks[i].mute != watch.mute[i] {
            changes.push((i, true, p.tracks[i].mute));
        }
        if p.tracks[i].solo != watch.solo[i] {
            changes.push((i, false, p.tracks[i].solo));
        }
    }
    for (i, is_mute, on) in changes {
        let mates = if is_mute {
            p.group_mates(i, |g| g.mute)
        } else {
            p.group_mates(i, |g| g.solo)
        };
        for j in mates {
            if is_mute {
                p.tracks[j].mute = on;
            } else {
                p.tracks[j].solo = on;
            }
        }
    }
    for (i, t) in p.tracks.iter().enumerate() {
        watch.mute[i] = t.mute;
        watch.solo[i] = t.solo;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TrackKind;

    fn project() -> Project {
        let mut p = Project::empty();
        for _ in 0..3 {
            p.add_track(TrackKind::Audio);
        }
        // Tracks 0, 1 and 2 are in one group, 3 is alone.
        let g = p.add_group("Drums").unwrap_or(0);
        for i in 0..3 {
            assert!(p.assign_group(i, Some(g)));
        }
        p
    }

    #[test]
    fn mute_and_solo_follow_the_group_and_only_when_the_group_has_them() {
        let mut p = project();
        let mut w = GroupWatch::default();
        propagate(&mut p, &mut w);
        p.tracks[1].mute = true;
        propagate(&mut p, &mut w);
        assert_eq!(
            p.tracks.iter().map(|t| t.mute).collect::<Vec<_>>(),
            [true, true, true, false]
        );
        // Un-muting one un-mutes them all.
        p.tracks[0].mute = false;
        propagate(&mut p, &mut w);
        assert!(p.tracks.iter().all(|t| !t.mute));
        p.tracks[2].solo = true;
        propagate(&mut p, &mut w);
        assert_eq!(
            p.tracks.iter().map(|t| t.solo).collect::<Vec<_>>(),
            [true, true, true, false]
        );
        // A group without Mute leaves the others alone.
        p.groups[0].mute = false;
        p.tracks[0].mute = true;
        propagate(&mut p, &mut w);
        assert_eq!(
            p.tracks.iter().map(|t| t.mute).collect::<Vec<_>>(),
            [true, false, false, false]
        );
        // A track outside every group changes alone.
        p.tracks[3].mute = true;
        propagate(&mut p, &mut w);
        assert!(!p.tracks[1].mute && p.tracks[3].mute);
        // Opening a project (reset) only looks.
        w.reset();
        p.groups[0].mute = true;
        p.tracks[1].mute = true;
        propagate(&mut p, &mut w);
        assert!(!p.tracks[2].mute);
    }

    #[test]
    fn groups_are_made_assigned_and_deleted_with_the_tracks_following() {
        let mut p = project();
        let second = p.add_group("Keys").unwrap_or(0);
        assert_eq!(second, 1);
        assert!(p.assign_group(3, Some(1)));
        assert!(!p.assign_group(3, Some(9)), "no such group");
        assert!(!p.assign_group(99, Some(0)), "no such track");
        assert_eq!(p.group_mates(0, |g| g.volume), [1, 2]);
        assert!(p.group_mates(3, |g| g.volume).is_empty());
        p.groups[0].volume = false;
        assert!(p.group_mates(0, |g| g.volume).is_empty());
        // A folder has no strip to group.
        let folder = p.add_track(TrackKind::Folder);
        assert!(!p.assign_group(folder, Some(0)));
        // Deleting the first group frees its tracks and moves the later group's number down.
        p.delete_group(0);
        assert_eq!(p.groups.len(), 1);
        assert_eq!(p.tracks[0].group, None);
        assert_eq!(p.tracks[3].group, Some(0));
        p.delete_group(5);
        // The limit.
        while p.add_group("x").is_some() {}
        assert_eq!(p.groups.len(), crate::model::MAX_GROUPS);
    }
}
