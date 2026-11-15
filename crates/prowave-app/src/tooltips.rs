//! Hover tooltips for the control bar and the M / S / R / I buttons of the track headers and
//! the Mixer: a short box that says what a button does (issue #118). Logic Pro has no tooltips
//! on these; Pro Wave adds them because a toolbar of bare icons is hard to learn.
//!
//! The texts are English here and translated through [`crate::i18n::tr`] when shown, so each
//! one is in every language file.

/// What the control bar button with key `key` does (the keys are the ones `transport.rs` gives
/// its buttons; a segmented group's items are its key and their number).
pub fn bar_tip(key: &str) -> Option<&'static str> {
    Some(match key {
        "view0" => "Show or hide the Library",
        "view1" => "Show or hide the Inspector",
        "view2" => "Show or hide Quick Help",
        "view3" => "Show or hide the Toolbar",
        "panes0" => "Switch the menus between dark and light",
        "panes1" => "Show or hide the Mixer",
        "panes2" => "Editors: show or hide the Piano Roll and the other editors",
        "rew" => "Go to the previous bar",
        "ffw" => "Go to the next bar",
        "start" => "Go to the start of the project",
        "play" => "Play or stop (Space)",
        "rec" => "Record",
        "capture" => "Capture Recording: keep what you just played",
        "cycle" => "Cycle: repeat the cycle section",
        "lowlat" => "Low Latency Mode",
        "tuner" => "Tuner",
        "countin" => "Count-in: play a bar of clicks before recording",
        "metronome" => "Metronome: play a click",
        "right0" => "List Editors",
        "right1" => "Note Pads",
        "right2" => "Loop Browser",
        "right3" => "File Browser",
        _ => return None,
    })
}

/// What a track header or Mixer strip button with this label does.
pub fn button_tip(label: &str) -> Option<&'static str> {
    Some(match label {
        "M" => "Mute",
        "S" => "Solo",
        "R" => "Record Enable: arm the track for recording",
        "I" => "Input Monitoring: hear the input of the track",
        _ => return None,
    })
}

/// Every tooltip text, for the translation check.
#[cfg(test)]
pub fn all_texts() -> Vec<&'static str> {
    let mut out = Vec::new();
    let keys = [
        "view0",
        "view1",
        "view2",
        "view3",
        "panes0",
        "panes1",
        "panes2",
        "rew",
        "ffw",
        "start",
        "play",
        "rec",
        "capture",
        "cycle",
        "lowlat",
        "tuner",
        "countin",
        "metronome",
        "right0",
        "right1",
        "right2",
        "right3",
    ];
    out.extend(keys.iter().filter_map(|k| bar_tip(k)));
    out.extend(["M", "S", "R", "I"].iter().filter_map(|l| button_tip(l)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_button_of_the_bar_has_a_tip_and_unknown_keys_have_none() {
        assert_eq!(all_texts().len(), 26);
        assert_eq!(bar_tip("nope"), None);
        assert_eq!(button_tip("X"), None);
        let texts = all_texts();
        for (i, t) in texts.iter().enumerate() {
            assert!(
                !t.is_empty() && texts[i + 1..].iter().all(|u| u != t),
                "{t}"
            );
        }
    }
}
