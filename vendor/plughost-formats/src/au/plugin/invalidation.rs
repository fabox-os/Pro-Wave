//! Native notification callbacks only publish a sticky flag, never call the invalid unit.
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_audio_toolbox::{AUAudioUnit, kAudioComponentInstanceInvalidationNotification};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSString};
use std::ptr::NonNull;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Observation {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    _callback: RcBlock<dyn Fn(NonNull<NSNotification>)>,
}

impl Observation {
    pub fn new(unit: &AUAudioUnit, invalidated: Arc<AtomicBool>) -> Self {
        let center = NSNotificationCenter::defaultCenter();
        let callback = RcBlock::new(move |_: NonNull<NSNotification>| {
            invalidated.store(true, Ordering::Release);
        });
        // SAFETY: Apple's notification name is a toll-free bridged CFString/NSString.
        let name = unsafe {
            &*(kAudioComponentInstanceInvalidationNotification
                as *const objc2_core_foundation::CFString)
                .cast::<NSString>()
        };
        // SAFETY: observation is restricted to this unit; the callback contains only Send state.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(name),
                Some(unit),
                None,
                &callback,
            )
        };
        Self {
            center,
            token,
            _callback: callback,
        }
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        // The callback owns its Arc even if a concurrent notification is already in flight.
        unsafe { self.center.removeObserver((*self.token).as_ref()) };
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Plugin, lock};
    use super::*;
    use plughost_core::{BlockContext, Events, FailureKind, Layout, ProcessConfig, SampleFormat};

    fn unit(plugin: &Plugin) -> Retained<AUAudioUnit> {
        lock(&plugin.engine).unit.as_ref().unwrap().clone()
    }
    fn post(unit: &AUAudioUnit) {
        let name = unsafe {
            &*(kAudioComponentInstanceInvalidationNotification
                as *const objc2_core_foundation::CFString)
                .cast::<NSString>()
        };
        unsafe {
            NSNotificationCenter::defaultCenter().postNotificationName_object(name, Some(unit))
        };
    }
    fn plugin() -> Plugin {
        // SAFETY: the tests load only the repository's fixtures and Apple's own units.
        let mut plugin = unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
        plugin
            .prepare(&ProcessConfig::new(
                48_000.0,
                64,
                SampleFormat::F32,
                Layout::Stereo,
                Layout::Stereo,
            ))
            .unwrap();
        plugin
    }

    #[test]
    fn invalidation_is_instance_scoped_sticky_and_rejects_processing_without_output_changes() {
        let mut invalid = plugin();
        let healthy = plugin();
        let processor = invalid.processor();
        let host_unit = unit(&invalid);
        post(&host_unit);
        post(&host_unit);
        let mut left = [17.0f32; 32];
        let mut right = [23.0f32; 32];
        let input = [0.0f32; 32];
        let result = invalid.process(
            &BlockContext::new(32),
            &[&input, &input],
            &mut [&mut left, &mut right],
            &[],
            &Events::new(),
            &mut Events::for_block(),
        );
        assert!(result.is_err(), "an invalidated instance must not render");
        assert_eq!(result.unwrap_err().kind(), FailureKind::RestartRequired);
        assert_eq!(left, [17.0; 32]);
        assert_eq!(right, [23.0; 32]);
        assert!(crate::BlockProcessor::restart_required(&processor));
        assert!(!crate::BlockProcessor::restart_required(
            &healthy.processor()
        ));
        assert!(healthy.timing().is_ok());
        assert_eq!(
            invalid
                .save_state(plughost_core::StatePurpose::Project)
                .unwrap_err()
                .kind(),
            FailureKind::RestartRequired
        );
        assert_eq!(
            invalid.reset().unwrap_err().kind(),
            FailureKind::RestartRequired
        );
        drop(invalid);
        post(&host_unit); // Observer removal must tolerate a retained native object after Plugin drop.
    }
}
