//! Drawing an SVG, which in a repository is untrusted input: it can claim any size, nest without
//! end, expand itself like a zip bomb, or point at files and addresses. So it's looked over before
//! the renderer sees it, the picture has a fixed largest size, it runs on one thread at a time with
//! a stack of its own, and nothing outside the file is ever read: no pictures, no fonts, no styles,
//! and scripts aren't run by anything here.
//!
//! Text isn't drawn: the renderer's text support is a feature left off here, which needs fonts and
//! crates this was built without. Whoever's looking is told, and Source shows it.
//!
//! Time isn't bounded. A thread can't be stopped from outside, so a file that's slow within these
//! limits keeps this one thread busy, and later ones wait for it. The app itself carries on.
use std::sync::Mutex;

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

/// How many elements this comes to with each `use` written out. `None` if it never ends.
fn elements_under<'a, 'i>(n: roxmltree::Node<'a, 'i>, ids: &std::collections::HashMap<&'a str, roxmltree::Node<'a, 'i>>, known: &mut std::collections::HashMap<roxmltree::NodeId, u64>, walking: &mut Vec<roxmltree::NodeId>) -> Option<u64> {
    if let Some(c) = known.get(&n.id()) {
        return Some(*c);
    }
    if walking.contains(&n.id()) {
        return None;
    }
    walking.push(n.id());
    let mut count = 1u64;
    for c in n.children().filter(|c| c.is_element()) {
        count = count.saturating_add(elements_under(c, ids, known, walking)?);
    }
    if n.tag_name().name() == "use"
        && let Some(target) = use_target(&n).and_then(|t| ids.get(t))
    {
        count = count.saturating_add(elements_under(*target, ids, known, walking)?);
    }
    walking.pop();
    known.insert(n.id(), count);
    Some(count)
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
    match elements_under(doc.root_element(), &ids, &mut known, &mut Vec::new()) {
        None => Err("It refers to itself.".to_string()),
        Some(n) if n > MAX_ELEMENTS => Err("It has too many parts, once what it repeats is counted.".to_string()),
        Some(_) => Ok(()),
    }
}

fn options() -> usvg::Options<'static> {
    usvg::Options {
        resources_dir: None,
        // A path, an address or embedded data: nothing's fetched, opened or decoded.
        image_href_resolver: usvg::ImageHrefResolver { resolve_data: Box::new(|_, _, _| None), resolve_string: Box::new(|_, _| None) },
        ..Default::default()
    }
}

/// A picture, and whether this file has text that isn't in it.
pub struct Drawn {
    pub image: egui::ColorImage,
    pub no_text: bool,
}

fn draw(text: &str) -> Result<Drawn, String> {
    look_over(text)?;
    let tree = usvg::Tree::from_str(text, &options()).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return Err("It says its size is nothing.".to_string());
    }
    let scale = (MAX_SIDE / w).min(MAX_SIDE / h).min(MAX_ENLARGE);
    let (pw, ph) = (((w * scale).ceil() as u32).clamp(1, MAX_SIDE as u32), ((h * scale).ceil() as u32).clamp(1, MAX_SIDE as u32));
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or("It can't be drawn at that size.")?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(Drawn { image: egui::ColorImage::from_rgba_premultiplied([pw as usize, ph as usize], pixmap.data()), no_text: text.contains("<text") || text.contains(":text") })
}

/// Draws one, off whichever thread this is called on. Not from the UI thread: it takes a while.
pub fn render(bytes: &[u8]) -> Result<Drawn, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("It's over {} MB.", MAX_BYTES / (1024 * 1024)));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "It isn't UTF-8.".to_string())?.to_string();
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let worker = std::thread::Builder::new().name("svg".to_string()).stack_size(STACK).spawn(move || draw(&text)).map_err(|e| e.to_string())?;
    // A panic in the renderer is one picture that doesn't show.
    worker.join().unwrap_or_else(|_| Err("The renderer couldn't draw it.".to_string()))
}

pub fn is_svg(path: &str) -> bool {
    path.to_lowercase().ends_with(".svg")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/></svg>"#;

    fn opaque(i: &egui::ColorImage) -> usize {
        i.pixels.iter().filter(|p| p.a() > 0).count()
    }

    #[test]
    fn an_ordinary_one_is_drawn_and_a_small_one_enlarged_a_little() {
        let d = render(RED.as_bytes()).unwrap();
        assert_eq!(d.image.size, [40, 20], "at twice its size");
        assert_eq!(d.image.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert!(!d.no_text);
    }

    /// What's left out is said, rather than a picture quietly missing its labels.
    #[test]
    fn text_isnt_drawn_and_that_is_said() {
        let d = render(br#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30"><text x="2" y="20" font-size="16">Words</text></svg>"#).unwrap();
        assert_eq!(opaque(&d.image), 0);
        assert!(d.no_text);
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
