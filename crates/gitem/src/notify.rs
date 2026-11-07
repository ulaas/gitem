//! A notification from the system when something finishes out of sight. Only ever about something
//! the user started, and only with the setting on. Nothing here watches a host.
//!
//! On Windows this is the notification area's balloon, which Windows shows as a notification. It
//! needs no app id and nothing from the installer, unlike the newer toast API. On Linux it's
//! `notify-send` and on macOS `osascript`, started directly with no shell between. Its text comes
//! from repository names and job titles, so it's cleaned, and escaped for what reads it. If the
//! tool isn't there, nothing happens: the taskbar's flash still does.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// As much as any of them shows, in characters.
const MAX_TITLE: usize = 64;
const MAX_BODY: usize = 256;
/// A tool that's stuck is left no longer than this.
const TOOL_LIMIT: Duration = Duration::from_secs(10);

/// Text that's safe to pass anywhere: one line, cut to length, with nothing that isn't shown. A
/// control character could end a line of an AppleScript, and a NUL can't be passed at all. The
/// direction marks can make a name read as something it isn't.
fn clean(text: &str, max: usize) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')).take(max).collect()
}

/// A notification's body can hold markup, by the specification `notify-send` follows, and a
/// repository could be named `<img src=...>`. Its title can't, so that's left alone.
fn body_markup_escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A string in an AppleScript: a backslash and a quote are all that can end or change one, once
/// `clean` has taken out line ends.
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The program and its arguments, as separate strings that nothing parses for a shell. By name of
/// system, so all of them can be tested on any.
fn command_for(system: &str, title: &str, body: &str) -> Option<(&'static str, Vec<String>)> {
    let (title, body) = (clean(title, MAX_TITLE), clean(body, MAX_BODY));
    match system {
        "windows" => None,
        "macos" => Some(("osascript", vec!["-e".to_string(), format!("display notification {} with title {}", applescript_string(&body), applescript_string(&title))])),
        // `--` so that a title beginning with a dash isn't taken as an option.
        _ => Some(("notify-send", vec!["--app-name=gitem".to_string(), "--".to_string(), title, body_markup_escaped(&body)])),
    }
}

/// Starts it and leaves it to end by itself, on a thread that's waiting so nothing else has to. A
/// tool that isn't there, or fails, or hangs, is nobody's problem but this thread's.
fn run_quietly(mut command: Command, limit: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let Ok(mut child) = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else { return };
        let t = Instant::now();
        while matches!(child.try_wait(), Ok(None)) {
            if t.elapsed() > limit {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
}

/// Whether there's a real window. Off in tests and screenshots, which have none, so they can't reach
/// the desktop. Windows has `set_window` for the same.
static HAS_WINDOW: AtomicBool = AtomicBool::new(false);

/// Only the real window says this.
pub fn window_is_up() {
    HAS_WINDOW.store(true, Ordering::Relaxed);
}

/// The real one. It does nothing until a real window has said it's there.
pub fn show(title: &str, body: &str) {
    #[cfg(windows)]
    win::show(title, body);
    if HAS_WINDOW.load(Ordering::Relaxed)
        && let Some((program, args)) = command_for(std::env::consts::OS, title, body)
    {
        let mut command = Command::new(program);
        command.args(args);
        run_quietly(command, TOOL_LIMIT);
    }
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
        assert!(!HAS_WINDOW.load(Ordering::Relaxed));
        show("Fetch finished", "gitem");
        clear();
    }

    const HOSTILE: &[&str] = &[
        "plain",
        "--help",
        "-u critical",
        "$(touch /tmp/owned)",
        "`touch /tmp/owned`",
        "; touch /tmp/owned; #",
        "a' & touch /tmp/owned & '",
        "\" & (do shell script \"touch /tmp/owned\") & \"",
        "\"; do shell script \"touch /tmp/owned\"; \"",
        "\\\"; do shell script \"touch /tmp/owned\"; \\\"",
        "ends in a backslash\\",
        "ends in two\\\\",
        "\"",
        "\\\"",
        "line one\ndo shell script \"touch /tmp/owned\"\r\n\u{0}nul\u{1b}[31m\t",
        "<img src=\"http://example.invalid/a.png\"/><b>&amp;</b>",
        "right\u{202e}gnp.exe",
        "ünïcödé 日本語 😀",
    ];

    /// Reads a script as AppleScript would, far enough to say what strings it has and what's
    /// outside them.
    fn applescript_parts(script: &str) -> (Vec<String>, String) {
        let (mut strings, mut outside) = (Vec::new(), String::new());
        let mut chars = script.chars();
        while let Some(c) = chars.next() {
            if c != '"' {
                outside.push(c);
                continue;
            }
            let mut string = String::new();
            loop {
                match chars.next().expect("a string left open") {
                    '"' => break,
                    '\\' => string.push(chars.next().expect("an escape left open")),
                    other => string.push(other),
                }
            }
            strings.push(string);
            outside.push('_');
        }
        (strings, outside)
    }

    #[test]
    fn nothing_in_the_text_gets_out_of_an_applescript_string() {
        for title in HOSTILE {
            for body in HOSTILE {
                let (program, args) = command_for("macos", title, body).unwrap();
                assert_eq!(program, "osascript");
                assert_eq!(args.len(), 2, "one script");
                assert_eq!(args[0], "-e");
                let (strings, outside) = applescript_parts(&args[1]);
                assert_eq!(outside, "display notification _ with title _", "{title} / {body}");
                assert_eq!(strings, [clean(body, MAX_BODY), clean(title, MAX_TITLE)], "and they read back as they were");
                assert!(!args[1].chars().any(char::is_control), "{:?}", args[1]);
            }
        }
    }

    #[test]
    fn notify_send_takes_no_option_from_the_text_and_no_markup_in_the_body() {
        for title in HOSTILE {
            for body in HOSTILE {
                let (program, args) = command_for("linux", title, body).unwrap();
                assert_eq!(program, "notify-send");
                assert_eq!(args.len(), 4, "always the same shape");
                assert_eq!(args[..2], ["--app-name=gitem", "--"], "the end of options comes before any text");
                assert_eq!(args[2], clean(title, MAX_TITLE));
                assert!(!args[3].contains(['<', '>']), "{}", args[3]);
                assert!(args.iter().all(|a| !a.chars().any(char::is_control)), "{args:?}");
                assert_eq!(args[3].replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"), clean(body, MAX_BODY), "and it reads back as it was");
            }
        }
        // Any other Unix goes the same way, and Windows has its own.
        assert_eq!(command_for("freebsd", "a", "b").unwrap().0, "notify-send");
        assert!(command_for("windows", "a", "b").is_none());
    }

    #[test]
    fn long_text_is_cut_and_what_isnt_shown_is_taken_out() {
        let (_, args) = command_for("linux", &"t".repeat(500), &"b".repeat(500)).unwrap();
        assert_eq!((args[2].len(), args[3].len()), (MAX_TITLE, MAX_BODY));
        assert_eq!(clean("a\nb\u{0}c\u{202e}d\u{2066}e", 100), "a b cde");
        // Cut by characters, so never through the middle of one.
        assert_eq!(clean("日本語", 2), "日本");
    }

    #[test]
    fn a_tool_thats_missing_fails_quietly() {
        run_quietly(Command::new("gitem-no-such-tool-anywhere"), TOOL_LIMIT).join().unwrap();
    }

    #[test]
    fn a_tool_that_fails_or_hangs_is_left_behind() {
        let mut fails = Command::new("git");
        fails.arg("no-such-command-anywhere");
        run_quietly(fails, TOOL_LIMIT).join().unwrap();
        let mut hangs = Command::new(if cfg!(windows) { "ping" } else { "sleep" });
        if cfg!(windows) {
            hangs.args(["-n", "30", "127.0.0.1"]);
        } else {
            hangs.arg("30");
        }
        let t = Instant::now();
        run_quietly(hangs, Duration::from_millis(100)).join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    }
}
