//! Editor views. A version 2 unit's Cocoa view comes from the view factory the unit names, new
//! for every opening: the v2 bridge hands out its view controller once, and that view does not
//! survive being taken out of its window. Other units give their view controller on request.
//! Units change the size of their views themselves, so an open editor's size is followed.
use std::ffi::{CString, c_void};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::{NSView, NSViewController};
use objc2_audio_toolbox::{
    AUAudioUnitV2Bridge, AudioUnit, AudioUnitGetProperty, AudioUnitGetPropertyInfo,
    kAudioUnitProperty_CocoaUI, kAudioUnitScope_Global,
};
use objc2_core_audio_kit::{AUAudioUnit_ViewController, AUViewControllerBase};
use objc2_core_foundation::{CFBundle, CFRetained, CFString, CFURL};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::{AuError, Handoff, Plugin, Slot};
use crate::ResizeRequest;

/// An open editor: its view in the host's window, and the view controller that owns the view.
pub(super) struct Editor {
    view: Retained<NSView>,
    controller: Option<Retained<NSViewController>>,
    /// The view size the host last gave its window.
    size: NSSize,
    /// The view controller's preferred content size when last read.
    preferred: NSSize,
    resize: ResizeRequest,
}

impl Plugin {
    /// Adds the Audio Unit's view (a v3 view, or a v2 Cocoa view) to `parent`. [`Plugin::idle`]
    /// passes the view's later sizes to `resize`.
    ///
    /// # Safety
    ///
    /// `parent` must be a valid `NSView` that outlives the editor; call on the main thread.
    pub(crate) unsafe fn open_editor(
        &mut self,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<(u32, u32), AuError> {
        self.close_editor();
        MainThreadMarker::new().ok_or(AuError::NoEditor)?;
        let (view, controller, preferred) = match self.cocoa_view()? {
            Some(view) => (view, None, NSSize::new(0.0, 0.0)),
            None => {
                let controller = self.view_controller()?;
                let preferred = controller.preferredContentSize();
                (controller.view(), Some(controller), preferred)
            }
        };
        let size = if has_area(preferred) {
            preferred
        } else {
            view.frame().size
        };
        view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        let parent = unsafe { &*(parent as *const NSView) };
        // The window follows the view; resizing it must not resize the view in turn.
        parent.setAutoresizesSubviews(false);
        parent.addSubview(&view);
        self.editor = Some(Editor {
            view,
            controller,
            size,
            preferred,
            resize,
        });
        Ok(pixels(size))
    }

    /// Passes a new size of the open editor's view to its window: the size a v3 view controller
    /// prefers when that changes, or the size a view gave itself.
    pub(crate) fn idle(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if let Some(controller) = &editor.controller {
            let preferred = controller.preferredContentSize();
            if preferred != editor.preferred {
                editor.preferred = preferred;
                if has_area(preferred) {
                    editor.view.setFrameSize(preferred);
                }
            }
        }
        let size = editor.view.frame().size;
        if size != editor.size {
            editor.size = size;
            let (width, height) = pixels(size);
            (editor.resize)(width, height);
        }
    }

    pub(crate) fn close_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            editor.view.removeFromSuperview();
        }
    }

    /// Whether the unit runs in its extension's process, which then provides its view and panels:
    /// a version 3 unit loads there unless it was registered in this process. A unit that went
    /// away is in none; opening its editor fails.
    pub(crate) fn editor_in_extension(&self) -> bool {
        self.shared_unit()
            .is_ok_and(|unit| !unsafe { unit.isLoadedInProcess() })
    }

    fn view_controller(&self) -> Result<Retained<NSViewController>, AuError> {
        let slot: Slot<Handoff<Option<Retained<AUViewControllerBase>>>> = Slot::new();
        let fill = slot.clone();
        let completion = RcBlock::new(move |controller: *mut AUViewControllerBase| {
            fill.fill(Handoff(unsafe { Retained::retain(controller) }));
        });
        let unit = self.shared_unit()?;
        unsafe { unit.requestViewControllerWithCompletionHandler(&completion) };
        slot.wait().0.ok_or(AuError::NoEditor)
    }

    /// A new view from the view factory a version 2 unit names in `kAudioUnitProperty_CocoaUI`,
    /// or `None` for other units.
    fn cocoa_view(&self) -> Result<Option<Retained<NSView>>, AuError> {
        let unit = self.shared_unit()?;
        let Some(bridge) = unit.downcast_ref::<AUAudioUnitV2Bridge>() else {
            return Ok(None);
        };
        let unit = unsafe { bridge.audioUnit() };
        let mut bytes = 0u32;
        let status = unsafe {
            AudioUnitGetPropertyInfo(
                unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                &mut bytes,
                std::ptr::null_mut(),
            )
        };
        let url = size_of::<NonNull<CFURL>>();
        let name = size_of::<NonNull<CFString>>();
        let count = (bytes as usize).saturating_sub(url) / name;
        if status != 0 || count == 0 {
            return Ok(None);
        }
        // AudioUnitCocoaViewInfo with `count` class names; pointer-aligned storage.
        let mut storage = vec![std::ptr::null_mut::<c_void>(); 1 + count];
        let status = unsafe {
            AudioUnitGetProperty(
                unit,
                kAudioUnitProperty_CocoaUI,
                kAudioUnitScope_Global,
                0,
                NonNull::new_unchecked(storage.as_mut_ptr().cast()),
                NonNull::from(&mut bytes),
            )
        };
        if status != 0 {
            return Ok(None);
        }
        // SAFETY: the property returned retained CF objects, owned by this call.
        let Some((location, name)) = (unsafe { cocoa_metadata(&storage) }) else {
            return Ok(None);
        };
        let Some(bundle) = CFBundle::new(None, Some(&location)) else {
            return Ok(None);
        };
        // SAFETY: loads the view bundle the unit names, as any host showing its view does.
        if !unsafe { bundle.load_executable() } {
            return Ok(None);
        }
        let Some(class) = CString::new(name.to_string())
            .ok()
            .and_then(|name| AnyClass::get(&name))
        else {
            return Ok(None);
        };
        Ok(factory_view(class, unit))
    }
}

/// A view of `unit` from the Cocoa view factory `class`; `None` when the class does not make
/// views or makes no factory.
fn factory_view(class: &AnyClass, unit: AudioUnit) -> Option<Retained<NSView>> {
    if !class.responds_to(sel!(uiViewForAudioUnit:withSize:)) {
        return None;
    }
    let factory: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
    unsafe { msg_send![&factory?, uiViewForAudioUnit: unit, withSize: NSSize::new(0.0, 0.0)] }
}

fn has_area(size: NSSize) -> bool {
    size.width > 0.0 && size.height > 0.0
}

fn pixels(size: NSSize) -> (u32, u32) {
    (size.width.round() as u32, size.height.round() as u32)
}

/// Takes ownership of the CF objects returned by the native Cocoa UI property; `None` when the
/// bundle or every class name is null, which is possible even when the property call succeeds.
///
/// # Safety
/// Each non-null pointer must be the retained CFURL (first entry) or CFString (remaining
/// entries) returned by the unit. The slice has at least one class-name entry.
unsafe fn cocoa_metadata(
    storage: &[*mut c_void],
) -> Option<(CFRetained<CFURL>, CFRetained<CFString>)> {
    // Read nullable pointers from raw storage: the bindings' NonNull fields cannot represent
    // a unit's malformed property. Still release every non-null object when one is missing.
    let location = NonNull::new(storage[0].cast::<CFURL>())
        .map(|location| unsafe { CFRetained::from_raw(location) });
    let names: Vec<CFRetained<CFString>> = storage[1..]
        .iter()
        .filter_map(|&name| NonNull::new(name.cast::<CFString>()))
        .map(|name| unsafe { CFRetained::from_raw(name) })
        .collect();
    location.zip(names.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::rc::Allocated;
    use objc2::runtime::NSObject;
    use objc2::{ClassType, define_class};

    define_class!(
        /// Names a view factory but is none.
        #[unsafe(super(NSObject))]
        struct NotAViewFactory;
    );

    define_class!(
        /// A view factory that cannot be made.
        #[unsafe(super(NSObject))]
        struct UnmadeViewFactory;

        impl UnmadeViewFactory {
            #[unsafe(method_id(init))]
            fn init(_this: Allocated<Self>) -> Option<Retained<Self>> {
                None
            }

            #[unsafe(method_id(uiViewForAudioUnit:withSize:))]
            fn view(&self, _unit: AudioUnit, _size: NSSize) -> Option<Retained<NSView>> {
                None
            }
        }
    );

    #[test]
    fn a_class_that_makes_no_views_is_not_an_editor() {
        let class = NotAViewFactory::class();
        assert!(factory_view(class, std::ptr::null_mut()).is_none());
    }

    #[test]
    fn a_view_factory_that_cannot_be_made_is_not_an_editor() {
        let class = UnmadeViewFactory::class();
        assert!(factory_view(class, std::ptr::null_mut()).is_none());
    }

    fn location() -> *mut c_void {
        CFRetained::into_raw(CFBundle::main_bundle().unwrap().bundle_url().unwrap())
            .as_ptr()
            .cast()
    }

    #[test]
    fn cocoa_property_with_no_class_name_is_not_an_editor() {
        let storage = [location(), std::ptr::null_mut(), std::ptr::null_mut()];
        assert!(unsafe { cocoa_metadata(&storage) }.is_none());
    }

    #[test]
    fn cocoa_property_with_no_bundle_is_not_an_editor() {
        let name = CFRetained::into_raw(CFString::from_str("ViewFactory"))
            .as_ptr()
            .cast();
        assert!(unsafe { cocoa_metadata(&[std::ptr::null_mut(), name]) }.is_none());
    }

    #[test]
    fn cocoa_property_uses_the_first_nonnull_class_name() {
        let name = CFRetained::into_raw(CFString::from_str("ViewFactory"))
            .as_ptr()
            .cast();
        let storage = [location(), std::ptr::null_mut(), name];
        let (_, name) = unsafe { cocoa_metadata(&storage) }.unwrap();
        assert_eq!(name.to_string(), "ViewFactory");
    }
}
