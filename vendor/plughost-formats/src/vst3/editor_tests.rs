use super::*;

#[test]
fn any_rectangle_a_plugin_reports_has_dimensions() {
    let rect = |left, top, right, bottom| ViewRect {
        left,
        top,
        right,
        bottom,
    };
    assert_eq!(dimensions(&rect(10, 20, 410, 320)), (400, 300));
    assert_eq!(
        dimensions(&rect(i32::MIN, i32::MIN, i32::MAX, i32::MAX)),
        (u32::MAX, u32::MAX)
    );
    // Inverted or overflowing rectangles are empty, not a panic.
    assert_eq!(dimensions(&rect(i32::MAX, 0, i32::MIN, 0)), (0, 0));
}

use std::cell::RefCell;
use std::rc::Rc;

use vst3::Steinberg::{FIDString, TBool, char16, int16};

fn rect((width, height): (u32, u32)) -> ViewRect {
    ViewRect {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    }
}

/// A plugin's view that records the sizes it is told and, told one, may ask for another.
struct TestView {
    size: Cell<(u32, u32)>,
    told: RefCell<Vec<(u32, u32)>>,
    /// Asked for from the next `onSize`.
    ask: Cell<Option<(u32, u32)>>,
    frame: Cell<*mut IPlugFrame>,
    this: Cell<*mut IPlugView>,
}

impl Class for TestView {
    type Interfaces = (IPlugView,);
}

impl IPlugViewTrait for TestView {
    unsafe fn isPlatformTypeSupported(&self, _type: FIDString) -> tresult {
        kResultOk
    }
    unsafe fn attached(&self, _parent: *mut c_void, _type: FIDString) -> tresult {
        kResultOk
    }
    unsafe fn removed(&self) -> tresult {
        kResultOk
    }
    unsafe fn onWheel(&self, _distance: f32) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyDown(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }
    unsafe fn onKeyUp(&self, _key: char16, _key_code: int16, _modifiers: int16) -> tresult {
        kResultFalse
    }
    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        unsafe { *size = rect(self.size.get()) };
        kResultOk
    }
    unsafe fn onSize(&self, new_size: *mut ViewRect) -> tresult {
        let size = dimensions(unsafe { &*new_size });
        self.size.set(size);
        self.told.borrow_mut().push(size);
        if let Some(ask) = self.ask.take() {
            let frame = unsafe { ComRef::<IPlugFrame>::from_raw(self.frame.get()) }.unwrap();
            unsafe { frame.resizeView(self.this.get(), &mut rect(ask)) };
        }
        kResultOk
    }
    unsafe fn onFocus(&self, _state: TBool) -> tresult {
        kResultOk
    }
    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        self.frame.set(frame);
        kResultOk
    }
    unsafe fn canResize(&self) -> tresult {
        kResultOk
    }
    unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
        kResultOk
    }
}

/// An editor whose view is 200 by 100, in a window that records the sizes it is given and
/// passes each to `window`.
struct Fixture {
    editor: Editor,
    view: ComWrapper<TestView>,
    window: Rc<RefCell<Vec<(u32, u32)>>>,
}

impl Fixture {
    fn new(window: impl Fn(u32, u32) + 'static) -> Fixture {
        let view = ComWrapper::new(TestView {
            size: Cell::new((200, 100)),
            told: RefCell::new(Vec::new()),
            ask: Cell::new(None),
            frame: Cell::new(std::ptr::null_mut()),
            this: Cell::new(std::ptr::null_mut()),
        });
        let sizes = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&sizes);
        let frame = ComWrapper::new(PlugFrame {
            resize: Box::new(move |width, height| {
                recorded.borrow_mut().push((width, height));
                window(width, height);
                true
            }),
            resizing: Cell::new(false),
        });
        let pointer = view.as_com_ref::<IPlugView>().unwrap().as_ptr();
        view.this.set(pointer);
        unsafe { view.setFrame(frame.as_com_ref::<IPlugFrame>().unwrap().as_ptr()) };
        Fixture {
            editor: Editor {
                view: view.to_com_ptr::<IPlugView>().unwrap(),
                _frame: frame,
            },
            view,
            window: sizes,
        }
    }

    fn ask(&self, size: (u32, u32)) -> tresult {
        let frame = unsafe { ComRef::<IPlugFrame>::from_raw(self.view.frame.get()) }.unwrap();
        unsafe { frame.resizeView(self.view.this.get(), &mut rect(size)) }
    }
}

#[test]
fn a_view_asking_for_the_size_it_has_is_not_resized() {
    let fixture = Fixture::new(|_, _| {});
    assert_eq!(fixture.ask((200, 100)), kResultOk);
    assert!(fixture.window.borrow().is_empty());
    assert!(fixture.view.told.borrow().is_empty());
}

#[test]
fn a_view_asking_again_while_its_window_resizes_is_refused() {
    let again = Rc::new(Cell::new(None));
    let answer = Rc::clone(&again);
    let view: Rc<Cell<Option<(*mut IPlugFrame, *mut IPlugView)>>> = Rc::new(Cell::new(None));
    let pointers = Rc::clone(&view);
    // A view that follows its parent's frame asks for a size while the window resizes.
    let fixture = Fixture::new(move |_, _| {
        if let Some((frame, view)) = pointers.take() {
            let frame = unsafe { ComRef::<IPlugFrame>::from_raw(frame) }.unwrap();
            answer.set(Some(unsafe {
                frame.resizeView(view, &mut rect((400, 200)))
            }));
        }
    });
    view.set(Some((fixture.view.frame.get(), fixture.view.this.get())));
    assert_eq!(fixture.ask((300, 150)), kResultOk);
    assert_eq!(again.get(), Some(kResultFalse));
    assert_eq!(*fixture.window.borrow(), [(300, 150)]);
    assert_eq!(*fixture.view.told.borrow(), [(300, 150)]);
}

#[test]
fn a_view_asking_without_a_size_is_refused() {
    let fixture = Fixture::new(|_, _| {});
    let frame = unsafe { ComRef::<IPlugFrame>::from_raw(fixture.view.frame.get()) }.unwrap();
    let result = unsafe { frame.resizeView(fixture.view.this.get(), std::ptr::null_mut()) };
    assert_eq!(result, kInvalidArgument);
    assert!(fixture.window.borrow().is_empty());
}

#[test]
fn a_dragged_size_the_view_has_is_not_offered_to_it() {
    let fixture = Fixture::new(|_, _| {});
    assert_eq!(fixture.editor.resize(200, 100), (200, 100));
    assert!(fixture.view.told.borrow().is_empty());
}

#[test]
fn a_view_that_asks_for_another_size_while_dragged_gets_it() {
    let fixture = Fixture::new(|_, _| {});
    fixture.view.ask.set(Some((320, 160)));
    // The window takes the size the view settled on, not the dragged one.
    assert_eq!(fixture.editor.resize(300, 150), (320, 160));
    assert_eq!(*fixture.view.told.borrow(), [(300, 150), (320, 160)]);
    assert_eq!(*fixture.window.borrow(), [(320, 160)]);
}
