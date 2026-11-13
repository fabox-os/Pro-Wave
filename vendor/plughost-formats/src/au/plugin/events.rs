//! Native automation callbacks own only a bounded queue; all AU queries stay on the owner.
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_audio_toolbox::{
    AUAudioUnit, AUAudioUnitV2Bridge, AUParameterAutomationEvent, AUParameterAutomationEventType,
    AUParameterObserverToken, AUParameterTree, AudioUnit, AudioUnitAddPropertyListener,
    AudioUnitElement, AudioUnitPropertyID, AudioUnitRemovePropertyListenerWithUserData,
    AudioUnitScope, kAudioUnitProperty_ParameterValueStrings,
};
use objc2_foundation::NSInteger;
use plughost_core::{ParameterEvent, ParameterEventBatch};

use super::{Plugin, lock, ranges};

const CAPACITY: usize = plughost_core::PARAMETER_EVENT_CAPACITY;
type Callback = dyn Fn(NSInteger, NonNull<AUParameterAutomationEvent>);

struct Pending {
    events: VecDeque<AUParameterAutomationEvent>,
    dropped: u64,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            events: VecDeque::with_capacity(CAPACITY),
            dropped: 0,
        }
    }
}

impl Pending {
    fn record(&mut self, event: AUParameterAutomationEvent) {
        if self.events.len() == CAPACITY {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(event);
    }
}

pub(super) struct Observation {
    tree: Retained<AUParameterTree>,
    token: AUParameterObserverToken,
    pending: Arc<Mutex<Pending>>,
    _callback: RcBlock<Callback>,
}

impl Observation {
    fn new(tree: Retained<AUParameterTree>) -> Self {
        let pending = Arc::new(Mutex::new(Pending::default()));
        let sink = Arc::clone(&pending);
        let callback = RcBlock::new(
            move |count: NSInteger, events: NonNull<AUParameterAutomationEvent>| {
                let Ok(count) = usize::try_from(count) else {
                    return;
                };
                let mut pending = sink.lock().unwrap_or_else(|error| error.into_inner());
                // SAFETY: AU owns the event array for the duration of this callback.
                for event in unsafe { std::slice::from_raw_parts(events.as_ptr(), count) } {
                    pending.record(*event);
                }
            },
        );
        let token =
            unsafe { tree.tokenByAddingParameterAutomationObserver(RcBlock::as_ptr(&callback)) };
        Self {
            tree,
            token,
            pending,
            _callback: callback,
        }
    }

    pub fn token(&self) -> AUParameterObserverToken {
        self.token
    }

    fn take(&self) -> Pending {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        // Native removal waits for in-flight callbacks. The retained tree and callback must
        // remain alive until it returns; no callback acquires the engine lock.
        unsafe { self.tree.removeParameterObserver(self.token) };
    }
}

/// Follows a v2 unit's `kAudioUnitProperty_ParameterValueStrings`. The bridge replaces its tree
/// when the unit announces a changed parameter list or parameter info, but keeps it, value
/// strings included, when only value strings change.
pub(super) struct ValueStrings {
    unit: AudioUnit,
    changed: Box<AtomicBool>,
}

impl ValueStrings {
    /// None for v3 units, or when the unit does not take the listener.
    pub fn new(unit: &AUAudioUnit) -> Option<Self> {
        let unit = unsafe { unit.downcast_ref::<AUAudioUnitV2Bridge>()?.audioUnit() };
        let changed = Box::new(AtomicBool::new(false));
        let status = unsafe {
            AudioUnitAddPropertyListener(
                unit,
                kAudioUnitProperty_ParameterValueStrings,
                Some(value_strings_changed),
                std::ptr::from_ref(&*changed).cast_mut().cast(),
            )
        };
        (status == 0).then_some(Self { unit, changed })
    }

    fn take(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

/// Runs on whichever thread the unit announces the change from; it only raises the flag.
unsafe extern "C-unwind" fn value_strings_changed(
    changed: NonNull<c_void>,
    _: AudioUnit,
    _: AudioUnitPropertyID,
    _: AudioUnitScope,
    _: AudioUnitElement,
) {
    // SAFETY: the listener is removed before its flag is freed.
    unsafe { changed.cast::<AtomicBool>().as_ref() }.store(true, Ordering::Relaxed);
}

impl Drop for ValueStrings {
    fn drop(&mut self) {
        // The unit must still exist; the plugin drops this before releasing it.
        unsafe {
            AudioUnitRemovePropertyListenerWithUserData(
                self.unit,
                kAudioUnitProperty_ParameterValueStrings,
                Some(value_strings_changed),
                std::ptr::from_ref(&*self.changed).cast_mut().cast(),
            )
        };
    }
}

impl Plugin {
    /// Follows the unit's parameter tree. A unit announces changed parameters by replacing its
    /// tree (the v2 bridge does so on the main thread's run loop for
    /// `kAudioUnitProperty_ParameterList` and `kAudioUnitProperty_ParameterInfo`); a replacement
    /// rebinds the observer and refreshes the processing thread's parameter ranges.
    pub(super) fn refresh_parameter_observer(&mut self) {
        let unit = self.shared_unit().ok();
        let tree = unit
            .as_ref()
            .and_then(|unit| unsafe { unit.parameterTree() });
        let replaced = match (&self.observation, &tree) {
            (Some(old), Some(new)) => !std::ptr::eq(&*old.tree, &**new),
            (None, None) => false,
            _ => true,
        };
        if !replaced {
            return;
        }
        self.observation = None;
        self.observation = tree.map(Observation::new);
        self.parameter_metadata_changed = true;
        if let Some(unit) = unit {
            let ranges = ranges(&unit);
            if let Some(prepared) = &mut lock(&self.engine).prepared {
                prepared.ranges = ranges;
            }
        }
    }

    /// Drains native value/gesture events and reports a replaced parameter tree or changed value
    /// strings. AU has no general dirty notification, so this method never synthesizes a dirty
    /// event from unrelated callbacks.
    pub(crate) fn take_parameter_events(&mut self) -> ParameterEventBatch {
        self.refresh_parameter_observer();
        let value_strings = self.value_strings.as_ref().is_some_and(ValueStrings::take);
        let changed = std::mem::take(&mut self.parameter_metadata_changed) || value_strings;
        let pending = self
            .observation
            .as_ref()
            .map(Observation::take)
            .unwrap_or_default();
        let mut batch =
            ParameterEventBatch::new(Vec::new(), changed || pending.dropped != 0, pending.dropped);
        if changed {
            batch.events.push(ParameterEvent::MetadataChanged);
        }
        for event in pending.events {
            let id = event.address;
            if event.eventType == AUParameterAutomationEventType::Value
                && let Some(sent) = self.host_edits.get_mut(&id)
            {
                // The v2 bridge reports this host's own edits back, each stamped just after this
                // host notified it and in order; its value tells nothing, as it is the one the
                // parameter has by then. A report stamped within the stamp delay of the oldest
                // edit waiting is that edit's; edits whose reports could no longer come are done.
                // Any other report came from the unit.
                let stamp = plughost_core::ipc::shared::clock::ticks(super::ECHO_STAMP);
                while sent
                    .front()
                    .is_some_and(|&notified| notified.saturating_add(stamp) < event.hostTime)
                {
                    sent.pop_front();
                }
                if sent
                    .front()
                    .is_some_and(|&notified| notified <= event.hostTime)
                {
                    sent.pop_front();
                    continue;
                }
            }
            let mapped = match event.eventType {
                AUParameterAutomationEventType::Touch => Some(ParameterEvent::BeginEdit { id }),
                AUParameterAutomationEventType::Release => Some(ParameterEvent::EndEdit { id }),
                AUParameterAutomationEventType::Value => self
                    .reported_normalized(id, f64::from(event.value))
                    .map(|normalized| ParameterEvent::Value { id, normalized }),
                _ => None,
            };
            if let Some(event) = mapped {
                batch.events.push(event);
            } else {
                batch.dropped = batch.dropped.saturating_add(1);
                batch.resync_required = true;
            }
        }
        if batch.events.len() > CAPACITY {
            let excess = batch.events.len() - CAPACITY;
            // The metadata change first is kept: the helper refreshes metadata only for one.
            let kept = usize::from(changed);
            batch.events.drain(kept..kept + excess);
            batch.dropped = batch.dropped.saturating_add(excess as u64);
            batch.resync_required = true;
        }
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_edits_are_not_reported_back_as_edits_of_the_unit() {
        // AUDelay is a v2 unit; its bridge reports listener notifications some 50 ms later.
        // SAFETY: the tests load only the repository's fixtures and Apple's own units.
        let mut plugin = unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
        let _ = plugin.take_parameter_events();
        plugin.set_parameter(0, 0.3).unwrap();
        crate::au::wait::run_loop_for(std::time::Duration::from_millis(200));
        assert_eq!(plugin.take_parameter_events().events, []);
    }

    #[test]
    fn quick_host_edits_are_not_reported_back_either() {
        // SAFETY: the tests load only the repository's fixtures and Apple's own units.
        let mut plugin = unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
        let _ = plugin.take_parameter_events();
        for value in [0.2, 0.4, 0.6] {
            plugin.set_parameter(0, value).unwrap();
        }
        crate::au::wait::run_loop_for(std::time::Duration::from_millis(200));
        assert_eq!(plugin.take_parameter_events().events, []);
    }

    #[test]
    fn an_edit_the_unit_makes_after_quick_host_edits_is_reported() {
        // SAFETY: the tests load only the repository's fixtures and Apple's own units.
        let mut plugin = unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
        let _ = plugin.take_parameter_events();
        // The bridge reports each of these back with the value the parameter has by then, 0.2.
        for value in [0.2, 0.4, 0.2] {
            plugin.set_parameter(0, value).unwrap();
        }
        crate::au::wait::run_loop_for(std::time::Duration::from_millis(200));
        assert_eq!(plugin.take_parameter_events().events, []);
        // The unit sets 0.4 itself, as its own editor would, and tells its listeners.
        let unit = plugin.shared_unit().unwrap();
        let audio_unit = unsafe {
            unit.downcast_ref::<AUAudioUnitV2Bridge>()
                .unwrap()
                .audioUnit()
        };
        let plain = plugin.parameter_to_plain(0, 0.4).unwrap() as f32;
        let mut target = objc2_audio_toolbox::AudioUnitParameter {
            mAudioUnit: audio_unit,
            mParameterID: 0,
            mScope: objc2_audio_toolbox::kAudioUnitScope_Global,
            mElement: 0,
        };
        unsafe {
            objc2_audio_toolbox::AudioUnitSetParameter(
                audio_unit,
                0,
                objc2_audio_toolbox::kAudioUnitScope_Global,
                0,
                plain,
                0,
            );
            objc2_audio_toolbox::AUParameterListenerNotify(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                NonNull::from(&mut target),
            );
        }
        crate::au::wait::run_loop_for(std::time::Duration::from_millis(200));
        let events = plugin.take_parameter_events().events;
        assert!(
            matches!(events.as_slice(), [ParameterEvent::Value { id: 0, normalized }]
                if (normalized - 0.4).abs() < 1e-6),
            "{events:?}"
        );
    }

    #[test]
    fn a_v2_unit_can_be_dropped_right_after_an_edit() {
        // AudioToolbox crashed when the unit went away while it set up the listener notification.
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..2 {
                        // SAFETY: the tests load only the repository's fixture and SDK sample
                        // plugins.
                        let mut plugin =
                            unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
                        plugin.set_parameter(0, 0.75).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn observer_queue_preserves_recent_gestures_and_reports_overflow() {
        let mut pending = Pending::default();
        for index in 0..CAPACITY + 3 {
            pending.record(AUParameterAutomationEvent {
                hostTime: index as u64,
                address: 7,
                value: index as f32,
                eventType: AUParameterAutomationEventType::Value,
                reserved: 0,
            });
        }
        assert_eq!(pending.dropped, 3);
        assert_eq!(pending.events.len(), CAPACITY);
        assert_eq!(pending.events.front().unwrap().hostTime, 3);
        assert_eq!(
            pending.events.back().unwrap().hostTime,
            (CAPACITY + 2) as u64
        );
    }
}
