//! Drawing an SVG, which in a repository is untrusted input: it can claim any size, nest without
//! end, expand itself like a zip bomb, or point at files and addresses. So it's looked over before
//! the renderer sees it, the picture has a fixed largest size, it runs on a thread with a stack of
//! its own, and nothing the file names is ever read: no pictures, no fonts, no styles, and scripts
//! aren't run by anything here. The only fonts are this computer's and the one that came with gitem,
//! found on the worker thread the first time one's needed.
//!
//! Waiting is bounded, and the work isn't. A thread can't be stopped from outside, and the renderer
//! has nowhere to ask whether to go on, so a file that's slow within these limits is given up on and
//! goes on running until it ends. At most `MAX_WORKERS` run, so opening it again and again doesn't
//! add threads: one that can't get a turn says so.
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
/// How long a view waits for a picture, and for a turn to draw one.
const TIME_LIMIT: Duration = Duration::from_secs(10);
/// Drawing at once, including any that have been given up on and are still going.
const MAX_WORKERS: usize = 2;
const TOO_SLOW: &str = "It took too long to draw, so gitem stopped waiting. Source still shows it.";
const NO_TURN: &str = "Other SVGs are taking too long to draw, so this one wasn't started. Source still shows it.";

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

/// Whether this can be given to the renderer, and if not, why.
fn look_over(text: &str) -> Result<(), String> {
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
    Ok(())
}

/// This computer's fonts and the one gitem came with, and that one's id. Once, whichever worker
/// gets here first: it reads a folder of files.
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

fn draw(text: &str, stop: &AtomicBool) -> Result<Drawn, String> {
    let (db, own) = fonts();
    draw_with(text, stop, db.clone(), *own)
}

/// `stop` is looked at between the steps, as nothing inside one asks.
fn draw_with(text: &str, stop: &AtomicBool, db: Arc<usvg::fontdb::Database>, own: Option<usvg::fontdb::ID>) -> Result<Drawn, String> {
    let given_up = || if stop.load(Ordering::Relaxed) { Err(TOO_SLOW.to_string()) } else { Ok(()) };
    look_over(text)?;
    given_up()?;
    let missing = Arc::new(AtomicBool::new(false));
    let tree = usvg::Tree::from_str(text, &options(db, own, missing.clone())).map_err(|e| e.to_string())?;
    given_up()?;
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

/// Draws one, off whichever thread this is called on. Not from the UI thread: it takes a while.
pub fn render(bytes: &[u8]) -> Result<Drawn, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("It's over {} MB.", MAX_BYTES / (1024 * 1024)));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "It isn't UTF-8.".to_string())?.to_string();
    static WORKERS: LazyLock<Arc<Slots>> = LazyLock::new(|| Arc::new(Slots::new(MAX_WORKERS)));
    let slow = SLOW_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|s| text.contains(s.marker)).map(|s| (s.limit, s.release.clone()));
    let limit = slow.as_ref().map_or(TIME_LIMIT, |(l, _)| *l);
    with_limit(&WORKERS, limit, move |stop| {
        if let Some((_, release)) = &slow {
            while !release.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        draw(&text, stop)
    })
}

/// Places for workers to run in, one held for as long as its thread lasts.
struct Slots {
    max: usize,
    taken: Mutex<usize>,
    freed: Condvar,
}

impl Slots {
    fn new(max: usize) -> Self {
        Self { max, taken: Mutex::new(0), freed: Condvar::new() }
    }

    /// One, if one comes free in this time.
    fn take(self: &Arc<Self>, patience: Duration) -> Option<Slot> {
        let until = Instant::now() + patience;
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        while *taken >= self.max {
            let left = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            taken = self.freed.wait_timeout(taken, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        *taken += 1;
        Some(Slot(self.clone()))
    }
}

/// Given back when its thread ends, however that happens.
struct Slot(Arc<Slots>);

impl Drop for Slot {
    fn drop(&mut self) {
        *self.0.taken.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.freed.notify_one();
    }
}

/// Runs `work` on a worker and waits for it up to `limit`, after waiting up to as long for a turn.
/// What's given up on is told through its flag, and holds its place until it ends.
fn with_limit<T: Send + 'static>(slots: &Arc<Slots>, limit: Duration, work: impl FnOnce(&AtomicBool) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let slot = slots.take(limit).ok_or(NO_TURN)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let flag = stop.clone();
    std::thread::Builder::new()
        .name("svg".to_string())
        .stack_size(STACK)
        .spawn(move || {
            let _slot = slot;
            let _ = tx.send(work(&flag));
        })
        .map_err(|e| e.to_string())?;
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            stop.store(true, Ordering::Relaxed);
            Err(TOO_SLOW.to_string())
        }
        // A panic in the renderer is one picture that doesn't show.
        Err(RecvTimeoutError::Disconnected) => Err("The renderer couldn't draw it.".to_string()),
    }
}

struct SlowForTest {
    marker: &'static str,
    limit: Duration,
    release: Arc<AtomicBool>,
}

static SLOW_FOR_TEST: Mutex<Option<SlowForTest>> = Mutex::new(None);

/// For a test of what a view does: a file with this in it draws only once the flag returned is set,
/// and is waited for this long. Nothing else takes any notice.
pub fn slow_for_test(marker: &'static str, limit: Duration) -> Arc<AtomicBool> {
    let release = Arc::new(AtomicBool::new(false));
    *SLOW_FOR_TEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(SlowForTest { marker, limit, release: release.clone() });
    release
}

pub fn is_svg(path: &str) -> bool {
    path.to_lowercase().ends_with(".svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
        let stop = AtomicBool::new(false);
        let d = draw_with(&text_in(r#"font-family="Ubuntu""#), &stop, Arc::new(usvg::fontdb::Database::new()), None).unwrap();
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

    /// Within the limits, and as awkward as it can be: it ends, at the picture's size or told as
    /// too slow, and never larger.
    #[test]
    fn text_within_the_limits_stays_inside_them() {
        let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="900" height="900">{}</svg>"#, r#"<text x="1" y="50" font-size="40">Some words</text>"#.repeat(2_000));
        let t = Instant::now();
        match render(doc.as_bytes()) {
            Ok(d) => assert!(d.image.size[0] <= 2048 && opaque(&d.image) > 0),
            Err(e) => assert_eq!(e, TOO_SLOW),
        }
        eprintln!("2,000 text elements: {:?}", t.elapsed());
        for huge in [r#"font-size="100000000""#, r#"font-size="1" letter-spacing="1e30""#, r#"font-size="1e30""#] {
            let doc = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="90" height="90"><text x="1" y="50" {huge}>Words</text></svg>"#);
            if let Ok(d) = render(doc.as_bytes()) {
                assert!(d.image.size[0] <= 2048, "{huge}");
            }
        }
    }

    // ---- the time limit, with a slow render that's injected rather than timed

    const SHORT: Duration = Duration::from_millis(40);

    /// Work that goes on until it's let go, as a slow render does, and says it's begun.
    fn stuck(begun: &Arc<AtomicUsize>, let_go: &Arc<AtomicBool>) -> impl FnOnce(&AtomicBool) -> Result<u32, String> + Send + 'static {
        let (begun, let_go) = (begun.clone(), let_go.clone());
        move |_| {
            begun.fetch_add(1, Ordering::SeqCst);
            while !let_go.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(1)
        }
    }

    fn eventually(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(30), "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_slow_one_is_given_up_on_at_the_limit_and_told_to_stop_where_it_can() {
        let slots = Arc::new(Slots::new(2));
        let seen = Arc::new(AtomicBool::new(false));
        let (told, let_go) = (seen.clone(), Arc::new(AtomicBool::new(false)));
        let release = let_go.clone();
        let got: Result<u32, String> = with_limit(&slots, SHORT, move |stop| {
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            told.store(stop.load(Ordering::SeqCst), Ordering::SeqCst);
            Ok(1)
        });
        assert_eq!(got.err().as_deref(), Some(TOO_SLOW));
        let_go.store(true, Ordering::SeqCst);
        eventually("its place back", || *slots.taken.lock().unwrap() == 0);
        assert!(seen.load(Ordering::SeqCst), "its flag was set");
    }

    /// The view asks again each time it's opened. Threads stay at the limit, later asks get an
    /// answer in the same time, and once the slow ones end it all works again.
    #[test]
    fn opening_a_slow_one_again_and_again_doesnt_add_threads() {
        let slots = Arc::new(Slots::new(2));
        let (begun, let_go) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let mut answers = Vec::new();
        for _ in 0..12 {
            answers.push(with_limit(&slots, SHORT, stuck(&begun, &let_go)).err().unwrap());
        }
        assert_eq!(answers[..2], [TOO_SLOW, TOO_SLOW]);
        assert!(answers[2..].iter().all(|a| a == NO_TURN), "{answers:?}");
        assert_eq!(begun.load(Ordering::SeqCst), 2, "no more than two ever started");
        assert_eq!(*slots.taken.lock().unwrap(), 2);
        let_go.store(true, Ordering::SeqCst);
        eventually("their places back", || *slots.taken.lock().unwrap() == 0);
        assert_eq!(with_limit(&slots, Duration::from_secs(30), |_| Ok(7)), Ok(7));
    }

    #[test]
    fn a_turn_coming_free_in_time_is_taken_and_a_panic_gives_its_place_back() {
        let slots = Arc::new(Slots::new(1));
        let (begun, let_go) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        assert!(with_limit(&slots, SHORT, stuck(&begun, &let_go)).is_err());
        let (waiter, go) = (slots.clone(), let_go.clone());
        let later = std::thread::spawn(move || with_limit(&waiter, Duration::from_secs(30), |_| Ok(2)));
        go.store(true, Ordering::SeqCst);
        assert_eq!(later.join().unwrap(), Ok(2));
        eventually("its place back", || *slots.taken.lock().unwrap() == 0);
        let got: Result<u32, String> = with_limit(&slots, Duration::from_secs(30), |_| panic!("deliberately, in a test"));
        assert!(got.err().unwrap().contains("couldn't draw"));
        eventually("its place back after a panic", || *slots.taken.lock().unwrap() == 0);
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
