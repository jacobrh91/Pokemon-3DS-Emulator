//! macOS implementation: messages the `NSWindow` behind the minifb window
//! through the Objective-C runtime.
//!
//! All the `unsafe` is in the "runtime" section below; the rest is safe code
//! that names each AppKit selector with its argument and result types.

use super::Window;
use std::ffi::{CStr, c_char, c_void};

/// An Objective-C object pointer (`id`).
type Id = *mut c_void;
/// A selector (`SEL`).
type Sel = *mut c_void;

/// `NSSize`: two `CGFloat`s, passed in floating-point registers.
#[repr(C)]
#[derive(Clone, Copy)]
struct NsSize {
    width: f64,
    height: f64,
}

/// `NSPoint`: two `CGFloat`s, returned in floating-point registers.
#[repr(C)]
#[derive(Clone, Copy)]
struct NsPoint {
    x: f64,
    y: f64,
}

/// `NSRect`: origin then size, four `CGFloat`s.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NsRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// `NSWindowCollectionBehaviorFullScreenPrimary`.
const FULL_SCREEN_PRIMARY: usize = 1 << 7;
/// `NSWindowStyleMaskFullScreen`.
const STYLE_MASK_FULL_SCREEN: usize = 1 << 14;

// ---------------------------------------------------------------------------
// The runtime: the only `unsafe` in this file.
// ---------------------------------------------------------------------------

#[link(name = "objc")]
unsafe extern "C" {
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_getClass(name: *const c_char) -> Id;
    fn objc_msgSend();
    #[cfg(not(target_arch = "aarch64"))]
    fn objc_msgSend_stret();
}

/// The selector named `name`.
fn selector(name: &CStr) -> Sel {
    // Safety: `name` is a valid NUL-terminated string.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// The class named `name`, or null.
fn class(name: &CStr) -> Id {
    // Safety: `name` is a valid NUL-terminated string.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// The argument lists a message can carry, as tuples. `objc_msgSend` is a
/// trampoline that must be called through a pointer of the callee's real
/// signature; each implementation builds that type from the tuple's.
trait Arguments {
    fn send<R>(self, object: Id, selector: Sel) -> R;
}

macro_rules! arguments {
    ($($name:ident: $ty:ident),*) => {
        impl<$($ty),*> Arguments for ($($ty,)*) {
            fn send<R>(self, object: Id, selector: Sel) -> R {
                let ($($name,)*) = self;
                // Safety: the signature is the one the caller of `message`
                // states with its argument and result types, which the
                // typed functions below keep in step with AppKit's.
                unsafe {
                    let call: unsafe extern "C" fn(Id, Sel, $($ty),*) -> R =
                        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                    call(object, selector, $($name),*)
                }
            }
        }
    };
}

arguments!();
arguments!(a: A);
arguments!(a: A, b: B);

/// Sends the message `name` with `arguments` to `object`, expecting an `R`.
///
/// Safe because it is module-private: every call pairs a selector with its
/// documented AppKit signature and uses a live object on the main thread.
fn message<R, A: Arguments>(object: Id, name: &CStr, arguments: A) -> R {
    debug_assert!(!object.is_null());
    arguments.send(object, selector(name))
}

/// Sends a message that returns an `NSRect`. A 32-byte struct comes back in
/// registers (d0-d3) on arm64 but in memory, through `objc_msgSend_stret`, on
/// x86_64.
#[cfg(target_arch = "aarch64")]
fn message_rect(object: Id, name: &CStr) -> NsRect {
    message(object, name, ())
}

#[cfg(not(target_arch = "aarch64"))]
fn message_rect(object: Id, name: &CStr) -> NsRect {
    debug_assert!(!object.is_null());
    let mut result = NsRect::default();
    // Safety: as `message`, for a `() -> NSRect` message.
    unsafe {
        let call: unsafe extern "C" fn(*mut NsRect, Id, Sel) =
            std::mem::transmute(objc_msgSend_stret as unsafe extern "C" fn());
        call(&mut result, object, selector(name));
    }
    result
}

// ---------------------------------------------------------------------------
// Window features, in safe code.
// ---------------------------------------------------------------------------

/// The `NSWindow` behind a minifb window (valid for the window's life), if
/// there is one.
fn ns_window(window: &Window) -> Option<Id> {
    let object = window.get_window_handle();
    (!object.is_null()).then_some(object)
}

/// The window's content area (below the title bar) from `contentLayoutRect`.
pub fn content_size(window: &Window) -> Option<(f32, f32)> {
    let rect = message_rect(ns_window(window)?, c"contentLayoutRect");
    (rect.width > 0.0 && rect.height > 0.0).then_some((rect.width as f32, rect.height as f32))
}

/// Whether the left mouse button is down, from `+[NSEvent pressedMouseButtons]`
/// (an `NSUInteger` bit mask; bit 0 is the left button).
pub fn left_button_down() -> bool {
    let class = class(c"NSEvent");
    if class.is_null() {
        return false;
    }
    let buttons: usize = message(class, c"pressedMouseButtons", ());
    buttons & 1 != 0
}

pub fn lock_aspect_ratio(
    window: &Window,
    width: usize,
    height: usize,
    min_width: usize,
    min_height: usize,
) {
    let Some(object) = ns_window(window) else {
        return;
    };
    let size = |width: usize, height: usize| NsSize {
        width: width as f64,
        height: height as f64,
    };
    message::<(), _>(object, c"setContentAspectRatio:", (size(width, height),));
    message::<(), _>(
        object,
        c"setContentMinSize:",
        (size(min_width, min_height),),
    );
}

pub fn enable_fullscreen(window: &Window) {
    if let Some(object) = ns_window(window) {
        message::<(), _>(object, c"setCollectionBehavior:", (FULL_SCREEN_PRIMARY,));
    }
}

pub fn toggle_fullscreen(window: &Window) {
    if let Some(object) = ns_window(window) {
        // The argument is the sender.
        message::<(), _>(
            object,
            c"toggleFullScreen:",
            (std::ptr::null_mut::<c_void>(),),
        );
    }
}

pub fn is_fullscreen(window: &Window) -> bool {
    ns_window(window).is_some_and(|object| {
        let mask: usize = message(object, c"styleMask", ());
        mask & STYLE_MASK_FULL_SCREEN != 0
    })
}

/// The view minifb draws the picture into: the last subview of the window's
/// content view (an `MTKView`), falling back to the content view itself. The
/// content view reaches under the title bar, so it is not the visible
/// picture area.
fn picture_view(window: Id) -> Option<Id> {
    let object = |target: Id, name: &CStr| -> Option<Id> {
        let reply: Id = message(target, name, ());
        (!reply.is_null()).then_some(reply)
    };
    let content_view = object(window, c"contentView")?;
    let last = object(content_view, c"subviews").and_then(|views| object(views, c"lastObject"));
    Some(last.unwrap_or(content_view))
}

/// The pointer inside the picture view, in points from its top-left, and the
/// view's size.
pub fn pointer_in_picture_view(window: &Window) -> Option<((f32, f32), (f32, f32))> {
    let object = ns_window(window)?;
    let view = picture_view(object)?;
    let in_window: NsPoint = message(object, c"mouseLocationOutsideOfEventStream", ());
    // A nil view means window coordinates.
    let in_view: NsPoint = message(
        view,
        c"convertPoint:fromView:",
        (in_window, std::ptr::null_mut::<c_void>()),
    );
    let bounds = message_rect(view, c"bounds");
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }
    // AppKit views are bottom-left origin unless flipped; the picture view is
    // not flipped, so measure Y from the top.
    let flipped: bool = message(view, c"isFlipped", ());
    let y = if flipped {
        in_view.y
    } else {
        bounds.height - in_view.y
    };
    Some((
        (in_view.x as f32, y as f32),
        (bounds.width as f32, bounds.height as f32),
    ))
}
