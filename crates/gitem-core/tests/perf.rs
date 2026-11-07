//! Parsing and layout must stay roughly linear in the size of a repository, which is
//! what keeps a large one usable. Git's output is generated rather than run, so this
//! measures only gitem's code.
//!
//! Each check has two bounds. Quadrupling the input must not take more than about
//! nine times as long, which stays true on a slow or busy machine and fails on anything
//! quadratic. And the whole run has a ceiling, roughly twenty times what it takes
//! today in a debug build, which catches a large constant-factor slowdown.
gitem_testenv::isolate!();

use std::hint::black_box;
use std::time::{Duration, Instant};

use gitem_core::{diff, graph, parse};

fn best_of_3(f: impl Fn()) -> Duration {
    (0..3)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap()
}

/// Times below this are mostly noise, so a ratio against them means little.
const SMALLEST_BASELINE: Duration = Duration::from_millis(2);
const MAX_RATIO: f64 = 9.0;

/// Err says which bound failed. Separate from the assert so a test can show that a
/// quadratic function really does fail it.
fn scaling_problem<T>(n: usize, ceiling: Duration, make: impl Fn(usize) -> T, run: impl Fn(&T)) -> Result<(Duration, Duration), String> {
    let (small, large) = (make(n), make(n * 4));
    let attempt = || {
        let t_small = best_of_3(|| run(&small));
        let t_large = best_of_3(|| run(&large));
        let ratio = t_large.as_secs_f64() / t_small.max(SMALLEST_BASELINE).as_secs_f64();
        if ratio > MAX_RATIO {
            return Err(format!("{n} items took {t_small:?} and {} took {t_large:?}, {ratio:.1} times as long", n * 4));
        }
        if t_large > ceiling {
            return Err(format!("{} items took {t_large:?}, over the ceiling of {ceiling:?}", n * 4));
        }
        Ok((t_small, t_large))
    };
    // A busy moment can spoil one attempt. A real slowdown spoils them all.
    attempt().or_else(|_| attempt()).or_else(|_| attempt())
}

fn assert_scales(what: &str, n: usize, ceiling_ms: u64, make: impl Fn(usize) -> String, run: impl Fn(&str)) {
    let run = |s: &String| run(s);
    match scaling_problem(n, Duration::from_millis(ceiling_ms), make, run) {
        Ok((small, large)) => eprintln!("{what}: {n} in {small:?}, {} in {large:?}", n * 4),
        Err(e) => panic!("{what}: {e}"),
    }
}

/// A check that can't fail proves nothing, so it gets seen failing.
#[test]
fn the_scaling_check_catches_quadratic_work() {
    let quadratic = |v: &Vec<u64>| {
        let mut total = 0u64;
        for a in v {
            for b in v {
                total = total.wrapping_add(black_box(a ^ b));
            }
        }
        black_box(total);
    };
    let problem = scaling_problem(6_000, Duration::from_secs(600), |n| (0..n as u64).collect::<Vec<u64>>(), quadratic);
    assert!(problem.is_err(), "took {problem:?}");
    let linear = |v: &Vec<u64>| {
        black_box(v.iter().fold(0u64, |t, x| t.wrapping_add(black_box(*x))));
    };
    let fine = scaling_problem(8_000_000, Duration::from_secs(600), |n| (0..n as u64).collect::<Vec<u64>>(), linear);
    assert!(fine.is_ok(), "{fine:?}");
}

#[test]
fn the_ceiling_catches_a_slow_constant() {
    let slow = |_: &()| std::thread::sleep(Duration::from_millis(30));
    assert!(scaling_problem(1, Duration::from_millis(5), |_| (), slow).is_err());
}

fn log_output(n: usize) -> String {
    let mut s = String::with_capacity(n * 160);
    for i in 0..n {
        let parent = if i + 1 < n { format!("{:040x}", i + 1) } else { String::new() };
        let refs = if i % 500 == 0 { format!("tag: v{i}, origin/b{i}") } else { String::new() };
        s.push_str(&format!("{i:040x}\0{parent}\0Ann\0a@example.com\01500000000\0Ann\0a@example.com\01500000000\0{refs}\0subject {i}\0body\0"));
    }
    s
}

#[test]
fn parsing_a_long_log_scales() {
    assert_scales("log", 25_000, 2_500, log_output, |s| {
        black_box(parse::parse_log(s));
    });
}

#[test]
fn laying_out_a_long_graph_scales() {
    let commits = |n: usize| {
        let mut s = String::new();
        for i in 0..n {
            // A merge every so often, and a side line running beside the main one.
            let parents = if i + 2 >= n {
                String::new()
            } else if i % 10 == 0 {
                format!("{:040x} {:040x}", i + 1, i + 2)
            } else {
                format!("{:040x}", i + 1)
            };
            s.push_str(&format!("{i:040x}\0{parents}\0A\0a@x\01\0A\0a@x\01\0\0s\0\0"));
        }
        s
    };
    assert_scales("graph", 25_000, 2_500, commits, |s| {
        black_box(graph::layout(&parse::parse_log(s)));
    });
}

#[test]
fn parsing_a_huge_status_scales() {
    let status = |n: usize| {
        let mut s = String::from("# branch.oid abc\0# branch.head main\0");
        for i in 0..n {
            s.push_str(&format!("1 .M N... 100644 100644 100644 abc def src/dir{}/file{i}.rs\0", i % 50));
            s.push_str(&format!("? new/untracked{i}.txt\0"));
        }
        s
    };
    assert_scales("status", 25_000, 800, status, |s| {
        black_box(parse::parse_status(s));
    });
}

#[test]
fn parsing_a_huge_diff_scales() {
    let unified = |n: usize| {
        let mut s = String::from("diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n");
        for i in 0..n {
            s.push_str(&format!("@@ -{i},3 +{i},3 @@ fn x\n context {i}\n-old {i}\n+new {i}\n context\n"));
        }
        s
    };
    assert_scales("diff", 25_000, 3_000, unified, |s| {
        let files = diff::parse_unified(s);
        black_box(diff::side_by_side(&files[0]));
    });
}

#[test]
fn parsing_a_big_blame_scales() {
    let blame = |n: usize| {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("{:040x} {} {} 1\nauthor Ann\nauthor-mail <a@example.com>\nauthor-time 1500000000\nsummary s{}\nfilename f\n\tline {i}\n", i % 200, i + 1, i + 1, i % 200));
        }
        s
    };
    assert_scales("blame", 25_000, 1_200, blame, |s| {
        black_box(parse::parse_blame(s));
    });
}

#[test]
fn parsing_many_refs_scales() {
    let refs = |n: usize| {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("refs/heads/feature/branch-{i}\0{i:040x}\0 \0origin/feature/branch-{i}\0[ahead 1, behind 2]\01500000000\0subject\0Ann\0\n"));
        }
        s
    };
    let sample = parse::parse_branches(&refs(1));
    assert_eq!(sample.len(), 1, "the generated line has to be one the parser takes");
    assert_scales("refs", 25_000, 1_400, refs, |s| {
        black_box(parse::parse_branches(s));
    });
}

#[test]
fn the_word_diff_of_a_long_line_scales() {
    let line = |n: usize| (0..n).map(|i| format!("word{} ", i % 7)).collect::<String>();
    // The limit for word highlights is 20,000 bytes, so measure just under it.
    let run = |s: &str| {
        let other = s.replacen("word3", "changed", 5);
        black_box(diff::word_diff(s, &other));
    };
    let t = best_of_3(|| run(&line(1_500)));
    assert!(t < Duration::from_millis(500), "{t:?}");
}
