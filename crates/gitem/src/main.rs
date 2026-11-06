// No console window on Windows in release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::Instant;

use gitem::{app, standalone, views::Tab};

const USAGE: &str = "gitem [repository-path ...]

  --version       print the version and the commit this was built from

Helper modes, used by git:
  --credential-helper get|store|erase
  --edit-file FILE
  --rebase-editor TODO-FILE
  --apply-todo SOURCE TODO-FILE
  --difftool LEFT RIGHT
  --mergetool BASE LOCAL REMOTE MERGED

For coding agents:
  --mcp [PATH]    read-only MCP server over stdio

Links:
  gitem://open?repo=PATH[&commit=SHA][&file=PATH][&line=N]

Visual checks:
  --shot OUT.png [--tab NAME] [--file PATH] [--size WxH] [--palette QUERY]";

fn tab_by_name(name: &str) -> Option<Tab> {
    Some(match name {
        "home" => Tab::home(),
        "graph" => Tab::graph(),
        "changes" => Tab::changes(),
        "launchpad" => Tab::Launchpad(Box::default()),
        "search" => Tab::Search(Box::default()),
        "worktrees" => Tab::Worktrees(Box::default()),
        "contributors" => Tab::Contributors(Box::default()),
        "reflog" => Tab::Reflog(Box::default()),
        "rebase" => Tab::rebase(None),
        "bisect" => Tab::Bisect(Box::default()),
        "patches" => Tab::Patches(Box::default()),
        "composer" => Tab::Composer(Box::default()),
        "tools" => Tab::Tools(Box::default()),
        "config" => Tab::Config(Box::default()),
        "credentials" => Tab::Credentials(Box::default()),
        "console" => Tab::Console(Box::default()),
        "settings" => Tab::Settings(Box::default()),
        "agents" => Tab::Agents(Box::default()),
        "review" => Tab::Review(Box::default()),
        "health" => Tab::Health(Box::default()),
        "treemap" => Tab::Treemap(Box::default()),
        "visual" => Tab::visual(""),
        _ => match name.strip_prefix("settings:") {
            Some(page) => Tab::Settings(Box::new(gitem::views::settings::SettingsView::on_page(page))),
            None => return None,
        },
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args));
}

fn dispatch(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!("{USAGE}");
            0
        }
        Some("--version" | "-V") => {
            println!("gitem {}", gitem::app::version_line());
            0
        }
        Some("--credential-helper") => gitem_core::cred::helper_main(args.get(1).map(String::as_str).unwrap_or("")),
        Some("--mcp") => gitem_core::mcp::serve(std::path::Path::new(args.get(1).map_or(".", String::as_str))),
        Some("--edit-file") if args.len() >= 2 => standalone::edit_file(PathBuf::from(&args[1])),
        Some("--rebase-editor") if args.len() >= 2 => standalone::rebase_editor(PathBuf::from(&args[1])),
        Some("--apply-todo") if args.len() >= 3 => apply_todo(&args[1], &args[2]),
        Some("--difftool") if args.len() >= 3 => standalone::difftool(args[1].clone(), args[2].clone()),
        Some("--mergetool") if args.len() >= 5 => standalone::mergetool(args[1].clone().into(), args[2].clone().into(), args[3].clone().into(), args[4].clone().into()),
        // Git and ssh pass only the prompt text and set our marker variable.
        Some(prompt) if std::env::var_os("GITEM_ASKPASS").is_some() && !prompt.starts_with("--") => standalone::askpass(prompt.to_string()),
        _ => run_app(args),
    }
}

/// Sequence-editor mode for in-app rebases: the list is already decided.
fn apply_todo(source: &str, todo: &str) -> i32 {
    let result = std::fs::read_to_string(source).and_then(|plan| {
        let generated = std::fs::read_to_string(todo).unwrap_or_default();
        std::fs::write(todo, gitem_core::rebase::merge_plan(&generated, &plan))
    });
    let _ = std::fs::remove_file(source);
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("gitem: {e}");
            1
        }
    }
}

fn run_app(args: &[String]) -> i32 {
    let mut paths = Vec::new();
    let mut shot = None;
    let mut tabs: Vec<Tab> = Vec::new();
    let mut size = [1500.0f32, 940.0];
    let mut inspect = None;
    let mut palette = None;
    let mut link = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--shot" => shot = it.next().map(PathBuf::from),
            "--tab" => {
                if let Some(t) = it.next().and_then(|n| tab_by_name(n)) {
                    tabs.push(t);
                }
            }
            "--file" => {
                if let Some(f) = it.next() {
                    tabs.push(Tab::file(f, None, None));
                }
            }
            "--visual" => {
                if let Some(f) = it.next() {
                    tabs.push(Tab::visual(f));
                }
            }
            "--merge" => {
                if let Some(f) = it.next() {
                    tabs.push(Tab::merge(f));
                }
            }
            "--diff-commit" => {
                if let Some(f) = it.next() {
                    tabs.push(Tab::diff(gitem_core::repo::DiffSource::Commit(f.clone()), None));
                }
            }
            "--inspect" => inspect = it.next().cloned(),
            "--palette" => palette = it.next().cloned(),
            "--diff-unstaged" => tabs.push(Tab::diff(gitem_core::repo::DiffSource::Unstaged, None)),
            "--size" => {
                if let Some((w, h)) = it.next().and_then(|s| s.split_once('x')) {
                    size = [w.parse().unwrap_or(size[0]), h.parse().unwrap_or(size[1])];
                }
            }
            s if s.starts_with("gitem://") => {
                match gitem_core::forge::DeepLink::parse(s) {
                    Some(d) => link = Some(d),
                    None => {
                        eprintln!("Not a valid gitem link: {s}");
                        return 2;
                    }
                }
            }
            s if s.starts_with("--") => {
                eprintln!("Unknown option {s}\n\n{USAGE}");
                return 2;
            }
            p => paths.push(PathBuf::from(p)),
        }
    }
    let options = eframe::NativeOptions { viewport: gitem::chrome::viewport("gitem", size).with_min_inner_size([800.0, 500.0]), persist_window: shot.is_none(), ..Default::default() };
    let result = eframe::run_native(
        "gitem",
        options,
        Box::new(move |cc| {
            let mut a = app::GitemApp::new(&cc.egui_ctx, &paths);
            a.shot_tabs = tabs;
            a.shot_inspect = inspect;
            if let Some(q) = &palette {
                a.open_palette_with(q);
            }
            if let Some(d) = link {
                a.apply_action(&cc.egui_ctx, gitem::action::Action::DeepLink(d));
            }
            a.shot = shot.map(|path| app::Shot { path, at: Instant::now(), sent: false, script: Vec::new() });
            Ok(Box::new(a))
        }),
    );
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("gitem: {e}");
            1
        }
    }
}
