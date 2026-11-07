//! A notification from the system when something finishes out of sight. Only ever about something
//! the user started, and only with the setting on. Nothing here watches a host.
//!
//! On Windows this is the notification area's balloon, which Windows shows as a notification. It
//! needs no app id and nothing from the installer, unlike the newer toast API. Elsewhere there's
//! nothing yet: the taskbar's flash still happens.

/// Shows one. A field on the app, so a test never puts anything on someone's desktop.
pub type Notifier = fn(&str, &str);

/// `(heading, body)`. Git's output stays out: a notification can show on a locked screen, and
/// output can hold addresses and names.
pub fn text(title: &str, place: &str, failed: bool) -> (String, String) {
    if failed { (format!("{title} failed"), format!("{place}. See gitem for what git said.")) } else { (format!("{title} finished"), place.to_string()) }
}

#[cfg(windows)]
mod win {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_INFO, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
    use windows_sys::Win32::UI::WindowsAndMessaging::LoadIconW;

    const ICON_ID: u32 = 1;
    /// Taking the icon away takes the notification with it, so it stays for about as long as one shows.
    const SHOWN_FOR: std::time::Duration = std::time::Duration::from_secs(12);

    /// Fills a fixed text field, cut to fit and always ended, without splitting a surrogate pair.
    fn fill(field: &mut [u16], text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut n = units.len().min(field.len() - 1);
        if n < units.len() && n > 0 && (0xD800..0xDC00).contains(&units[n - 1]) {
            n -= 1;
        }
        field[..n].copy_from_slice(&units[..n]);
        field[n] = 0;
    }

    pub(super) fn data(hwnd: isize, title: &str, body: &str, warn: bool) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd as HWND, uID: ICON_ID, uFlags: NIF_INFO | NIF_ICON | NIF_TIP, ..Default::default() };
        fill(&mut d.szInfoTitle, title);
        fill(&mut d.szInfo, body);
        fill(&mut d.szTip, "gitem");
        d.dwInfoFlags = if warn { NIIF_WARNING } else { NIIF_INFO };
        d
    }

    /// The window it's shown for, once one's been told about. None in tests and screenshots.
    static WINDOW: Mutex<Option<(isize, bool)>> = Mutex::new(None);
    static LATEST: AtomicU64 = AtomicU64::new(0);

    pub(super) fn set_window(hwnd: isize) {
        if let Ok(mut w) = WINDOW.lock() {
            *w = Some((hwnd, false));
        }
    }

    pub(super) fn show(title: &str, body: &str) {
        let Ok(mut window) = WINDOW.lock() else { return };
        let Some((hwnd, added)) = window.as_mut() else { return };
        let mut d = data(*hwnd, title, body, title.ends_with("failed"));
        // SAFETY: `d` is fully initialised and outlives the calls, and the icon comes from this
        // program's own resources, by the id the build script gives it. A null icon is allowed.
        unsafe {
            d.hIcon = LoadIconW(GetModuleHandleW(std::ptr::null()), ICON_ID as usize as *const u16);
            if Shell_NotifyIconW(if *added { NIM_MODIFY } else { NIM_ADD }, &d) != 0 {
                *added = true;
            }
        }
        let turn = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            std::thread::sleep(SHOWN_FOR);
            if LATEST.load(Ordering::SeqCst) == turn {
                clear();
            }
        });
    }

    pub(super) fn clear() {
        let Ok(mut window) = WINDOW.lock() else { return };
        let Some((hwnd, added)) = window.as_mut() else { return };
        if std::mem::take(added) {
            let d = NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: *hwnd as HWND, uID: ICON_ID, ..Default::default() };
            // SAFETY: as above.
            unsafe {
                Shell_NotifyIconW(NIM_DELETE, &d);
            }
        }
    }
}

/// The real one. On Windows it does nothing until `set_window` has been told about a window, which
/// only the real window does.
pub fn show(title: &str, body: &str) {
    #[cfg(windows)]
    win::show(title, body);
    #[cfg(not(windows))]
    let _ = (title, body);
}

/// Takes the icon away, as a window that's gone leaves it behind until the pointer passes over it.
pub fn clear() {
    #[cfg(windows)]
    win::clear();
}

#[cfg(windows)]
pub fn set_window(frame: &eframe::Frame) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(h) = frame.window_handle()
        && let RawWindowHandle::Win32(w) = h.as_raw()
    {
        win::set_window(w.hwnd.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_it_says_leaves_out_git_output() {
        assert_eq!(text("Fetch", "gitem", false), ("Fetch finished".to_string(), "gitem".to_string()));
        let (head, body) = text("Push", "gitem", true);
        assert_eq!(head, "Push failed");
        assert!(body.starts_with("gitem.") && !body.contains("https"), "{body}");
    }

    #[cfg(windows)]
    #[test]
    fn the_call_is_built_as_windows_wants_and_long_text_is_cut_and_ended() {
        use windows_sys::Win32::UI::Shell::{NIF_INFO, NIIF_INFO, NIIF_WARNING};
        let long = "x".repeat(500);
        let d = win::data(42, &long, &long, false);
        assert_eq!(d.cbSize as usize, size_of_val(&d));
        assert_eq!(d.hWnd as isize, 42);
        assert_ne!(d.uFlags & NIF_INFO, 0);
        let (title, body, flags) = (d.szInfoTitle, d.szInfo, d.dwInfoFlags);
        assert_eq!(flags, NIIF_INFO);
        assert_eq!(title[63], 0);
        assert_eq!(body[255], 0);
        assert_eq!(title[62], b'x' as u16);
        assert_eq!(win::data(42, "a", "b", true).dwInfoFlags, NIIF_WARNING);
        // A pair cut in half would show as a broken character.
        let d = win::data(42, &format!("{}\u{1F600}", "x".repeat(62)), "", false);
        let title = d.szInfoTitle;
        assert_eq!(title[61], b'x' as u16);
        assert_eq!(title[62], 0);
    }

    /// With no window it does nothing, so a test or a screenshot can't reach the desktop.
    #[test]
    fn with_no_window_told_about_nothing_is_shown() {
        show("Fetch finished", "gitem");
        clear();
    }
}
