//! What only macOS needs.

use objc2::sel;
use objc2_app_kit::{NSApplication, NSView};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Quit, from the app menu or Cmd+Q, asks the window to close, as its own close button does, so
/// unsaved edits get their question and a merge tool's answer is the one it gives on closing.
/// winit's menu ends the process outright, with exit code 0, which git would take as a merge done.
///
/// The menu item is pointed at the window's delegate, which turns `windowShouldClose:` into a
/// close request and keeps the window open. Nothing changes if any part of that isn't there.
pub fn quit_through_close(window: &impl HasWindowHandle) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: an AppKit handle's view is an NSView, alive as long as the window it belongs to.
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(delegate) = view.window().and_then(|w| w.delegate()) else { return };
    let Some(menu) = NSApplication::sharedApplication(mtm).mainMenu() else { return };
    for top in menu.itemArray() {
        let Some(sub) = top.submenu() else { continue };
        for item in sub.itemArray() {
            if item.action() == Some(sel!(terminate:)) {
                // SAFETY: the delegate answers `windowShouldClose:` and lives as long as the
                // window, which is as long as the process: the item holds it weakly.
                unsafe {
                    item.setTarget(Some(delegate.as_ref()));
                    item.setAction(Some(sel!(windowShouldClose:)));
                }
            }
        }
    }
}
