//! What the graph's layout holds in memory on a wide history, by counting what's allocated. The
//! shape is the one in the notes for PF-18: 65,258 commits, with 1,261 unmerged heads listed first,
//! so that up to 1,261 lanes run side by side. It's generated, not a real repository, and it only
//! measures the layout, not the whole app.
gitem_testenv::isolate!();

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gitem_core::graph;
use gitem_core::model::Commit;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call goes straight on to the system's allocator, with what it was given.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// Tests in this binary share the counters, so they take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn wide_history() -> Vec<Commit> {
    const COMMITS: usize = 65_258;
    const HEADS: usize = 1_261;
    // Each head comes off the main line further down, so the lanes close one at a time.
    const STEP: usize = 17;
    let name = |i: usize| format!("{i:040x}");
    let mut v = Vec::with_capacity(COMMITS);
    for h in 0..HEADS {
        v.push(Commit { sha: format!("h{h:039x}"), parents: vec![name(h * STEP)], ..Default::default() });
    }
    for i in 0..COMMITS - HEADS {
        let parents = if i + 1 < COMMITS - HEADS { vec![name(i + 1)] } else { Vec::new() };
        v.push(Commit { sha: name(i), parents, ..Default::default() });
    }
    v
}

fn measured<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    let before = LIVE.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let out = f();
    (out, LIVE.load(Ordering::Relaxed) - before, PEAK.load(Ordering::Relaxed) - before)
}

const MB: f64 = 1024.0 * 1024.0;

#[test]
fn wide_history_row_storage_is_small_and_says_how_small() {
    let _turn = ONE_AT_A_TIME.lock().unwrap();
    let commits = wide_history();
    let (rows, row_bytes, row_peak) = measured(|| graph::layout_rows(&commits));
    let widest = (0..rows.len()).map(|i| rows.row(i).width).max().unwrap_or(0);
    let edges: usize = (0..rows.len()).map(|i| rows.top(i).len() + rows.bottom(i).len()).sum();
    let (full, full_bytes, _) = measured(|| graph::layout(&commits));
    eprintln!("{} commits, {widest} lanes at the widest, {edges} edges drawn in all", commits.len());
    eprintln!("every edge stored per row: {:.1} MB", full_bytes as f64 / MB);
    eprintln!("lane changes stored: {:.1} MB, {:.1} MB at its peak while laying out", row_bytes as f64 / MB, row_peak as f64 / MB);
    assert_eq!(widest as usize, full.iter().map(|r| r.width as usize).max().unwrap());
    assert!(widest >= 1_200, "the shape this is about: {widest}");
    assert_eq!(edges, full.iter().map(|r| r.top.len() + r.bottom.len()).sum::<usize>());
    assert!(full_bytes > 100 * 1024 * 1024, "what this was about: {full_bytes}");
    // Far from it, whatever an allocator's rounding does to these.
    assert!(row_bytes < full_bytes / 10, "{row_bytes} of {full_bytes}");
    drop(rows);
    drop(full);
}
