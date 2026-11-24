//! The plug-in's window on Linux/X11, in the plug-in's own process. Pro Wave owns the top-level
//! window and its 28-pixel footer with a power button; the plug-in draws only inside its parented
//! view. A missing or unusable `DISPLAY` leaves the editor unavailable so the app shows sliders.
// Temporary: Task 6 is the first step that consumes this helper. Remove with that step.
#![allow(dead_code)]

use crate::EditorParent;
use std::cell::Cell;
use std::env;
use std::ffi::{c_int, c_long, c_uchar, c_void, CStr, CString, OsStr};
use std::os::raw::c_ulong;
use std::sync::atomic::{AtomicBool, Ordering};
use x11_dl::xlib::{self, Xlib};

const FOOTER: c_int = 28;
const POWER_X: c_int = 12;
const POWER_Y: c_int = 4;
const POWER_W: u32 = 28;
const POWER_H: u32 = 20;

/// Whether an X11 display was named. An empty name means "no display", as does no name.
pub(crate) fn available_from_env(display: Option<&OsStr>) -> bool {
    display.is_some_and(|name| !name.is_empty())
}

/// Latched window events. The `note_` methods are also the test injector: they let the event
/// decoder be checked without opening a display.
#[derive(Default)]
pub(crate) struct EventState {
    closed: AtomicBool,
    power: AtomicBool,
}

impl EventState {
    pub(crate) fn note_close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }

    pub(crate) fn note_power_click(&self) {
        self.power.store(true, Ordering::Relaxed);
    }

    pub(crate) fn take_closed(&self) -> bool {
        self.closed.swap(false, Ordering::Relaxed)
    }

    pub(crate) fn take_power_click(&self) -> bool {
        self.power.swap(false, Ordering::Relaxed)
    }
}

/// Allocates a colour for the host chrome, falling back to black on a display without one.
fn color(
    xlib: &Xlib,
    display: *mut xlib::Display,
    colormap: xlib::Colormap,
    fallback: c_ulong,
    red: u8,
    green: u8,
    blue: u8,
) -> c_ulong {
    let mut color = xlib::XColor {
        pixel: 0,
        red: u16::from(red) * 257,
        green: u16::from(green) * 257,
        blue: u16::from(blue) * 257,
        flags: xlib::DoRed | xlib::DoGreen | xlib::DoBlue,
        pad: 0,
    };
    // SAFETY: `display` is open and `colormap` belongs to its default screen; `color` is an
    // initialized XColor owned by this call.
    let ok = unsafe { (xlib.XAllocColor)(display, colormap, &mut color) };
    if ok == 0 {
        fallback
    } else {
        color.pixel
    }
}

/// Interns one atom on `display`. The names here are static C strings, so they cannot fail.
fn atom(xlib: &Xlib, display: *mut xlib::Display, name: &CStr) -> xlib::Atom {
    // SAFETY: `display` is open and `name` is NUL-terminated; `XInternAtom` creates it if needed.
    unsafe { (xlib.XInternAtom)(display, name.as_ptr(), 0) }
}

/// A top-level X11 window for one plug-in editor. Every call here is on the process's main
/// thread; the plug-in process owns the display connection for its whole life.
pub(crate) struct Window {
    xlib: Xlib,
    display: *mut xlib::Display,
    window: xlib::Window,
    parent: xlib::Window,
    footer: xlib::Window,
    power: xlib::Window,
    wm_delete: xlib::Atom,
    xembed: xlib::Atom,
    xembed_info: xlib::Atom,
    utf8: xlib::Atom,
    state: EventState,
    size: Cell<(u32, u32)>,
    embedded: Cell<Option<xlib::Window>>,
}

impl Window {
    /// A hidden window for the plug-in called `title`, or `None` if there is no usable display.
    pub(crate) fn new(title: &str) -> Option<Window> {
        let display_name = env::var_os("DISPLAY")?;
        if !available_from_env(Some(&display_name)) {
            return None;
        }
        let display_name = display_name.into_string().ok()?;
        let name = CString::new(title).ok()?;
        let xlib = Xlib::open().ok()?;
        // SAFETY: the display name is a valid C string and `XOpenDisplay` returns either a live
        // connection or null. Every call below uses that connection only on the main thread.
        let display = unsafe { (xlib.XOpenDisplay)(CString::new(display_name).ok()?.as_ptr()) };
        if display.is_null() {
            return None;
        }
        // SAFETY: the connection is open; these accessors only read its screen and colormap.
        let (colormap, black) = unsafe {
            let screen = (xlib.XDefaultScreen)(display);
            (
                (xlib.XDefaultColormap)(display, screen),
                (xlib.XBlackPixel)(display, screen),
            )
        };
        let dark = color(&xlib, display, colormap, black, 28, 28, 30);
        let power_color = color(&xlib, display, colormap, black, 90, 90, 94);
        let wm_delete = atom(&xlib, display, c"WM_DELETE_WINDOW");
        let xembed = atom(&xlib, display, c"_XEMBED");
        let xembed_info = atom(&xlib, display, c"_XEMBED_INFO");
        let utf8 = atom(&xlib, display, c"UTF8_STRING");
        // SAFETY: each window is created on the open display under a parent that outlives it:
        // the root outlives `window`, which outlives `parent` and `footer`, which outlives `power`.
        let (window, parent, footer, power) = unsafe {
            let root = (xlib.XDefaultRootWindow)(display);
            let window = (xlib.XCreateSimpleWindow)(
                display,
                root,
                0,
                0,
                480,
                320 + FOOTER as u32,
                0,
                dark,
                dark,
            );
            let parent = (xlib.XCreateSimpleWindow)(display, window, 0, 0, 480, 320, 0, dark, dark);
            let footer = (xlib.XCreateSimpleWindow)(
                display,
                window,
                0,
                320,
                480,
                FOOTER as u32,
                0,
                dark,
                dark,
            );
            let power = (xlib.XCreateSimpleWindow)(
                display,
                footer,
                POWER_X,
                POWER_Y,
                POWER_W,
                POWER_H,
                0,
                power_color,
                power_color,
            );
            (window, parent, footer, power)
        };
        // SAFETY: the windows above are alive and belong to this display; the atom arrays are
        // stack values borrowed only for the duration of each call.
        unsafe {
            (xlib.XSelectInput)(
                display,
                window,
                xlib::StructureNotifyMask | xlib::SubstructureNotifyMask,
            );
            (xlib.XSelectInput)(
                display,
                parent,
                xlib::StructureNotifyMask | xlib::SubstructureNotifyMask,
            );
            (xlib.XSelectInput)(display, footer, xlib::ButtonPressMask);
            (xlib.XSelectInput)(display, power, xlib::ButtonPressMask);
            let mut protocols = wm_delete;
            (xlib.XSetWMProtocols)(display, window, &mut protocols, 1);
            (xlib.XStoreName)(display, window, name.as_ptr());
            (xlib.XChangeProperty)(
                display,
                window,
                utf8,
                utf8,
                8,
                xlib::PropModeReplace,
                name.as_bytes().as_ptr().cast::<c_uchar>(),
                name.as_bytes().len() as c_int,
            );
            let info: [c_long; 2] = [0, 1];
            (xlib.XChangeProperty)(
                display,
                parent,
                xembed_info,
                xembed_info,
                32,
                xlib::PropModeReplace,
                info.as_ptr().cast::<c_uchar>(),
                info.len() as c_int,
            );
            (xlib.XFlush)(display);
        }
        Some(Window {
            xlib,
            display,
            window,
            parent,
            footer,
            power,
            wm_delete,
            xembed,
            xembed_info,
            utf8,
            state: EventState::default(),
            size: Cell::new((480, 320)),
            embedded: Cell::new(None),
        })
    }

    /// The typed X11 handle a CLAP or VST3 editor attaches to.
    pub(crate) fn editor_parent(&self) -> EditorParent {
        EditorParent::X11 {
            display: self.display.cast(),
            window: self.parent,
        }
    }

    pub(crate) fn set_title(&self, title: &str) {
        let Ok(name) = CString::new(title) else {
            return;
        };
        // SAFETY: the display and top-level window are alive; `name` is a valid C string.
        unsafe {
            (self.xlib.XStoreName)(self.display, self.window, name.as_ptr());
            (self.xlib.XChangeProperty)(
                self.display,
                self.window,
                self.utf8,
                self.utf8,
                8,
                xlib::PropModeReplace,
                name.as_bytes().as_ptr().cast::<c_uchar>(),
                name.as_bytes().len() as c_int,
            );
            (self.xlib.XFlush)(self.display);
        }
    }

    /// Sizes the window around a plug-in view of `width` by `height` pixels.
    pub(crate) fn set_plugin_size(&self, width: u32, height: u32) {
        let width = width.clamp(120, 8192);
        let height = height.clamp(40, 8192);
        self.size.set((width, height));
        // SAFETY: all four windows are alive and belong to this display; the sizes are clamped
        // to values that fit the X11 geometry arguments.
        unsafe {
            (self.xlib.XMoveResizeWindow)(
                self.display,
                self.window,
                0,
                0,
                width,
                height + FOOTER as u32,
            );
            (self.xlib.XMoveResizeWindow)(self.display, self.parent, 0, 0, width, height);
            (self.xlib.XMoveResizeWindow)(
                self.display,
                self.footer,
                0,
                height as c_int,
                width,
                FOOTER as u32,
            );
            (self.xlib.XFlush)(self.display);
        }
    }

    /// Resizes the host chrome when the user resizes the top-level window.
    fn layout_children(&self, width: u32, height: u32) {
        // SAFETY: both windows are alive and belong to this display.
        unsafe {
            (self.xlib.XMoveResizeWindow)(self.display, self.parent, 0, 0, width, height);
            (self.xlib.XMoveResizeWindow)(
                self.display,
                self.footer,
                0,
                height as c_int,
                width,
                FOOTER as u32,
            );
        }
    }

    pub(crate) fn show(&self) {
        // SAFETY: the four windows are alive and mapped under a live display.
        unsafe {
            (self.xlib.XMapWindow)(self.display, self.window);
            (self.xlib.XMapWindow)(self.display, self.parent);
            (self.xlib.XMapWindow)(self.display, self.footer);
            (self.xlib.XMapWindow)(self.display, self.power);
            (self.xlib.XFlush)(self.display);
        }
        self.state.closed.store(false, Ordering::Relaxed);
    }

    pub(crate) fn hide(&self) {
        // SAFETY: the top-level window is alive and belongs to this display.
        unsafe {
            (self.xlib.XUnmapWindow)(self.display, self.window);
            (self.xlib.XFlush)(self.display);
        }
    }

    /// Drains the events already waiting on the connection. Never blocks for a new event.
    pub(crate) fn pump(&self) {
        // SAFETY: `XPending` only reads the open connection; a positive count means the next
        // `XNextEvent` will return an initialized event without blocking.
        while unsafe { (self.xlib.XPending)(self.display) } > 0 {
            let mut event = std::mem::MaybeUninit::<xlib::XEvent>::uninit();
            // SAFETY: the event count was positive, so `XNextEvent` initializes `event` and does
            // not block; it is read only after that call.
            unsafe {
                (self.xlib.XNextEvent)(self.display, event.as_mut_ptr());
            }
            // SAFETY: the call above initialized the event; Xlib owns no memory inside it.
            let event = unsafe { event.assume_init() };
            self.handle(event);
        }
    }

    fn handle(&self, event: xlib::XEvent) {
        let any: &xlib::XAnyEvent = event.as_ref();
        match any.type_ {
            xlib::ClientMessage => {
                let message: &xlib::XClientMessageEvent = event.as_ref();
                if message.window == self.window
                    && message.message_type == self.wm_delete
                    && message.data.get_long(0) == self.wm_delete as c_long
                {
                    self.state.note_close();
                }
            }
            xlib::ButtonPress => {
                let press: &xlib::XButtonEvent = event.as_ref();
                if press.window == self.power {
                    self.state.note_power_click();
                }
            }
            xlib::ConfigureNotify => {
                let configure: &xlib::XConfigureEvent = event.as_ref();
                if configure.window == self.window {
                    let width = configure.width.max(0) as u32;
                    let height = (configure.height - FOOTER).max(0) as u32;
                    self.size.set((width, height));
                    self.layout_children(width, height);
                }
            }
            xlib::CreateNotify => {
                let create: &xlib::XCreateWindowEvent = event.as_ref();
                if create.parent == self.parent {
                    self.embed(create.window);
                }
            }
            xlib::PropertyNotify => {
                let property: &xlib::XPropertyEvent = event.as_ref();
                if property.atom == self.xembed_info && self.embedded.get() == Some(property.window)
                {
                    self.sync_embedded_map(property.window);
                }
            }
            _ => {}
        }
    }

    /// Starts XEmbed for a view the plug-in created inside the content window.
    fn embed(&self, child: xlib::Window) {
        // SAFETY: `child` was just created by the plug-in under our content window and is alive
        // for this call; `XSelectInput` only installs event masks on it.
        unsafe {
            (self.xlib.XSelectInput)(
                self.display,
                child,
                xlib::PropertyChangeMask | xlib::StructureNotifyMask,
            );
        }
        self.embedded.set(Some(child));
        self.send_xembed(child, 0, 0, self.parent as c_long, 0);
        self.sync_embedded_map(child);
        // SAFETY: `child` is alive and is the plug-in's embedded view; focus reverts to its
        // parent if it disappears.
        unsafe {
            (self.xlib.XSetInputFocus)(
                self.display,
                child,
                xlib::RevertToParent,
                xlib::CurrentTime,
            );
        }
    }

    /// Sends one XEmbed client message, as the freedesktop XEmbed protocol specifies.
    fn send_xembed(
        &self,
        child: xlib::Window,
        opcode: c_long,
        detail: c_long,
        data1: c_long,
        data2: c_long,
    ) {
        let data = [xlib::CurrentTime as c_long, opcode, detail, data1, data2];
        let message = xlib::XClientMessageEvent {
            type_: xlib::ClientMessage,
            serial: 0,
            send_event: 0,
            display: self.display,
            window: child,
            message_type: self.xembed,
            format: 32,
            data: xlib::ClientMessageData::from(data),
        };
        let mut event = xlib::XEvent::from(message);
        // SAFETY: `child` is alive, `event` is a fully initialized ClientMessage, and XEmbed
        // specifies no event mask and no propagation for these messages.
        unsafe {
            (self.xlib.XSendEvent)(self.display, child, 0, xlib::NoEventMask, &mut event);
            (self.xlib.XFlush)(self.display);
        }
    }

    /// Maps or unmaps the embedded view to match its `_XEMBED_INFO` mapped flag.
    fn sync_embedded_map(&self, child: xlib::Window) {
        let mut actual_type = 0;
        let mut actual_format = 0;
        let mut items = 0;
        let mut remaining = 0;
        let mut data: *mut c_uchar = std::ptr::null_mut();
        // SAFETY: `display` and `child` are alive; the out parameters are valid for the call and
        // Xlib allocates `data`, which this function frees after reading it.
        let result = unsafe {
            (self.xlib.XGetWindowProperty)(
                self.display,
                child,
                self.xembed_info,
                0,
                2,
                0,
                self.xembed_info,
                &mut actual_type,
                &mut actual_format,
                &mut items,
                &mut remaining,
                &mut data,
            )
        };
        if result != 0 || data.is_null() {
            return;
        }
        // SAFETY: on success Xlib allocated `items` 32-bit values at `data`; the slice is read
        // before `XFree` releases that allocation.
        let mapped = unsafe {
            let values = std::slice::from_raw_parts(data.cast::<c_long>(), items as usize);
            actual_format == 32 && items >= 2 && values.get(1).is_some_and(|flags| *flags & 1 != 0)
        };
        // SAFETY: `data` was allocated by the `XGetWindowProperty` call above and is freed once.
        unsafe {
            (self.xlib.XFree)(data.cast::<c_void>());
        }
        // SAFETY: `child` is alive and belongs to this display; mapping or unmapping it is the
        // embedder's side of the XEmbed protocol.
        unsafe {
            if mapped {
                (self.xlib.XMapWindow)(self.display, child);
            } else {
                (self.xlib.XUnmapWindow)(self.display, child);
            }
        }
    }

    /// Whether the user closed the window since the last call.
    pub(crate) fn take_closed(&self) -> bool {
        self.state.take_closed()
    }

    /// Whether the power button was clicked since the last call.
    pub(crate) fn take_power_click(&self) -> bool {
        self.state.take_power_click()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: this connection and every window it made are owned by `Window`; closing it
        // releases them all, exactly once, when the helper is dropped on the main thread.
        unsafe {
            (self.xlib.XCloseDisplay)(self.display);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{available_from_env, EventState};
    use std::ffi::OsStr;

    #[test]
    fn no_display_means_no_editor() {
        assert!(!available_from_env(None));
        assert!(!available_from_env(Some(OsStr::new(""))));
    }

    #[test]
    fn a_display_name_allows_an_editor() {
        assert!(available_from_env(Some(OsStr::new(":0"))));
    }

    #[test]
    fn close_and_power_clicks_are_latched() {
        let state = EventState::default();
        state.note_close();
        state.note_power_click();
        assert!(state.take_closed());
        assert!(state.take_power_click());
        assert!(!state.take_power_click());
    }
}
