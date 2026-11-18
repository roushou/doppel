//! Tags the window's drawing as sRGB. GPUI's Metal layer is made without a
//! colour space, so macOS shows its colours in the display's own (Display P3
//! on most Macs): the design's sRGB tokens come out more saturated than in
//! the web console, which colour-manages them. With the layer tagged,
//! both consoles show the same colours, and a guest's sRGB screen shows true.

#![expect(
    unsafe_code,
    reason = "the layer is reached through AppKit's and CoreGraphics' C and Objective-C interfaces"
)]

use std::ffi::c_void;

use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// CoreGraphics' colour space, only ever behind a pointer. Its encoding is
/// what Objective-C checks `setColorspace:`'s argument against.
#[repr(C)]
struct CGColorSpace {
    _private: [u8; 0],
}

// SAFETY: `CGColorSpaceRef` is a pointer to the opaque struct `CGColorSpace`.
unsafe impl RefEncode for CGColorSpace {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut CGColorSpace;
    fn CGColorSpaceRelease(space: *mut CGColorSpace);
}

/// Tags `window`'s layer as sRGB. Leaves it as it was where there's no
/// AppKit view or layer to tag: the colours are then only more saturated.
pub(crate) fn tag(window: &impl HasWindowHandle) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: `view` is the window's live NSView, as the handle promises for
    // as long as `window` is borrowed. `layer` returns its backing layer or
    // nil; `setColorspace:` is CAMetalLayer's, which GPUI's layer is, and
    // retains the colour space, so ours is released after.
    unsafe {
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        let responds: bool = msg_send![layer, respondsToSelector: objc2::sel!(setColorspace:)];
        if !responds {
            return;
        }
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        if space.is_null() {
            return;
        }
        let _: () = msg_send![layer, setColorspace: space];
        CGColorSpaceRelease(space);
    }
}
