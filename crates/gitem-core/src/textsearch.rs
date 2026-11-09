//! Find in files: what ripgrep does, inside gitem. The `ignore` crate walks the folders on
//! every core, honouring `.gitignore`, and `grep-searcher` reads each file with the same
//! literal and SIMD fast paths `rg` has. Results go out as each file is done, so the first
//! show while the rest are still being searched, and a search stops within a file of being
//! told to.
//!
//! At a revision there's no folder to walk, so git hands over the files' contents and the
//! same searcher reads them: what matches is the same at a revision as in the working tree.
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::overrides::{Override, OverrideBuilder};
use ignore::{WalkBuilder, WalkState};

use crate::runner::{GitExe, RunOpts};

/// The most of a line a result keeps. The rest is never shown, and a minified file's one
/// line can be megabytes.
pub const PREVIEW_MAX: usize = 240;
/// How much of the line before the first match a cut preview keeps, so it has some context.
const PREVIEW_BEFORE: usize = 40;
/// Files bigger than this are left out unless the query says otherwise: they're nearly
/// always generated, and one can take longer than all the rest.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
/// More than anyone reads; VS Code stops at the same.
pub const DEFAULT_MAX_MATCHES: usize = 20_000;
/// How often, at most, the caller is told there's more: about a frame.
const NOTIFY_EVERY: Duration = Duration::from_millis(16);
/// Errors kept for the summary. One unreadable folder can give thousands.
const ERRORS_KEPT: usize = 20;

/// What to look for, and where not to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Globs, comma-separated as VS Code has them: `*.rs, src/**`. Empty means everything.
    pub include: String,
    pub exclude: String,
    /// Search what `.gitignore` and the like leave out too.
    pub include_ignored: bool,
    /// In bytes; 0 means no limit.
    pub max_file_size: u64,
    /// Stop once this many have been found; 0 means no limit.
    pub max_matches: usize,
    /// How many files are read at once; 0 means one per core.
    pub threads: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self { text: String::new(), regex: false, case_sensitive: false, whole_word: false, include: String::new(), exclude: String::new(), include_ignored: false, max_file_size: DEFAULT_MAX_FILE_SIZE, max_matches: DEFAULT_MAX_MATCHES, threads: 0 }
    }
}

impl Query {
    /// A query that can match a line break needs the whole file at once, which is slower.
    pub fn is_multi_line(&self) -> bool {
        // `\s` alone doesn't count: with a line at a time, the matcher leaves the break out of it.
        self.text.contains('\n') || (self.regex && self.text.contains("\\n"))
    }
}

/// Where to look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everything under the root, or under one folder of it (repository-relative, `/`-separated).
    WorkingTree { folder: Option<String> },
    /// These files only, repository-relative: the changed ones, or those open in editors.
    Files(Vec<String>),
    /// What the files were at a commit, branch or tag.
    Revision(String),
}

/// Everything a search needs.
pub struct Request {
    pub root: PathBuf,
    pub query: Query,
    pub scope: Scope,
    /// Text to search in place of what's on disk, by repository-relative path: editors with
    /// unsaved changes. Not used at a revision.
    pub overlay: HashMap<String, Arc<str>>,
    /// Needed for a revision only.
    pub git: Option<GitExe>,
}

/// One line with a match on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineMatch {
    /// From 1.
    pub line: u32,
    /// Characters before the first match on the full line, from 0: where an editor puts the
    /// cursor.
    pub column: u32,
    /// The line, or the part of it around the matches, without leading indentation.
    pub preview: String,
    /// Where the matches are in `preview`, in bytes. Empty for a match of nothing, like `^`.
    pub ranges: Vec<Range<u32>>,
    /// Some of the line was cut off before or after the preview.
    pub cut_before: bool,
    pub cut_after: bool,
}

/// One file's matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatches {
    /// Repository-relative, `/`-separated.
    pub path: String,
    pub lines: Vec<LineMatch>,
    /// Matches, not lines: a line can have several.
    pub matches: usize,
    /// It was searched from an editor's unsaved text.
    pub unsaved: bool,
}

/// How a search went, sent last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub files_searched: usize,
    pub files_matched: usize,
    pub matches: usize,
    /// Over `max_file_size`, so not looked at.
    pub skipped_large: usize,
    /// It stopped at `max_matches`: there are more.
    pub limit_hit: bool,
    pub cancelled: bool,
    /// What couldn't be read, the first few of them.
    pub errors: Vec<String>,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Found(FileMatches),
    Done(Summary),
}

/// A search under way. Dropping it stops it.
pub struct Search {
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Search {
    /// Everything that's arrived since last asked. Doesn't wait.
    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }

    /// Waits for the next, for tests and tools. None once the search is over and all's read.
    pub fn next_blocking(&self) -> Option<Event> {
        self.rx.recv().ok()
    }

    /// Asks it to stop. It does within a file, and still sends `Done`.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Every result and the summary, waiting for the end.
    pub fn collect(self) -> (Vec<FileMatches>, Summary) {
        let mut files = Vec::new();
        while let Some(e) = self.next_blocking() {
            match e {
                Event::Found(f) => files.push(f),
                Event::Done(s) => return (files, s),
            }
        }
        (files, Summary { cancelled: true, ..Default::default() })
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The matcher for a query, or why the query isn't one. Cheap: worth calling as someone
/// types, to say a regex is wrong before searching.
pub fn compile(q: &Query) -> Result<RegexMatcher, String> {
    if q.text.is_empty() {
        return Err("Nothing to search for.".into());
    }
    let mut b = RegexMatcherBuilder::new();
    // `^` and `$` are for lines, as in every editor's find.
    b.case_insensitive(!q.case_sensitive).multi_line(true).fixed_strings(!q.regex).word(q.whole_word).crlf(true);
    // `crlf` above lets `$` match before a `\r\n`, and makes the line the unit of matching,
    // which lets the searcher go a line at a time and use its fast paths. A search across
    // lines takes the second back. The order matters: `crlf` sets the line terminator.
    if q.is_multi_line() {
        b.line_terminator(None);
    }
    b.build(&q.text).map_err(|e| e.to_string())
}

/// The include and exclude globs as the walker wants them, or which glob is wrong.
pub fn globs(root: &Path, q: &Query) -> Result<Override, String> {
    let mut b = OverrideBuilder::new(root);
    // Windows and macOS don't care about case in names, so a glob shouldn't either.
    b.case_insensitive(cfg!(any(windows, target_os = "macos"))).map_err(|e| e.to_string())?;
    let add = |b: &mut OverrideBuilder, glob: &str| b.add(glob).map(|_| ()).map_err(|e| format!("{glob}: {e}"));
    for g in split_globs(&q.include) {
        // `src` means the folder too, and everything in it, as in VS Code.
        let g = anchored(&g);
        add(&mut b, &g)?;
        if !g.ends_with("/**") {
            add(&mut b, &format!("{}/**", g.trim_end_matches('/')))?;
        }
    }
    for g in split_globs(&q.exclude) {
        add(&mut b, &format!("!{}", anchored(&g)))?;
    }
    b.build().map_err(|e| e.to_string())
}

/// A glob as VS Code reads one: anywhere in the tree, unless it starts with `./` or `/`,
/// which tie it to the top. Without this, `vendor/**` would only mean a top-level vendor.
fn anchored(g: &str) -> String {
    if let Some(rest) = g.strip_prefix("./") {
        format!("/{rest}")
    } else if g.starts_with('/') || g.starts_with("**") {
        g.to_string()
    } else {
        format!("**/{g}")
    }
}

/// `a, {b,c}/*.rs` → `a`, `{b,c}/*.rs`: commas inside braces belong to the glob.
pub fn split_globs(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0i32);
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|g| g.trim().replace('\\', "/")).filter(|g| !g.is_empty()).collect()
}

/// Starts a search. A query or glob that's wrong is said here, before anything runs.
/// `notify` is called, at most about once a frame, when there's something new to poll.
pub fn start(req: Request, notify: impl Fn() + Send + Sync + 'static) -> Result<Search, String> {
    let matcher = compile(&req.query)?;
    let overrides = globs(&req.root, &req.query)?;
    if matches!(req.scope, Scope::Revision(_)) && req.git.is_none() {
        return Err("Searching a revision needs git.".into());
    }
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared {
        matcher,
        overrides,
        root: req.root.clone(),
        query: req.query.clone(),
        overlay: req.overlay,
        cancel: cancel.clone(),
        limit_hit: AtomicBool::new(false),
        matches: AtomicUsize::new(0),
        files_searched: AtomicUsize::new(0),
        files_matched: AtomicUsize::new(0),
        skipped_large: AtomicUsize::new(0),
        errors: Mutex::new(Vec::new()),
        started: Instant::now(),
        last_notify: AtomicU64::new(0),
        notify: Box::new(notify),
    });
    let (scope, git) = (req.scope, req.git);
    std::thread::Builder::new()
        .name("gitem-search".into())
        .spawn(move || {
            match scope {
                Scope::WorkingTree { folder } => walk(&shared, folder.as_deref(), &tx),
                Scope::Files(list) => files(&shared, list, &tx),
                Scope::Revision(rev) => revision(&shared, git.as_ref().expect("checked"), &rev, &tx),
            }
            let s = &shared;
            let summary = Summary {
                files_searched: s.files_searched.load(Ordering::Relaxed),
                files_matched: s.files_matched.load(Ordering::Relaxed),
                matches: s.matches.load(Ordering::Relaxed).min(if s.query.max_matches == 0 { usize::MAX } else { s.query.max_matches }),
                skipped_large: s.skipped_large.load(Ordering::Relaxed),
                limit_hit: s.limit_hit.load(Ordering::Relaxed),
                cancelled: s.cancel.load(Ordering::Relaxed),
                errors: s.errors.lock().map(|e| e.clone()).unwrap_or_default(),
                elapsed: s.started.elapsed(),
            };
            let _ = tx.send(Event::Done(summary));
            (s.notify)();
        })
        .map_err(|e| e.to_string())?;
    Ok(Search { rx, cancel })
}

/// What every worker thread reads and counts into.
struct Shared {
    matcher: RegexMatcher,
    overrides: Override,
    root: PathBuf,
    query: Query,
    overlay: HashMap<String, Arc<str>>,
    cancel: Arc<AtomicBool>,
    limit_hit: AtomicBool,
    matches: AtomicUsize,
    files_searched: AtomicUsize,
    files_matched: AtomicUsize,
    skipped_large: AtomicUsize,
    errors: Mutex<Vec<String>>,
    started: Instant,
    /// Milliseconds since `started` of the last call to `notify`.
    last_notify: AtomicU64,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || self.limit_hit.load(Ordering::Relaxed)
    }

    /// How many files to read at once.
    fn threads(&self) -> usize {
        match self.query.threads {
            0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
            n => n,
        }
    }

    fn error(&self, e: impl ToString) {
        if let Ok(mut v) = self.errors.lock()
            && v.len() < ERRORS_KEPT
        {
            v.push(e.to_string());
        }
    }

    /// Counts `n` more matches. False once the limit's reached: what's past it isn't kept.
    fn take(&self, n: usize) -> bool {
        let max = self.query.max_matches;
        let before = self.matches.fetch_add(n, Ordering::Relaxed);
        if max != 0 && before + n >= max {
            self.limit_hit.store(true, Ordering::Relaxed);
            return before < max;
        }
        true
    }

    fn send(&self, tx: &Sender<Event>, f: FileMatches) {
        self.files_matched.fetch_add(1, Ordering::Relaxed);
        let _ = tx.send(Event::Found(f));
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.last_notify.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= NOTIFY_EVERY.as_millis() as u64 && self.last_notify.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
            (self.notify)();
        }
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }

    fn searcher(&self) -> Searcher {
        let mut b = SearcherBuilder::new();
        if !self.query.is_multi_line() {
            // The same as the matcher's, which `crlf` set. A lone `\n` still ends a line.
            b.line_terminator(grep_matcher::LineTerminator::crlf());
        }
        b.line_number(true)
            .multi_line(self.query.is_multi_line())
            // A NUL byte means binary, as git and every editor decide. Such a file's matches are
            // dropped, not shown as garbage.
            .binary_detection(BinaryDetection::quit(0))
            // No memory maps: a file cut short while mapped ends the process on Unix, and an
            // editor or build can do that to any file at any time. Reading is as fast for the
            // small files a repository is mostly made of.
            .build()
    }

    /// Searches one file, from the overlay if it's there, and sends what it found.
    fn search(&self, searcher: &mut Searcher, rel: String, path: &Path, size: Option<u64>, tx: &Sender<Event>) {
        if self.stopped() {
            return;
        }
        let overlay = self.overlay.get(&rel).cloned();
        if overlay.is_none() && self.query.max_file_size > 0 && size.is_some_and(|s| s > self.query.max_file_size) {
            self.skipped_large.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.files_searched.fetch_add(1, Ordering::Relaxed);
        let mut sink = Collect { shared: self, lines: Vec::new(), matches: 0, binary: false };
        let result = match &overlay {
            Some(text) => searcher.search_slice(&self.matcher, text.as_bytes(), &mut sink),
            None => searcher.search_path(&self.matcher, path, &mut sink),
        };
        if let Err(e) = result {
            self.error(format!("{rel}: {e}"));
            return;
        }
        if sink.binary || sink.lines.is_empty() {
            return;
        }
        self.send(tx, FileMatches { path: rel, lines: sink.lines, matches: sink.matches, unsaved: overlay.is_some() });
    }

    /// Searches a file's contents, as git has them at a revision.
    fn search_bytes(&self, searcher: &mut Searcher, rel: String, bytes: &[u8], tx: &Sender<Event>) {
        if self.stopped() {
            return;
        }
        self.files_searched.fetch_add(1, Ordering::Relaxed);
        let mut sink = Collect { shared: self, lines: Vec::new(), matches: 0, binary: false };
        if let Err(e) = searcher.search_slice(&self.matcher, bytes, &mut sink) {
            return self.error(format!("{rel}: {e}"));
        }
        if !sink.binary && !sink.lines.is_empty() {
            self.send(tx, FileMatches { path: rel, lines: sink.lines, matches: sink.matches, unsaved: false });
        }
    }
}

/// One file's matches as the searcher finds them.
struct Collect<'a> {
    shared: &'a Shared,
    lines: Vec<LineMatch>,
    matches: usize,
    binary: bool,
}

impl Sink for Collect<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _: &Searcher, m: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.shared.stopped() {
            return Ok(false);
        }
        let bytes = m.bytes();
        let mut found: Vec<Range<usize>> = Vec::new();
        let _ = self.shared.matcher.find_iter(bytes, |r| {
            if !r.is_empty() {
                found.push(r.start()..r.end());
            }
            true
        });
        // A match of nothing, like `^`, still says the line matched.
        let n = found.len().max(1);
        if !self.shared.take(n) {
            return Ok(false);
        }
        self.matches += n;
        let first = m.line_number().unwrap_or(1) as u32;
        let mut at = 0;
        for (i, line) in m.lines().enumerate() {
            let content = trim_line_end(line);
            let ranges: Vec<Range<usize>> = found.iter().filter(|r| r.start < at + content.len().max(1) && r.end > at).map(|r| r.start.saturating_sub(at).min(content.len())..(r.end - at).min(content.len())).collect();
            if i == 0 || !ranges.is_empty() {
                self.lines.push(line_match(first + i as u32, content, &ranges));
            }
            at += line.len();
        }
        Ok(!self.shared.limit_hit.load(Ordering::Relaxed))
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, Self::Error> {
        self.binary = true;
        Ok(false)
    }
}

fn trim_line_end(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A line's preview: indentation dropped, cut to `PREVIEW_MAX` around the first match,
/// and any bytes that aren't UTF-8 replaced, with the matches' places moved to suit.
pub fn line_match(line: u32, content: &[u8], ranges: &[Range<usize>]) -> LineMatch {
    let first = ranges.first().map(|r| r.start).unwrap_or(0);
    let column = String::from_utf8_lossy(&content[..first.min(content.len())]).chars().count() as u32;
    let indent = content.iter().take_while(|b| matches!(b, b' ' | b'\t')).count().min(first);
    let (mut start, mut end) = (indent, content.len());
    if end - start > PREVIEW_MAX {
        start = start.max(first.saturating_sub(PREVIEW_BEFORE));
        end = (start + PREVIEW_MAX).min(content.len());
    }
    // Not in the middle of a character.
    while start < content.len() && start > indent && (content[start] & 0xC0) == 0x80 {
        start += 1;
    }
    while end < content.len() && end > start && (content[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    // Each stretch between match edges is converted on its own, so a replaced byte can't
    // move a highlight.
    let mut cuts: Vec<usize> = vec![start, end];
    for r in ranges {
        cuts.extend([r.start.clamp(start, end), r.end.clamp(start, end)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut preview = String::with_capacity(end - start);
    let mut at: HashMap<usize, u32> = HashMap::new();
    for w in cuts.windows(2) {
        at.insert(w[0], preview.len() as u32);
        preview.push_str(&String::from_utf8_lossy(&content[w[0]..w[1]]));
    }
    at.insert(end, preview.len() as u32);
    let ranges = ranges.iter().map(|r| (r.start.clamp(start, end), r.end.clamp(start, end))).filter(|(s, e)| e > s).map(|(s, e)| at[&s]..at[&e]).collect();
    LineMatch { line, column, preview, ranges, cut_before: start > indent, cut_after: end < content.len() }
}

/// The working tree, or a folder of it, on every core.
fn walk(shared: &Arc<Shared>, folder: Option<&str>, tx: &Sender<Event>) {
    let start = match folder {
        Some(f) => shared.root.join(f),
        None => shared.root.clone(),
    };
    let honour = !shared.query.include_ignored;
    let mut b = WalkBuilder::new(&start);
    b.threads(shared.query.threads)
        .hidden(false)
        .git_ignore(honour)
        .git_exclude(honour)
        .git_global(honour)
        .ignore(honour)
        .parents(honour)
        .follow_links(false)
        .overrides(shared.overrides.clone())
        // Never git's own folder, nor a submodule's or worktree's `.git` file.
        .filter_entry(|e| e.file_name() != ".git");
    b.build_parallel().run(|| {
        let shared = shared.clone();
        let tx = tx.clone();
        let mut searcher = shared.searcher();
        Box::new(move |entry| {
            if shared.stopped() {
                return WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    shared.error(e);
                    return WalkState::Continue;
                }
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            let size = entry.metadata().ok().map(|m| m.len());
            let rel = shared.relative(entry.path());
            shared.search(&mut searcher, rel, entry.path(), size, &tx);
            WalkState::Continue
        })
    });
    // An editor's unsaved file that isn't on disk yet.
    let mut searcher = shared.searcher();
    for rel in shared.overlay.keys() {
        let path = shared.root.join(rel);
        let inside = folder.is_none_or(|f| rel.starts_with(&format!("{}/", f.trim_end_matches('/'))));
        if inside && !path.exists() && allowed(shared, rel, false) {
            shared.search(&mut searcher, rel.clone(), &path, None, tx);
        }
    }
}

/// Whether the include and exclude globs let a path through.
fn allowed(shared: &Shared, rel: &str, is_dir: bool) -> bool {
    !shared.overrides.matched(shared.root.join(rel), is_dir).is_ignore()
}

/// A list of files, on every core.
fn files(shared: &Arc<Shared>, list: Vec<String>, tx: &Sender<Event>) {
    let list: Vec<String> = list.into_iter().filter(|rel| allowed(shared, rel, false)).collect();
    let next = AtomicUsize::new(0);
    let threads = shared.threads().min(list.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                let mut searcher = shared.searcher();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(rel) = list.get(i) else { break };
                    if shared.stopped() {
                        break;
                    }
                    let path = shared.root.join(rel);
                    let meta = std::fs::metadata(&path).ok();
                    // Gone since it was listed, and not open either.
                    if meta.as_ref().is_none_or(|m| !m.is_file()) && !shared.overlay.contains_key(rel) {
                        continue;
                    }
                    shared.search(&mut searcher, rel.clone(), &path, meta.map(|m| m.len()), tx);
                }
            });
        }
    });
}

/// A file at a revision, as `git ls-tree --long` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Blob {
    oid: String,
    size: u64,
    path: String,
}

/// `mode SP type SP oid SP+ size TAB path NUL`, files only: not links, nor submodules.
fn parse_ls_tree_long(out: &[u8]) -> Vec<Blob> {
    out.split(|b| *b == 0)
        .filter_map(|rec| {
            let rec = std::str::from_utf8(rec).ok()?;
            let (meta, path) = rec.split_once('\t')?;
            let mut f = meta.split_whitespace();
            let (mode, kind, oid, size) = (f.next()?, f.next()?, f.next()?, f.next()?);
            (kind == "blob" && mode != "120000").then(|| Blob { oid: oid.to_string(), size: size.parse().unwrap_or(0), path: path.to_string() })
        })
        .collect()
}

/// For plain text, `git grep` names the files at a revision that could have it, inside one
/// process and faster than they can be read out of it, and they're searched as it names them.
/// Never fewer than the searcher would find: no `-w`, no `-I`, and only for ASCII, where git
/// ignores case as the searcher does. None for a regular expression, which is the searcher's
/// syntax, not git's: then every file is read.
fn candidates(shared: &Shared, git: &GitExe, rev: &str) -> Option<crate::runner::Streaming> {
    let q = &shared.query;
    if q.regex || q.text.is_empty() || !q.text.is_ascii() || q.is_multi_line() {
        return None;
    }
    let mut a: Vec<String> = ["grep", "-l", "-z", "--no-color", "--full-name", "-F"].map(String::from).to_vec();
    if !q.case_sensitive {
        a.push("-i".into());
    }
    a.extend(["-e".to_string(), q.text.clone(), rev.to_string(), "--".to_string()]);
    git.stream(&shared.root, &a).ok()
}

/// At a revision there's no folder to walk: git lists the files and a `cat-file` on each core
/// brings back their contents, which the same searcher reads as it would the working tree's.
/// What matches is the same at a revision as on disk.
fn revision(shared: &Arc<Shared>, git: &GitExe, rev: &str, tx: &Sender<Event>) {
    let rev = match crate::repo::rev_arg(rev) {
        Ok(r) => r.to_string(),
        Err(e) => return shared.error(e),
    };
    // Started first, to look while the files are listed. Until it's read, it waits.
    let named = candidates(shared, git, &rev);
    let opts = RunOpts { cancel: Some(shared.cancel.clone()), ..Default::default() };
    let args: Vec<String> = ["ls-tree", "-r", "-z", "--long", "--full-tree", rev.as_str()].map(String::from).to_vec();
    let listed = match git.run_opts(Some(&shared.root), &args, &opts, &mut |_| {}) {
        Ok(o) if o.ok() => o,
        Ok(o) => return shared.error(format!("{rev}: {}", o.stderr.trim())),
        Err(e) => return shared.error(e),
    };
    let max = shared.query.max_file_size;
    let mut blobs = Vec::new();
    for blob in parse_ls_tree_long(&listed.stdout) {
        if !allowed(shared, &blob.path, false) {
            continue;
        }
        if max > 0 && blob.size > max {
            shared.skipped_large.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        blobs.push(blob);
    }
    let (send, work) = channel::<usize>();
    let work = Mutex::new(work);
    std::thread::scope(|s| {
        // One `cat-file` per thread, asked for one file after another: starting git costs more
        // than reading a file, on Windows most of all.
        for _ in 0..shared.threads().min(blobs.len().div_ceil(16).max(1)) {
            s.spawn(|| {
                let mut git = match git.cat_file(&shared.root) {
                    Ok(g) => g,
                    Err(e) => return shared.error(e),
                };
                let mut searcher = shared.searcher();
                let mut bytes = Vec::new();
                loop {
                    let next = work.lock().ok().and_then(|w| w.recv().ok());
                    let Some(blob) = next.and_then(|i| blobs.get(i)) else { break };
                    if shared.stopped() {
                        break;
                    }
                    match git.read(&blob.oid, &mut bytes) {
                        Ok(true) => shared.search_bytes(&mut searcher, blob.path.clone(), &bytes, tx),
                        Ok(false) => shared.error(format!("{}: missing from the repository", blob.path)),
                        Err(e) => return shared.error(format!("{}: {e}", blob.path)),
                    }
                }
            });
        }
        let mut sent = vec![false; blobs.len()];
        if let Some(mut named) = named {
            use std::io::BufRead;
            let by_path: HashMap<&str, usize> = blobs.iter().enumerate().map(|(i, b)| (b.path.as_str(), i)).collect();
            let prefix = format!("{rev}:");
            let mut path = Vec::new();
            while !shared.stopped() && named.stdout.read_until(0, &mut path).is_ok_and(|n| n > 0) {
                let p = String::from_utf8_lossy(path.strip_suffix(&[0]).unwrap_or(&path)).into_owned();
                if let Some(&i) = by_path.get(p.strip_prefix(&prefix).unwrap_or(&p)) {
                    sent[i] = true;
                    let _ = send.send(i);
                }
                path.clear();
            }
            // Zero is something found and one nothing. Anything else, and the rest are read too.
            if !shared.stopped() && named.finish(&[0, 1]) {
                let passed = sent.iter().filter(|s| !**s).count();
                shared.files_searched.fetch_add(passed, Ordering::Relaxed);
                sent.fill(true);
            }
        }
        for (i, done) in sent.iter().enumerate() {
            if !done && !shared.stopped() {
                let _ = send.send(i);
            }
        }
        drop(send);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_tree_lists_files_with_sizes_and_leaves_out_links_and_submodules() {
        let out = b"100644 blob aaa      12\tsrc/a b.rs\x00100755 blob bbb       3\trun.sh\x00120000 blob ccc       5\tlink\x00160000 commit ddd       -\tsub\x00";
        let blobs = parse_ls_tree_long(out);
        assert_eq!(blobs, vec![Blob { oid: "aaa".into(), size: 12, path: "src/a b.rs".into() }, Blob { oid: "bbb".into(), size: 3, path: "run.sh".into() }]);
    }

    fn preview(text: &str, needle: &str) -> LineMatch {
        let ranges: Vec<Range<usize>> = text.match_indices(needle).map(|(i, m)| i..i + m.len()).collect();
        line_match(1, text.as_bytes(), &ranges)
    }

    fn shown(m: &LineMatch) -> Vec<&str> {
        m.ranges.iter().map(|r| &m.preview[r.start as usize..r.end as usize]).collect()
    }

    #[test]
    fn a_preview_drops_indentation_and_keeps_its_highlights() {
        let m = preview("\t    let x = foo(foo);", "foo");
        assert_eq!(m.preview, "let x = foo(foo);");
        assert_eq!(shown(&m), ["foo", "foo"]);
        assert_eq!(m.column, 13);
        assert!(!m.cut_before && !m.cut_after);
    }

    #[test]
    fn a_long_line_is_cut_around_its_first_match() {
        let line = format!("{}needle{}", "a".repeat(5000), "b".repeat(5000));
        let m = preview(&line, "needle");
        assert!(m.preview.len() <= PREVIEW_MAX);
        assert_eq!(shown(&m), ["needle"]);
        assert!(m.cut_before && m.cut_after);
        assert_eq!(m.column, 5000);
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let line = format!("{}needle{}", "é".repeat(3000), "ü".repeat(3000));
        let m = preview(&line, "needle");
        assert_eq!(shown(&m), ["needle"]);
        assert_eq!(m.column, 3000, "characters, not bytes");
    }

    #[test]
    fn bytes_that_arent_utf8_dont_move_a_highlight() {
        let mut line = b"\xff\xfe x ".to_vec();
        line.extend_from_slice(b"hit \xc3 hit");
        let ranges = vec![5..8, 11..14];
        let m = line_match(1, &line, &ranges);
        assert_eq!(shown(&m), ["hit", "hit"]);
    }

    #[test]
    fn globs_split_on_commas_outside_braces() {
        assert_eq!(split_globs(" *.rs, ./src/** ,{a,b}/*.md,, "), ["*.rs", "./src/**", "{a,b}/*.md"]);
        assert_eq!([anchored("*.rs"), anchored("./src"), anchored("/src"), anchored("**/x")], ["**/*.rs", "/src", "/src", "**/x"]);
        assert!(split_globs("").is_empty());
    }

    #[test]
    fn a_wrong_regex_is_said_before_searching() {
        let q = Query { text: "(".into(), regex: true, ..Default::default() };
        assert!(compile(&q).is_err());
        let q = Query { text: "(".into(), ..Default::default() };
        assert!(compile(&q).is_ok(), "as text, it's just a bracket");
    }
}
