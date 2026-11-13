//! A plugin editor view attached to a native parent view that the host owns (an `NSView` on
//! macOS, an `HWND` on Windows).

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::c_void;

use vst3::Steinberg::Vst::{IEditController, IEditControllerTrait, ViewType};
use vst3::Steinberg::{
    IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInvalidArgument,
    kResultFalse, kResultOk, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use super::errors::Vst3Error;
use crate::hosted::ResizeRequest;

#[cfg(target_os = "macos")]
const PLATFORM: vst3::Steinberg::FIDString = vst3::Steinberg::kPlatformTypeNSView;
#[cfg(target_os = "windows")]
const PLATFORM: vst3::Steinberg::FIDString = vst3::Steinberg::kPlatformTypeHWND;

pub(crate) struct Editor {
    view: ComPtr<IPlugView>,
    _frame: ComWrapper<PlugFrame>,
}

impl Editor {
    /// # Safety
    ///
    /// `parent` must be a valid native view of the platform type that outlives the editor.
    pub unsafe fn attach(
        controller: &ComPtr<IEditController>,
        parent: *mut c_void,
        resize: ResizeRequest,
    ) -> Result<(Editor, (u32, u32)), Vst3Error> {
        let view = unsafe { ComPtr::from_raw(controller.createView(ViewType::kEditor)) }
            .ok_or(Vst3Error::NoEditor)?;
        if unsafe { view.isPlatformTypeSupported(PLATFORM) } != kResultOk {
            return Err(Vst3Error::NoEditor);
        }
        let frame = ComWrapper::new(PlugFrame {
            resize,
            resizing: Cell::new(false),
        });
        let frame_ptr = frame
            .as_com_ref::<IPlugFrame>()
            .map_or(std::ptr::null_mut(), |f| f.as_ptr());
        unsafe { view.setFrame(frame_ptr) };
        let result = unsafe { view.attached(parent, PLATFORM) };
        if result != kResultOk {
            unsafe { view.setFrame(std::ptr::null_mut()) };
            return Err(Vst3Error::EditorAttach(result));
        }
        let editor = Editor {
            view,
            _frame: frame,
        };
        let size = editor.size();
        Ok((editor, size))
    }

    pub fn size(&self) -> (u32, u32) {
        view_size(&self.view.as_com_ref())
    }

    pub fn can_resize(&self) -> bool {
        unsafe { self.view.canResize() == kResultOk }
    }

    #[cfg(target_os = "windows")]
    pub fn set_scale(&self, scale: f64) -> Option<(u32, u32)> {
        use vst3::Steinberg::{IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait};
        let scaling = self.view.cast::<IPlugViewContentScaleSupport>()?;
        (unsafe { scaling.setContentScaleFactor(scale as f32) } == kResultOk).then(|| self.size())
    }

    /// Offers the size the user dragged the window to. The view may adjust it, and may ask for
    /// another size while it resizes; the returned size is the view's, which the window should
    /// have.
    pub fn resize(&self, width: u32, height: u32) -> (u32, u32) {
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe { self.view.checkSizeConstraint(&mut rect) };
        if dimensions(&rect) != self.size() {
            unsafe { self.view.onSize(&mut rect) };
        }
        self.size()
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            self.view.removed();
            self.view.setFrame(std::ptr::null_mut());
        }
    }
}

fn view_size(view: &ComRef<'_, IPlugView>) -> (u32, u32) {
    // SAFETY: ViewRect is plain C data the view fills in.
    let mut rect: ViewRect = unsafe { std::mem::zeroed() };
    unsafe { view.getSize(&mut rect) };
    dimensions(&rect)
}

/// The width and height of a rectangle the plugin filled in. Its coordinates can be any `i32`s,
/// whose difference only fits a wider type.
fn dimensions(rect: &ViewRect) -> (u32, u32) {
    let length = |start: i32, end: i32| (i64::from(end) - i64::from(start)).max(0) as u32;
    (length(rect.left, rect.right), length(rect.top, rect.bottom))
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;

struct PlugFrame {
    resize: ResizeRequest,
    /// The host window is being resized for the view. A view that asks again from the resize
    /// it sees is refused, as the SDK's host does, instead of resizing the window inside itself.
    resizing: Cell<bool>,
}

impl Class for PlugFrame {
    type Interfaces = (IPlugFrame,);
}

impl IPlugFrameTrait for PlugFrame {
    /// Resizes the window, then the view if it does not have the size yet. The view is told
    /// only a size the window got.
    unsafe fn resizeView(&self, view: *mut IPlugView, newSize: *mut ViewRect) -> tresult {
        let Some(view) = (unsafe { ComRef::from_raw(view) }) else {
            return kInvalidArgument;
        };
        if newSize.is_null() {
            return kInvalidArgument;
        }
        if self.resizing.get() {
            return kResultFalse;
        }
        let mut rect = unsafe { *newSize };
        let size = dimensions(&rect);
        if view_size(&view) == size {
            return kResultOk;
        }
        self.resizing.set(true);
        let resized = (self.resize)(size.0, size.1);
        self.resizing.set(false);
        if !resized {
            return kResultFalse;
        }
        if view_size(&view) != size {
            unsafe { view.onSize(&mut rect) };
        }
        kResultOk
    }
}
