//! Drawing an SVG, which in a repository is untrusted input: it can claim any size, nest without
//! end, expand itself like a zip bomb, or point at files and addresses. So it's looked over before
//! the renderer sees it, the picture has a fixed largest size, and nothing the file names is ever
//! read: no pictures, no fonts, no styles, and scripts aren't run by anything here. The only fonts
//! are this computer's and the one that came with gitem, and only for a file with text in it.
//!
//! A thread can't be stopped from outside and the renderer has nowhere to ask whether to go on, so
//! it runs in a process of its own: gitem's own executable with a hidden option, handed the file on
//! stdin, answering on stdout. That process can be killed at the time limit, and when whoever was
//! waiting has gone. A hostile file that crashes it costs one picture.
//!
//! All the limits here are applied on the child's side. What it answers is untrusted too, so the
//! parent reads only so much of it and checks its size again. It's not a sandbox: the child has the
//! user's rights, and what keeps it from files and the network is the renderer's options below.
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Condvar, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use resvg::usvg;

/// Bigger than this is most likely not a drawing someone wants to look at.
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Elements, once `use` has been followed.
const MAX_ELEMENTS: u64 = 200_000;
/// Well past what any drawing tool writes, and well short of what would run out of stack.
const MAX_DEPTH: usize = 128;
/// Each one can go over the whole picture, so these are what can take a long time.
const MAX_FILTERS: usize = 64;
/// The longest side of the picture made, however large it says it is.
pub const MAX_SIDE: f32 = 2048.0;
/// Nothing's drawn larger than this for being small to begin with.
const MAX_ENLARGE: f32 = 2.0;
/// Entity text that could be written out, in bytes: their sizes times how often they could be used.
const MAX_ENTITY_OUTPUT: usize = 16 * 1024 * 1024;
const STACK: usize = 16 * 1024 * 1024;
/// Elements holding text, once `use` has been followed. Each is shaped by itself.
const MAX_TEXT_ELEMENTS: u64 = 5_000;
/// Characters of text, with entities and `use` written out.
const MAX_TEXT_CHARS: u64 = 200_000;
/// How many `use` can lead through each other. Looking it over goes down them on the stack.
const MAX_USE_DEPTH: usize = 256;
/// How long a view waits for a picture. Then the child is killed.
const TIME_LIMIT: Duration = Duration::from_secs(10);
/// How long the child lets itself run, in case the parent is stopped and not gone.
const CHILD_OWN_LIMIT: Duration = Duration::from_secs(15);
/// Children running at once. More wait for a turn.
const MAX_CHILDREN: usize = 2;
/// Said for one that didn't end in time, crashed, or answered with something that isn't an answer.
pub const GAVE_UP: &str = "It took too long to draw, or stopped the renderer, so gitem gave up on it. Source still shows it.";
pub const NO_TURN: &str = "Other SVGs are taking too long to draw, so this one wasn't started. Source still shows it.";
/// Nobody's waiting for this, so nobody reads it.
const STOPPED: &str = "Whoever asked for this has gone.";
/// The hidden option, ahead of all the others in `main`.
pub const CHILD_ARG: &str = "--render-svg";
/// All a child gets of the parent's environment, which can hold tokens. Where fonts are, and what
/// Windows needs to start a process.
const KEPT_ENV: &[&str] = &["SYSTEMROOT", "WINDIR", "USERPROFILE", "HOME", "XDG_DATA_HOME", "XDG_DATA_DIRS", "XDG_CONFIG_HOME", "XDG_CONFIG_DIRS", "FONTCONFIG_FILE", "FONTCONFIG_PATH"];

/// An entity that refers to another grows by powers, and only a few lines of that fill the memory.
/// One that doesn't is ordinary: some drawing tools write them for namespaces.
fn entities_are_safe(text: &str) -> Result<(), String> {
    let mut total = 0;
    let mut rest = text;
    while let Some(at) = rest.find("<!ENTITY") {
        let after = &rest[at..];
        let end = after.find('>').ok_or("It isn't well formed.")?;
        let declared = &after[..end];
        if declared.contains('&') {
            return Err("It defines text in terms of other text, which can grow without limit.".to_string());
        }
        total += declared.len();
        rest = &after[end..];
    }
    if total > 0 && total.saturating_mul(text.matches('&').count()) > MAX_ENTITY_OUTPUT {
        return Err("Its defined text would expand to too much.".to_string());
    }
    Ok(())
}

/// The `#id` a `use` points at.
fn use_target<'a>(n: &roxmltree::Node<'a, '_>) -> Option<&'a str> {
    const XLINK: &str = "http://www.w3.org/1999/xlink";
    n.attribute("href").or_else(|| n.attribute((XLINK, "href")))?.strip_prefix('#')
}

/// What's under an element with each `use` written out.
#[derive(Clone, Copy, Default)]
struct Tally {
    elements: u64,
    text_elements: u64,
    text_chars: u64,
}

impl Tally {
    fn add(&mut self, o: Tally) {
        self.elements = self.elements.saturating_add(o.elements);
        self.text_elements = self.text_elements.saturating_add(o.text_elements);
        self.text_chars = self.text_chars.saturating_add(o.text_chars);
    }
}

fn holds_text(n: &roxmltree::Node) -> bool {
    matches!(n.tag_name().name(), "text" | "tspan" | "textPath")
}

fn tally_under<'a, 'i>(n: roxmltree::Node<'a, 'i>, ids: &std::collections::HashMap<&'a str, roxmltree::Node<'a, 'i>>, known: &mut std::collections::HashMap<roxmltree::NodeId, Tally>, walking: &mut Vec<roxmltree::NodeId>) -> Result<Tally, String> {
    if let Some(c) = known.get(&n.id()) {
        return Ok(*c);
    }
    if walking.contains(&n.id()) {
        return Err("It refers to itself.".to_string());
    }
    if walking.len() > MAX_USE_DEPTH {
        return Err("It nests too deeply, once what it repeats is followed.".to_string());
    }
    walking.push(n.id());
    let mut count = Tally { elements: 1, ..Default::default() };
    if holds_text(&n) {
        count.text_elements = 1;
        count.text_chars = n.children().filter(|c| c.is_text()).map(|c| c.text().map_or(0, |t| t.chars().count() as u64)).sum();
    }
    for c in n.children().filter(|c| c.is_element()) {
        count.add(tally_under(c, ids, known, walking)?);
    }
    if n.tag_name().name() == "use"
        && let Some(target) = use_target(&n).and_then(|t| ids.get(t))
    {
        count.add(tally_under(*target, ids, known, walking)?);
    }
    walking.pop();
    known.insert(n.id(), count);
    Ok(count)
}

/// Whether this can be given to the renderer, and if not, why. If so, what's in it.
fn look_over(text: &str) -> Result<Tally, String> {
    entities_are_safe(text)?;
    let options = roxmltree::ParsingOptions { allow_dtd: true, nodes_limit: 4 * MAX_ELEMENTS as u32, ..Default::default() };
    let doc = roxmltree::Document::parse_with_options(text, options).map_err(|e| if matches!(e, roxmltree::Error::NodesLimitReached) { "It has too many parts.".to_string() } else { format!("It isn't well formed: {e}") })?;
    let mut filters = 0;
    let mut ids = std::collections::HashMap::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        if n.ancestors().count() > MAX_DEPTH {
            return Err("It nests too deeply.".to_string());
        }
        if n.has_attribute("filter") || n.attribute("style").is_some_and(|s| s.contains("filter")) || n.tag_name().name().starts_with("fe") {
            filters += 1;
        }
        if let Some(id) = n.attribute("id") {
            ids.insert(id, n);
        }
    }
    if filters > MAX_FILTERS {
        return Err("It has too many filters.".to_string());
    }
    let mut known = std::collections::HashMap::new();
    let all = tally_under(doc.root_element(), &ids, &mut known, &mut Vec::new())?;
    if all.elements > MAX_ELEMENTS {
        return Err("It has too many parts, once what it repeats is counted.".to_string());
    }
    if all.text_elements > MAX_TEXT_ELEMENTS || all.text_chars > MAX_TEXT_CHARS {
        return Err("It has too much text, once what it repeats is counted.".to_string());
    }
    Ok(all)
}

/// This computer's fonts and the one gitem came with, and that one's id. It reads a folder of files,
/// in each child that has text to draw.
fn fonts() -> &'static (Arc<usvg::fontdb::Database>, Option<usvg::fontdb::ID>) {
    static FONTS: OnceLock<(Arc<usvg::fontdb::Database>, Option<usvg::fontdb::ID>)> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        let own = egui::FontDefinitions::default().font_data.get("Ubuntu-Light").and_then(|f| db.load_font_source(usvg::fontdb::Source::Binary(Arc::new(f.font.to_vec()))).first().copied());
        (Arc::new(db), own)
    })
}

/// Whether a font this asks for by name is on this computer. Asking for "serif" and the like can't
/// be wrong, and the renderer's own choice would quietly go to one of those.
fn named_font_is_missing(families: &[usvg::FontFamily], db: &usvg::fontdb::Database) -> bool {
    let names: Vec<&str> = families.iter().filter_map(|f| if let usvg::FontFamily::Named(n) = f { Some(n.as_str()) } else { None }).collect();
    names.len() == families.len() && !names.is_empty() && !db.faces().any(|face| face.families.iter().any(|(name, _)| names.iter().any(|n| name.eq_ignore_ascii_case(n))))
}

fn options(fontdb: Arc<usvg::fontdb::Database>, own_font: Option<usvg::fontdb::ID>, missing: Arc<AtomicBool>) -> usvg::Options<'static> {
    let (chosen, by_character) = (usvg::FontResolver::default_font_selector(), usvg::FontResolver::default_fallback_selector());
    let missing_one = missing.clone();
    usvg::Options {
        resources_dir: None,
        fontdb,
        font_resolver: usvg::FontResolver {
            // Nothing it asks for loads anything: these only choose among what's already here.
            select_font: Box::new(move |font, db| {
                if named_font_is_missing(font.families(), db) {
                    missing_one.store(true, Ordering::Relaxed);
                    return own_font.or_else(|| chosen(font, db));
                }
                let found = chosen(font, db).or(own_font);
                if found.is_none() {
                    missing_one.store(true, Ordering::Relaxed);
                }
                found
            }),
            select_fallback: Box::new(move |c, used, db| {
                let found = by_character(c, used, db);
                if found.is_none() {
                    missing.store(true, Ordering::Relaxed);
                }
                found
            }),
        },
        // A path, an address or embedded data: nothing's fetched, opened or decoded.
        image_href_resolver: usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) },
        ..Default::default()
    }
}

/// A picture, and whether its text wanted a font or a character this computer doesn't have.
pub struct Drawn {
    pub image: egui::ColorImage,
    pub font_missing: bool,
}

/// Said beside a picture with `font_missing`.
pub const FONT_MISSING: &str = "Its text asks for something this computer's fonts don't have, so some is in another font or not drawn. Source has it.";

/// Fonts are the slow part of a child starting, and most drawings have no text.
fn draw(text: &str) -> Result<Drawn, String> {
    let found = look_over(text)?;
    if found.text_elements == 0 {
        return draw_with(text, Arc::new(usvg::fontdb::Database::new()), None);
    }
    let (db, own) = fonts();
    draw_with(text, db.clone(), *own)
}

fn draw_with(text: &str, db: Arc<usvg::fontdb::Database>, own: Option<usvg::fontdb::ID>) -> Result<Drawn, String> {
    let missing = Arc::new(AtomicBool::new(false));
    let tree = usvg::Tree::from_str(text, &options(db, own, missing.clone())).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("It says its size is nothing.".to_string());
    }
    let scale = (MAX_SIDE / w).min(MAX_SIDE / h).min(MAX_ENLARGE);
    let (pw, ph) = (((w * scale).ceil() as u32).clamp(1, MAX_SIDE as u32), ((h * scale).ceil() as u32).clamp(1, MAX_SIDE as u32));
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or("It can't be drawn at that size.")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(Drawn { image: egui::ColorImage::from_rgba_premultiplied([pw as usize, ph as usize], pixmap.data()), font_missing: missing.load(Ordering::Relaxed) })
}

fn draw_bytes(bytes: &[u8]) -> Result<Drawn, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("It's over {} MB.", MAX_BYTES / (1024 * 1024)));
    }
    draw(std::str::from_utf8(bytes).map_err(|_| "It isn't UTF-8.".to_string())?)
}

/// On a stack large enough for the limits above. None if the renderer panicked.
fn on_own_stack(bytes: Vec<u8>) -> Option<Result<Drawn, String>> {
    std::thread::Builder::new().name("svg".to_string()).stack_size(STACK).spawn(move || draw_bytes(&bytes)).ok()?.join().ok()
}

// ---- what the child answers: `GSV1`, then 0, width, height, whether a font's missing and the
// pixels, or 1 and why it was refused.

const MAGIC: &[u8; 4] = b"GSV1";
const PICTURE_HEADER: usize = MAGIC.len() + 1 + 4 + 4 + 1;
const MAX_WHY: usize = 1024;
/// The largest answer that could be right. The parent reads no more than this.
const MAX_ANSWER: usize = PICTURE_HEADER + (MAX_SIDE as usize) * (MAX_SIDE as usize) * 4;

fn answer(result: &Result<Drawn, String>) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    match result {
        Ok(d) => {
            out.reserve_exact(PICTURE_HEADER + d.image.pixels.len() * 4);
            out.push(0);
            out.extend_from_slice(&(d.image.size[0] as u32).to_le_bytes());
            out.extend_from_slice(&(d.image.size[1] as u32).to_le_bytes());
            out.push(d.font_missing as u8);
            out.extend(d.image.pixels.iter().flat_map(|p| p.to_array()));
        }
        Err(why) => {
            out.push(1);
            let mut end = why.len().min(MAX_WHY);
            while !why.is_char_boundary(end) {
                end -= 1;
            }
            out.extend_from_slice(&why.as_bytes()[..end]);
        }
    }
    out
}

/// What a child answered, which is no more to be trusted than the file it was given. Anything that
/// isn't exactly right is told as a renderer gone wrong, as a crash is.
fn read_answer(bytes: &[u8]) -> Result<Drawn, String> {
    let wrong = || GAVE_UP.to_string();
    let rest = bytes.strip_prefix(MAGIC).ok_or_else(wrong)?;
    let (&kind, rest) = rest.split_first().ok_or_else(wrong)?;
    match kind {
        0 => {
            let (head, pixels) = rest.split_at_checked(9).ok_or_else(wrong)?;
            let (w, h) = (u32::from_le_bytes([head[0], head[1], head[2], head[3]]), u32::from_le_bytes([head[4], head[5], head[6], head[7]]));
            let side = 1..=MAX_SIDE as u32;
            if !side.contains(&w) || !side.contains(&h) || head[8] > 1 || pixels.len() as u64 != u64::from(w) * u64::from(h) * 4 {
                return Err(wrong());
            }
            Ok(Drawn { image: egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixels), font_missing: head[8] == 1 })
        }
        // It goes on the screen, so it's only ever a short line of text.
        1 if rest.len() <= MAX_WHY => Err(std::str::from_utf8(rest).map_err(|_| wrong())?.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()),
        _ => Err(wrong()),
    }
}

// ---- the child

/// A crash here is one picture that doesn't show, so nothing about it may reach the user directly:
/// no box from Windows, no crash report offered at the next launch.
fn keep_crashes_quiet() {
    std::panic::set_hook(Box::new(|_| {}));
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode};
        // SAFETY: only sets a flag on this process.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        }
    }
}

/// Ends this process when the parent does, however it does: its end of stdin closes then. And in
/// any case at the time limit, as a parent that's only stopped closes nothing.
fn end_with_the_parent() {
    std::thread::spawn(|| {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        std::process::exit(5);
    });
    std::thread::spawn(|| {
        std::thread::sleep(CHILD_OWN_LIMIT);
        std::process::exit(6);
    });
}

/// `gitem --render-svg`. Opens no window and reads no settings: `main` comes here first. Nothing
/// happens until the file has arrived, and the parent sends it only once it can kill this process.
pub fn child_main() -> i32 {
    keep_crashes_quiet();
    let mut input = std::io::stdin();
    let mut size = [0; 8];
    if input.read_exact(&mut size).is_err() {
        return 2;
    }
    let size = u64::from_le_bytes(size);
    if size > MAX_BYTES as u64 {
        return 2;
    }
    let mut bytes = vec![0; size as usize];
    if input.read_exact(&mut bytes).is_err() {
        return 2;
    }
    #[cfg(debug_assertions)]
    if let Ok(mode) = std::env::var(TEST_MODE) {
        return act_for_test(&mode);
    }
    end_with_the_parent();
    let Some(result) = on_own_stack(bytes) else { return 3 };
    let mut out = std::io::stdout().lock();
    if out.write_all(&answer(&result)).is_err() || out.flush().is_err() {
        return 4;
    }
    0
}

/// Only by this variable, which only a test sets, and only in a debug build: nothing in a file can
/// get here. The parent passes on nothing else of its environment, so it's not read from the user's.
#[cfg(debug_assertions)]
const TEST_MODE: &str = "GITEM_SVG_TEST";

#[cfg(debug_assertions)]
fn act_for_test(mode: &str) -> i32 {
    let say = |bytes: &[u8]| {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    };
    let picture = |w: u32, h: u32, pixels: usize| {
        let mut out = MAGIC.to_vec();
        out.push(0);
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.push(0);
        out.resize(out.len() + pixels, 255);
        out
    };
    // A minute, so a test that goes wrong can't leave one for good.
    let never_ending = || std::thread::sleep(Duration::from_secs(60));
    match mode {
        // Ends with the parent, as the real thing does.
        "hang" => {
            end_with_the_parent();
            never_ending();
        }
        // Ends only if it's killed. This is the renderer stuck where nothing else can run.
        "hang-deaf" => never_ending(),
        "panic" => panic!("a panic asked for by a test"),
        "abort" => std::process::abort(),
        "exit-quietly" => {}
        "garbage" => say(b"this is not an answer"),
        "wrong-size" => say(&picture(2, 2, 3)),
        "too-many-pixels" => say(&picture(2, 2, 17)),
        "too-wide" => say(&picture(MAX_SIDE as u32 + 1, 1, (MAX_SIDE as usize + 1) * 4)),
        "overflowing-sides" => say(&picture(u32::MAX, u32::MAX, 4)),
        "long-why" => say(&[&MAGIC[..], &[1], &vec![b'x'; MAX_WHY + 1]].concat()),
        "control-characters" => say(&[&MAGIC[..], &[1], b"line\x1b[31m\none"].concat()),
        "flood" => {
            let mut out = std::io::stdout().lock();
            while out.write_all(&[7; 65536]).is_ok() {}
        }
        "answer-then-hang" => {
            say(&answer(&Err("too early".to_string())));
            never_ending();
        }
        "environment" => {
            let names: Vec<String> = std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
            say(&answer(&Err(names.join(" "))));
        }
        _ => return 9,
    }
    0
}

// ---- the parent

/// How to run a child. `Default` is how the app does.
pub struct Renderer {
    pub exe: PathBuf,
    pub limit: Duration,
    /// What a debug build's child should do instead of drawing. Release builds ignore it.
    pub test_mode: Option<&'static str>,
    /// Where a test hears the id of each child started.
    pub started: Option<Started>,
}

pub type Started = Arc<Mutex<Vec<u32>>>;

static EXE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// For tests, whose own executable isn't gitem.
pub fn use_renderer(exe: impl Into<PathBuf>) {
    *EXE.lock().unwrap_or_else(|e| e.into_inner()) = Some(exe.into());
}

impl Default for Renderer {
    fn default() -> Self {
        let exe = EXE.lock().unwrap_or_else(|e| e.into_inner()).clone().or_else(|| std::env::current_exe().ok()).unwrap_or_default();
        Self { exe, limit: TIME_LIMIT, test_mode: None, started: None }
    }
}

/// Draws one in a child, off whichever thread this is called on. Not from the UI thread: it waits.
/// Once `cancel` is set the child is killed and nothing more is done.
pub fn render(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<Drawn, String> {
    #[allow(unused_mut)]
    let mut renderer = Renderer::default();
    #[cfg(debug_assertions)]
    if let Some(slow) = SLOW_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| bytes.windows(s.marker.len()).any(|w| w == s.marker.as_bytes())) {
        renderer.limit = slow.limit;
        renderer.test_mode = Some("hang-deaf");
        renderer.started = Some(slow.started.clone());
    }
    renderer.render(bytes, cancel)
}

impl Renderer {
    pub fn render(&self, bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<Drawn, String> {
        if bytes.len() > MAX_BYTES {
            return Err(format!("It's over {} MB.", MAX_BYTES / (1024 * 1024)));
        }
        static TURNS: LazyLock<Arc<Slots>> = LazyLock::new(|| Arc::new(Slots::new(MAX_CHILDREN)));
        let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
        // The ones ahead of this are killed at the limit, so a turn comes by then if there's one.
        let _turn = TURNS.take(self.limit.saturating_mul(2), &cancelled)?;
        let mut child = Running::start(self).map_err(|e| format!("gitem couldn't start its renderer: {e}"))?;
        let until = Instant::now() + self.limit;
        let answered = child.exchange(bytes);
        loop {
            if cancelled() {
                return Err(STOPPED.to_string());
            }
            let left = until.checked_duration_since(Instant::now()).ok_or(GAVE_UP)?;
            match answered.recv_timeout(left.min(Duration::from_millis(10))) {
                Ok(got) => return read_answer(&got),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(GAVE_UP.to_string()),
            }
        }
    }
}

/// Which children are running, by process id. For tests, to see one go.
pub fn children_running() -> Vec<u32> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// A child, killed and waited for when this is dropped, however its parent stopped waiting.
struct Running {
    child: Child,
    /// Open for as long as it runs, as its end closing is what tells the child the parent's gone.
    stdin: Option<Arc<ChildStdin>>,
}

impl Running {
    fn start(with: &Renderer) -> std::io::Result<Self> {
        let mut cmd = Command::new(&with.exe);
        cmd.arg(CHILD_ARG).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).env_clear();
        for name in KEPT_ENV {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        #[cfg(debug_assertions)]
        if let Some(mode) = with.test_mode {
            cmd.env(TEST_MODE, mode);
        }
        // Not wherever the parent is: on Windows a process holds its folder against being removed.
        let quiet_place = std::env::temp_dir();
        if quiet_place.is_dir() {
            cmd.current_dir(quiet_place);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        #[cfg(windows)]
        job::hold(&child);
        LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(child.id());
        if let Some(started) = &with.started {
            started.lock().unwrap_or_else(|e| e.into_inner()).push(child.id());
        }
        let stdin = child.stdin.take().map(Arc::new);
        Ok(Self { child, stdin })
    }

    /// Sends the file and reads the answer, each on a thread of its own as a hostile child needn't
    /// do either. Killing the child ends both. What comes back is one read more than could be right,
    /// so one that goes on writing is found out without it filling the memory.
    fn exchange(&mut self, bytes: &[u8]) -> std::sync::mpsc::Receiver<Vec<u8>> {
        if let Some(stdin) = self.stdin.clone() {
            let message = [&(bytes.len() as u64).to_le_bytes()[..], bytes].concat();
            std::thread::spawn(move || {
                let _ = (&*stdin).write_all(&message);
                let _ = (&*stdin).flush();
            });
        }
        let (tx, rx) = channel();
        if let Some(stdout) = self.child.stdout.take() {
            std::thread::spawn(move || {
                let mut got = Vec::new();
                if stdout.take(MAX_ANSWER as u64 + 1).read_to_end(&mut got).is_ok() {
                    let _ = tx.send(got);
                }
            });
        }
        rx
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        let id = self.child.id();
        LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| *p != id);
    }
}

/// Windows ends every process in a job when the last handle to the job closes, and this process
/// holds the only one, so nothing outlives it, however it ends. It's created when first needed and
/// never closed. If it can't be, a child still ends when its stdin does, unless it's stuck in a way
/// that stops that thread too.
#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::sync::LazyLock;

    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject};

    /// As a number, since a handle isn't `Send`. It's only passed back to the calls below.
    static JOB: LazyLock<Option<usize>> = LazyLock::new(|| {
        // SAFETY: no name and no attributes, so it can't be found or inherited. The information
        // passed is fully initialised, its size is given, and it outlives the call.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&raw const limits).cast(), size_of_val(&limits) as u32);
            (set != 0).then_some(job as usize)
        }
    });

    pub(super) fn hold(child: &std::process::Child) {
        if let Some(job) = *JOB {
            // SAFETY: both handles are open: the job for good, the child for as long as it's borrowed.
            unsafe {
                AssignProcessToJobObject(job as _, child.as_raw_handle() as _);
            }
        }
    }
}

/// Places for children to run in, one held from before a child starts until it's gone.
struct Slots {
    max: usize,
    taken: Mutex<usize>,
    freed: Condvar,
}

impl Slots {
    fn new(max: usize) -> Self {
        Self { max, taken: Mutex::new(0), freed: Condvar::new() }
    }

    /// One, if one comes free in this time and nobody calls it off first.
    fn take(self: &Arc<Self>, patience: Duration, cancelled: &dyn Fn() -> bool) -> Result<Slot, String> {
        let until = Instant::now() + patience;
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancelled() {
                return Err(STOPPED.to_string());
            }
            if *taken < self.max {
                *taken += 1;
                return Ok(Slot(self.clone()));
            }
            let left = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()).ok_or(NO_TURN)?;
            taken = self.freed.wait_timeout(taken, left.min(Duration::from_millis(50))).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// Given back when its child has gone.
struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        *self.0.taken.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.freed.notify_one();
    }
}

#[cfg(debug_assertions)]
struct SlowForTest {
    marker: &'static str,
    limit: Duration,
    started: Started,
}

#[cfg(debug_assertions)]
static SLOW_FOR_TEST: Mutex<Vec<SlowForTest>> = Mutex::new(Vec::new());

/// For a test of what a view does: a file with this in it gets a child that never ends, killed at
/// this limit, and the ids of those are returned. Debug builds only, in the test's own process: a
/// test has to ask, or a file's content means nothing.
#[cfg(debug_assertions)]
pub fn slow_for_test(marker: &'static str, limit: Duration) -> Started {
    let started = Started::default();
    SLOW_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).push(SlowForTest { marker, limit, started: started.clone() });
    started
}

pub fn is_svg(path: &str) -> bool {
    path.to_lowercase().ends_with(".svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limits and the drawing, in this process. `svg_process.rs` has the real child.
    fn render(bytes: &[u8]) -> Result<Drawn, String> {
        on_own_stack(bytes.to_vec()).expect("no panic")
    }

    const RED: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;

    fn opaque(i: &egui::ColorImage) -> usize {
        i.pixels.iter().filter(|p| p.a() > 0).count()
    }

    #[test]
    fn an_ordinary_one_is_drawn_and_a_small_one_enlarged_a_little() {
        let d = render(RED.as_bytes()).unwrap();
        assert_eq!(d.image.size, [40, 20], "at twice its size");
        assert_eq!(d.image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert!(!d.font_missing);
    }

    fn text_in(font: &str) -> String {
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30"><text x="2" y="20" font-size="16" {font}>Words</text></svg>"#)
    }

    #[test]
    fn text_is_drawn_in_gitems_own_font_when_the_one_named_is_there() {
        let d = render(text_in(r#"font-family="Ubuntu""#).as_bytes()).unwrap();
        assert!(opaque(&d.image) > 50, "{}", opaque(&d.image));
        assert!(!d.font_missing);
    }

    /// Not a picture that quietly looks wrong.
    #[test]
    fn a_font_that_isnt_here_is_said_and_gitems_own_stands_in() {
        let d = render(text_in(r#"font-family="No Such Font, Nor This""#).as_bytes()).unwrap();
        assert!(opaque(&d.image) > 50, "{}", opaque(&d.image));
        assert!(d.font_missing);
        // One of the names being here is enough, and so is asking for no name in particular.
        assert!(!render(text_in(r#"font-family="No Such Font, Ubuntu""#).as_bytes()).unwrap().font_missing);
    }

    #[test]
    fn with_no_fonts_at_all_the_text_is_left_out_and_said() {
        let d = draw_with(&text_in(r#"font-family="Ubuntu""#), Arc::new(usvg::fontdb::Database::new()), None).unwrap();
        assert_eq!(opaque(&d.image), 0);
        assert!(d.font_missing);
    }

    /// Fonts come from what's here. It can't name a file of its own, or fetch one.
    #[test]
    fn a_font_face_in_the_file_reaches_for_nothing() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30"><style>@font-face {{ font-family: Far; src: url("http://127.0.0.1:{port}/f.ttf"); }}</style><text x="2" y="20" font-size="16" font-family="Far">Words</text></svg>"#);
        assert!(render(doc.as_bytes()).unwrap().font_missing);
        std::thread::sleep(Duration::from_millis(200));
        assert!(listener.accept().is_err(), "nothing called");
    }

    #[test]
    fn too_much_text_is_refused_before_any_of_it_is_shaped() {
        let many = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9">{}</svg>"#, "<text>x</text>".repeat(5_001));
        assert!(render(many.as_bytes()).err().unwrap().contains("too much text"));
        let spans = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><text>{}</text></svg>"#, "<tspan>x</tspan>".repeat(5_001));
        assert!(render(spans.as_bytes()).err().unwrap().contains("too much text"));
        let long = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><text>{}</text></svg>"#, "x".repeat(200_001));
        assert!(render(long.as_bytes()).err().unwrap().contains("too much text"));
        // Small in the file, large once it's repeated.
        let mut grown = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="9"><defs><text id="a0">{}</text>"#, "x".repeat(1000));
        for n in 1..=2 {
            grown.push_str(&format!("<g id='a{n}'>{}</g>", format!("<use href='#a{}'/>", n - 1).repeat(40)));
        }
        grown.push_str("</defs><use href='#a2'/></svg>");
        assert!(render(grown.as_bytes()).err().unwrap().contains("too much text"));
    }

    #[test]
    fn a_long_chain_of_use_is_refused_without_running_out_of_stack() {
        let mut doc = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg">"#);
        for n in 0..60_000 {
            doc.push_str(&format!("<g id='a{n}'><use href='#a{}'/></g>", n + 1));
        }
        doc.push_str("</svg>");
        assert!(render(doc.as_bytes()).err().unwrap().contains("nests too deeply"));
    }

    /// Within the limits, and as awkward as it can be: it ends, at the picture's size and no larger.
    /// The time limit is the parent's.
    #[test]
    fn text_within_the_limits_stays_inside_them() {
        let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="900" height="900">{}</svg>"#, r#"<text x="1" y="50" font-size="40">Some words</text>"#.repeat(2_000));
        let t = Instant::now();
        let d = render(doc.as_bytes()).unwrap();
        assert!(d.image.size[0] <= 2048 && opaque(&d.image) > 0);
        eprintln!("2,000 text elements: {:?}", t.elapsed());
        for huge in [r#"font-size="100000000""#, r#"font-size="1" letter-spacing="1e30""#, r#"font-size="1e30""#] {
            let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="90" height="90"><text x="1" y="50" {huge}>Words</text></svg>"#);
            if let Ok(d) = render(doc.as_bytes()) {
                assert!(d.image.size[0] <= 2048, "{huge}");
            }
        }
    }

    // ---- what a child answers, which is as untrusted as the file

    fn picture(w: u32, h: u32, flag: u8, pixels: usize) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(0);
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.push(flag);
        out.resize(out.len() + pixels, 9);
        out
    }

    #[test]
    fn a_good_answer_comes_back_as_it_went() {
        let d = render(RED.as_bytes()).unwrap();
        let back = read_answer(&answer(&Ok(d))).unwrap();
        assert_eq!(back.image.size, [40, 20]);
        assert_eq!(back.image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert!(!back.font_missing);
        assert_eq!(read_answer(&answer(&Err("It nests too deeply.".to_string()))).err().as_deref(), Some("It nests too deeply."));
        // The largest there is, at exactly what the parent's willing to read.
        let side = MAX_SIDE as u32;
        let full = picture(side, side, 1, (side * side * 4) as usize);
        assert_eq!(full.len(), MAX_ANSWER);
        assert!(read_answer(&full).unwrap().font_missing);
    }

    #[test]
    fn a_reason_too_long_is_cut_on_a_character_for_sending() {
        let long = "é".repeat(MAX_WHY);
        let sent = answer(&Err(long));
        assert!(sent.len() <= MAGIC.len() + 1 + MAX_WHY);
        assert!(read_answer(&sent).err().unwrap().starts_with('é'));
    }

    #[test]
    fn an_answer_that_isnt_right_is_told_as_a_renderer_gone_wrong() {
        let side = MAX_SIDE as u32;
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("nothing", Vec::new()),
            ("garbage", b"this is not an answer".to_vec()),
            ("magic only", MAGIC.to_vec()),
            ("unknown kind", [&MAGIC[..], &[2]].concat()),
            ("header cut short", picture(2, 2, 0, 0)[..12].to_vec()),
            ("too few pixels", picture(2, 2, 0, 15)),
            ("too many pixels", picture(2, 2, 0, 17)),
            ("no width", picture(0, 2, 0, 0)),
            ("no height", picture(2, 0, 0, 0)),
            ("too wide", picture(side + 1, 1, 0, ((side + 1) * 4) as usize)),
            ("too tall", picture(1, side + 1, 0, ((side + 1) * 4) as usize)),
            ("sides whose product wraps", picture(0x8000_0000, 2, 0, 0)),
            ("the largest sides", picture(u32::MAX, u32::MAX, 0, 4)),
            ("a flag that's no flag", picture(1, 1, 2, 4)),
            ("a reason too long", [&MAGIC[..], &[1], &vec![b'x'; MAX_WHY + 1]].concat()),
            ("a reason that isn't text", [&MAGIC[..], &[1], &[0xff, 0xfe]].concat()),
        ];
        for (what, bytes) in cases {
            assert_eq!(read_answer(&bytes).err().as_deref(), Some(GAVE_UP), "{what}");
        }
    }

    #[test]
    fn a_reason_goes_on_the_screen_without_its_control_characters() {
        let e = read_answer(&[&MAGIC[..], &[1], b"line\x1b[31m\none"].concat()).err().unwrap();
        assert_eq!(e, "line [31m one");
    }

    // ---- turns

    #[test]
    fn a_turn_is_taken_or_waited_for_or_called_off() {
        let never = || false;
        let slots = Arc::new(Slots::new(1));
        let held = slots.take(Duration::ZERO, &never).unwrap();
        assert_eq!(slots.take(Duration::from_millis(20), &never).err().as_deref(), Some(NO_TURN));
        let flag = AtomicBool::new(true);
        let t = Instant::now();
        let called_off = slots.take(Duration::from_secs(30), &|| flag.load(Ordering::Relaxed));
        assert_eq!(called_off.err().as_deref(), Some(STOPPED));
        assert!(t.elapsed() < Duration::from_secs(20));
        let waiter = slots.clone();
        let later = std::thread::spawn(move || waiter.take(Duration::from_secs(30), &|| false).is_ok());
        drop(held);
        assert!(later.join().unwrap());
        assert_eq!(*slots.taken.lock().unwrap(), 0, "given back");
    }

    #[test]
    fn huge_dimensions_come_out_at_the_largest_size_and_no_larger() {
        let t = std::time::Instant::now();
        let i = render(br#"<svg xmlns="http://www.w3.org/2000/svg" width="1000000000" height="500000000"><rect width="1000000000" height="500000000" fill="red"/></svg>"#).unwrap();
        assert_eq!(i.image.size, [2048, 1024]);
        assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
        // Past what a float holds: refused, or inside the same limit.
        if let Ok(d) = render(br#"<svg xmlns="http://www.w3.org/2000/svg" width="1e308" height="1e308"></svg>"#) {
            assert!(d.image.size[0] <= 2048 && d.image.size[1] <= 2048, "{:?}", d.image.size);
        }
        // A wide sliver stays inside as well.
        let i = render(br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="1"></svg>"#).unwrap();
        assert!(i.image.size[0] <= 2048 && i.image.size[1] >= 1, "{:?}", i.image.size);
    }

    #[test]
    fn deep_nesting_is_refused_without_running_out_of_stack() {
        for depth in [200, 5000, 100_000] {
            let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{}{}</svg>"#, "<g>".repeat(depth), "</g>".repeat(depth));
            let e = render(doc.as_bytes()).err().unwrap();
            assert!(e.contains("nests") || e.contains("well formed") || e.contains("too many"), "{depth}: {e}");
        }
        // Within the limit it's drawn, on its own stack.
        let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{}<rect width="4" height="4" fill="red"/>{}</svg>"#, "<g>".repeat(100), "</g>".repeat(100));
        assert!(opaque(&render(doc.as_bytes()).unwrap().image) > 0);
    }

    #[test]
    fn nothing_outside_the_file_is_reached() {
        // A listener that notes anyone calling, and a picture beside where this runs.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("secret.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255])).save(&local).unwrap();
        let local = local.to_string_lossy().replace('\\', "/");
        let doc = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="8">
            <style>@import url("http://127.0.0.1:{port}/a.css");</style>
            <script xlink:href="http://127.0.0.1:{port}/a.js">alert(1)</script>
            <image href="http://127.0.0.1:{port}/a.png" width="8" height="8"/>
            <image href="{local}" width="8" height="8"/>
            <image xlink:href="file:///{local}" width="8" height="8"/>
            <image href="secret.png" width="8" height="8"/>
            <image href="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==" width="8" height="8"/>
            <use href="other.svg#x"/>
            <foreignObject width="8" height="8"><iframe xmlns="http://www.w3.org/1999/xhtml" src="http://127.0.0.1:{port}/"></iframe></foreignObject>
            </svg>"##
        );
        let i = render(doc.as_bytes()).unwrap();
        assert_eq!(opaque(&i.image), 0, "none of them drew anything");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(listener.accept().is_err(), "nothing called");
    }

    #[test]
    fn what_grows_by_powers_is_refused_before_it_grows() {
        let mut laughs = String::from("<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY a \"lol\">");
        for (n, p) in ('b'..='j').zip('a'..) {
            laughs.push_str(&format!("<!ENTITY {n} \"{}\">", format!("&{p};").repeat(10)));
        }
        laughs.push_str("]><svg xmlns=\"http://www.w3.org/2000/svg\"><text>&j;</text></svg>");
        assert!(render(laughs.as_bytes()).err().unwrap().contains("in terms of other text"));

        let mut uses = String::from(r#"<svg xmlns="http://www.w3.org/2000/svg"><defs><g id="a0"><rect width="1" height="1"/></g>"#);
        for n in 1..=9 {
            uses.push_str(&format!("<g id=\"a{n}\">{}</g>", format!("<use href=\"#a{}\"/>", n - 1).repeat(10)));
        }
        uses.push_str("</defs><use href=\"#a9\"/></svg>");
        assert!(render(uses.as_bytes()).err().unwrap().contains("too many parts"));

        let looped = r##"<svg xmlns="http://www.w3.org/2000/svg"><g id="a"><use href="#b"/></g><g id="b"><use href="#a"/></g></svg>"##;
        assert!(render(looped.as_bytes()).err().unwrap().contains("refers to itself"));

        let big = format!("<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY a \"{}\">]><svg xmlns=\"http://www.w3.org/2000/svg\">{}</svg>", "x".repeat(100_000), "<text>&a;</text>".repeat(1000));
        assert!(render(big.as_bytes()).err().unwrap().contains("expand to too much"));
        // As some drawing tools write them, it's fine.
        let plain = "<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY ns \"http://www.w3.org/2000/svg\">]><svg xmlns=\"&ns;\" width=\"2\" height=\"2\"><rect width=\"2\" height=\"2\"/></svg>";
        assert!(render(plain.as_bytes()).is_ok(), "{:?}", render(plain.as_bytes()).err());
    }

    #[test]
    fn too_many_filters_too_many_parts_and_too_big_are_refused() {
        let filters = format!(r#"<svg xmlns="http://www.w3.org/2000/svg"><filter id="f"><feGaussianBlur stdDeviation="9"/></filter>{}</svg>"#, r##"<rect width="9" height="9" filter="url(#f)"/>"##.repeat(100));
        assert!(render(filters.as_bytes()).err().unwrap().contains("filters"));
        let parts = format!(r#"<svg xmlns="http://www.w3.org/2000/svg">{}</svg>"#, "<g/>".repeat(900_000));
        assert!(render(parts.as_bytes()).err().unwrap().contains("too many parts"));
        assert!(render(&vec![b' '; MAX_BYTES + 1]).err().unwrap().contains("MB"));
    }

    #[test]
    fn what_isnt_one_says_so() {
        for bad in [&b"not xml at all"[..], b"<html></html>", b"\xff\xfe\x00", b"<svg", b""] {
            assert!(render(bad).is_err(), "{bad:?}");
        }
        assert!(is_svg("a/B.SVG"));
        assert!(!is_svg("a.svgz"));
    }
}
