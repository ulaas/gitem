//! For `tests/perf_numbers.rs`: a git that takes longer for the named subcommands and logs
//! when each one starts and ends, to see how many pile up. Then it runs the real git.
use std::io::Write;

fn log(line: String) {
    if let Ok(p) = std::env::var("SLOWGIT_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn now() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let real = std::env::var("SLOWGIT_REAL").expect("SLOWGIT_REAL");
    let names = std::env::var("SLOWGIT_MATCH").unwrap_or_default();
    let ms: u64 = std::env::var("SLOWGIT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let hit = args.iter().find(|a| names.split(',').any(|n| !n.is_empty() && n == a.as_str())).cloned();
    if let Some(h) = &hit {
        log(format!("S {} {} {h}", now(), std::process::id()));
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let st = std::process::Command::new(real).args(&args).status().expect("run git");
    if let Some(h) = &hit {
        log(format!("E {} {} {h}", now(), std::process::id()));
    }
    std::process::exit(st.code().unwrap_or(1));
}
