//! The plug-in's window on macOS, in the plug-in's own process: an AppKit window laid out as
//! Logic's plug-in window is. A dark header with the round power button (Bypass) on the left,
//! the plug-in's own view below it, and the plug-in's name along the bottom.
//!
//! AppKit is reached through the Objective-C runtime's C functions (`objc_msgSend`), written
//! out here, so nothing is added to the build. Everything in this file runs on the process's
//! main thread.
//!
//! Not done: Logic's preset menu, Compare, Copy and Paste, Undo and Redo, and the View and
//! Side Chain menus of the header.

use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) type Id = *mut c_void;
type Sel = *const c_void;

const HEADER: f64 = 44.0;
const FOOTER: f64 = 26.0;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Size {
    pub w: f64,
    pub h: f64,
}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn class_addMethod(class: Id, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
}

#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopTimerCreate(
        allocator: *const c_void,
        fire: f64,
        interval: f64,
        flags: u64,
        order: isize,
        callout: unsafe extern "C" fn(*mut c_void, *mut c_void),
        context: *const TimerContext,
    ) -> *mut c_void;
    fn CFRunLoopAddTimer(run_loop: *mut c_void, timer: *mut c_void, mode: *const c_void);
}

#[repr(C)]
struct TimerContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    describe: *const c_void,
}

/// Sends an Objective-C message: `msg!(ReturnType, object, "selector:", ArgType: value, …)`.
///
/// Every use is `unsafe`: the selector, the argument types and the return type must be the
/// method's own.
macro_rules! msg {
    ($ret:ty, $obj:expr, $sel:expr $(, $t:ty : $a:expr)* $(,)?) => {{
        let send: unsafe extern "C" fn($crate::mac::Id, *const std::ffi::c_void $(, $t)*) -> $ret =
            std::mem::transmute($crate::mac::send_fn());
        send($obj, $crate::mac::sel($sel) $(, $a)*)
    }};
}
pub(crate) use msg;

pub(crate) fn send_fn() -> *const c_void {
    objc_msgSend as *const c_void
}

pub(crate) fn sel(name: &str) -> Sel {
    let name = CString::new(name).unwrap_or_default();
    // SAFETY: `name` is NUL-terminated; the runtime copies it.
    unsafe { sel_registerName(name.as_ptr()) }
}

pub(crate) fn class(name: &str) -> Id {
    let name = CString::new(name).unwrap_or_default();
    // SAFETY: `name` is NUL-terminated; the result is null for a class that is not loaded.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// An autoreleased `NSString` of `text`.
pub(crate) fn ns_string(text: &str) -> Id {
    let text = CString::new(text.replace('\0', " ")).unwrap_or_default();
    // SAFETY: `stringWithUTF8String:` takes a NUL-terminated UTF-8 string and copies it.
    unsafe { msg!(Id, class("NSString"), "stringWithUTF8String:", *const c_char: text.as_ptr()) }
}

/// The frame of `view` (an `NSView`).
///
/// # Safety
/// `view` is a live `NSView`.
pub(crate) unsafe fn frame(view: Id) -> Rect {
    #[cfg(target_arch = "x86_64")]
    {
        // A structure this large comes back through a pointer on x86-64.
        let mut out = Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        let send: unsafe extern "C" fn(*mut Rect, Id, Sel) =
            std::mem::transmute(objc_msgSend_stret as *const c_void);
        send(&mut out, view, sel("frame"));
        out
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // SAFETY: the caller's contract; `frame` returns an `NSRect`.
        unsafe { msg!(Rect, view, "frame") }
    }
}

static CLOSED: AtomicBool = AtomicBool::new(false);
static POWER: AtomicBool = AtomicBool::new(false);

/// The window's close button: the window is hidden, not destroyed, so the plug-in keeps its
/// view and opening again is instant.
unsafe extern "C" fn should_close(_this: Id, _cmd: Sel, window: Id) -> bool {
    // SAFETY: AppKit passes the window being closed.
    unsafe { msg!((), window, "orderOut:", Id: std::ptr::null_mut()) };
    CLOSED.store(true, Ordering::Relaxed);
    false
}

unsafe extern "C" fn power_clicked(_this: Id, _cmd: Sel, _sender: Id) {
    POWER.store(true, Ordering::Relaxed);
}

/// The object the window and the power button report to (one class for the process).
fn delegate() -> Id {
    static MADE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MADE.get_or_init(|| {
        // SAFETY: a new subclass of NSObject with two methods whose type strings match the
        // functions above ("c@:@": returns BOOL, takes self, _cmd and an object; "v@:@":
        // the same returning nothing).
        unsafe {
            let made = objc_allocateClassPair(class("NSObject"), c"PWPluginWindow".as_ptr(), 0);
            if !made.is_null() {
                class_addMethod(
                    made,
                    sel("windowShouldClose:"),
                    should_close as *const c_void,
                    c"c@:@".as_ptr(),
                );
                class_addMethod(
                    made,
                    sel("powerClicked:"),
                    power_clicked as *const c_void,
                    c"v@:@".as_ptr(),
                );
                objc_registerClassPair(made);
            }
            msg!(Id, class("PWPluginWindow"), "new") as usize
        }
    }) as Id
}

fn color(red: f64, green: f64, blue: f64) -> Id {
    // SAFETY: `colorWithSRGBRed:green:blue:alpha:` takes four CGFloats.
    unsafe {
        msg!(Id, class("NSColor"), "colorWithSRGBRed:green:blue:alpha:",
            f64: red, f64: green, f64: blue, f64: 1.0)
    }
}

/// A plug-in's window.
pub(crate) struct Window {
    window: Id,
    /// The view the plug-in's own view goes in.
    parent: Id,
    header: Id,
    footer: Id,
    power: Id,
}

impl Window {
    /// A hidden window for the plug-in called `name` (written along the bottom, as Logic does).
    pub(crate) fn new(name: &str) -> Window {
        let content = Rect {
            x: 0.0,
            y: 0.0,
            w: 480.0,
            h: 320.0 + HEADER + FOOTER,
        };
        let zero = Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        // SAFETY: AppKit calls on the main thread with each method's own argument types. The
        // window and its views are kept for the life of the process.
        unsafe {
            // Titled, closable, miniaturizable.
            let window = msg!(Id, msg!(Id, class("NSWindow"), "alloc"),
                "initWithContentRect:styleMask:backing:defer:",
                Rect: content, usize: 1 | 2 | 4, usize: 2, bool: false);
            msg!((), window, "setReleasedWhenClosed:", bool: false);
            msg!((), window, "setDelegate:", Id: delegate());
            let dark = msg!(Id, class("NSAppearance"), "appearanceNamed:",
                Id: ns_string("NSAppearanceNameDarkAqua"));
            msg!((), window, "setAppearance:", Id: dark);
            msg!((), window, "setBackgroundColor:", Id: color(0.11, 0.11, 0.12));
            // Above Pro Wave's own window, which belongs to another process.
            msg!((), window, "setLevel:", isize: 3);
            let root = msg!(Id, window, "contentView");

            let view = |frame: Rect| {
                let view = msg!(Id, msg!(Id, class("NSView"), "alloc"), "initWithFrame:",
                    Rect: frame);
                msg!((), root, "addSubview:", Id: view);
                view
            };
            let header = view(zero);
            let parent = view(zero);

            let image = msg!(Id, class("NSImage"),
                "imageWithSystemSymbolName:accessibilityDescription:",
                Id: ns_string("power"), Id: std::ptr::null_mut());
            let power = msg!(Id, class("NSButton"), "buttonWithImage:target:action:",
                Id: image, Id: delegate(), *const c_void: sel("powerClicked:"));
            // A round, filled button, as Logic's power button is: no bezel, a layer for the
            // disc (coloured by `set_power`) and the symbol in white.
            msg!((), power, "setBordered:", bool: false);
            msg!((), power, "setWantsLayer:", bool: true);
            let layer = msg!(Id, power, "layer");
            msg!((), layer, "setCornerRadius:", f64: 16.0);
            msg!((), power, "setContentTintColor:", Id: color(1.0, 1.0, 1.0));
            msg!((), power, "setFrame:", Rect: Rect { x: 12.0, y: 6.0, w: 32.0, h: 32.0 });
            msg!((), header, "addSubview:", Id: power);

            let footer = msg!(Id, class("NSTextField"), "labelWithString:", Id: ns_string(name));
            msg!((), footer, "setAlignment:", usize: 1);
            msg!((), footer, "setTextColor:", Id: color(0.86, 0.86, 0.88));
            let font = msg!(Id, class("NSFont"), "systemFontOfSize:weight:", f64: 14.0, f64: 0.3);
            msg!((), footer, "setFont:", Id: font);
            msg!((), root, "addSubview:", Id: footer);

            let made = Window {
                window,
                parent,
                header,
                footer,
                power,
            };
            made.set_plugin_size(480, 320);
            msg!((), window, "center");
            made
        }
    }

    /// The `NSView` the plug-in puts its own view in.
    pub(crate) fn parent(&self) -> *mut c_void {
        self.parent
    }

    /// Sizes the window around a plug-in view of `width` by `height` points.
    pub(crate) fn set_plugin_size(&self, width: u32, height: u32) {
        let (w, h) = (
            f64::from(width.clamp(120, 8192)),
            f64::from(height.clamp(40, 8192)),
        );
        // SAFETY: AppKit calls on the main thread with each method's own argument types.
        // AppKit's origin is the bottom left: the footer, the plug-in, then the header.
        unsafe {
            msg!((), self.window, "setContentSize:", Size: Size { w, h: h + HEADER + FOOTER });
            msg!((), self.footer, "setFrame:",
                Rect: Rect { x: 0.0, y: 4.0, w, h: FOOTER - 8.0 });
            msg!((), self.parent, "setFrame:", Rect: Rect { x: 0.0, y: FOOTER, w, h });
            msg!((), self.header, "setFrame:",
                Rect: Rect { x: 0.0, y: FOOTER + h, w, h: HEADER });
        }
    }

    pub(crate) fn set_title(&self, title: &str) {
        // SAFETY: `setTitle:` takes an NSString.
        unsafe { msg!((), self.window, "setTitle:", Id: ns_string(title)) };
    }

    /// Lights the power button (the plug-in is on) or greys it (bypassed).
    pub(crate) fn set_power(&self, on: bool) {
        let tint = if on {
            color(0.16, 0.47, 0.95)
        } else {
            color(0.35, 0.35, 0.37)
        };
        // SAFETY: the button is layer-backed (see `new`); a layer's background is a CGColor,
        // which `NSColor` gives.
        unsafe {
            let layer = msg!(Id, self.power, "layer");
            let fill = msg!(Id, tint, "CGColor");
            msg!((), layer, "setBackgroundColor:", Id: fill);
        }
    }

    pub(crate) fn show(&self) {
        // SAFETY: AppKit calls on the main thread with each method's own argument types.
        unsafe {
            let app = msg!(Id, class("NSApplication"), "sharedApplication");
            msg!((), app, "activateIgnoringOtherApps:", bool: true);
            msg!((), self.window, "makeKeyAndOrderFront:", Id: std::ptr::null_mut());
        }
        CLOSED.store(false, Ordering::Relaxed);
    }

    pub(crate) fn hide(&self) {
        // SAFETY: `orderOut:` takes a sender, which may be nil.
        unsafe { msg!((), self.window, "orderOut:", Id: std::ptr::null_mut()) };
    }

    /// Whether the user closed the window since the last call.
    pub(crate) fn take_closed(&self) -> bool {
        CLOSED.swap(false, Ordering::Relaxed)
    }

    /// Whether the power button was clicked since the last call.
    pub(crate) fn take_power_click(&self) -> bool {
        POWER.swap(false, Ordering::Relaxed)
    }
}

unsafe extern "C" fn fire(_timer: *mut c_void, info: *mut c_void) {
    // SAFETY: `info` is the boxed closure `run` leaked; the timer fires on the main thread
    // only, one call at a time.
    let tick = unsafe { &mut *info.cast::<Box<dyn FnMut()>>() };
    tick();
}

/// Runs the process as an AppKit application without a Dock icon, calling `tick` about 120
/// times a second on the main thread. Never returns.
pub(crate) fn run(tick: Box<dyn FnMut()>) -> ! {
    let info = Box::into_raw(Box::new(tick));
    let context = TimerContext {
        version: 0,
        info: info.cast(),
        retain: std::ptr::null(),
        release: std::ptr::null(),
        describe: std::ptr::null(),
    };
    // SAFETY: this is the main thread; the timer's closure lives for the rest of the process,
    // and Core Foundation copies `context`. Activation policy 1 is "accessory": windows, no
    // Dock icon.
    unsafe {
        let app = msg!(Id, class("NSApplication"), "sharedApplication");
        msg!(bool, app, "setActivationPolicy:", isize: 1);
        let timer = CFRunLoopTimerCreate(
            std::ptr::null(),
            CFAbsoluteTimeGetCurrent(),
            1.0 / 120.0,
            0,
            0,
            fire,
            &context,
        );
        CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopCommonModes);
        msg!((), app, "run");
    }
    std::process::exit(0)
}
