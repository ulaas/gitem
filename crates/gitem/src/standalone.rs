//! Small single-purpose windows run when git invokes gitem as a helper: password
//! prompts, commit-message editing, the rebase editor, diff tool, and merge tool.
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};

use egui::{RichText, Ui};
use gitem_core::repo::DiffSource;
use gitem_core::settings::Settings;
use gitem_core::{GitExe, Repo};

use crate::action::{Action, Cx};
use crate::chrome;
use crate::data::RepoData;
use crate::theme::{self, Palette};
use crate::views::diffview::DiffView;
use crate::views::mergeview::{MergeSource, MergeView};
use crate::views::rebase::RebaseView;
use crate::widgets::*;

/// Process exit code, set by the helper window before closing.
pub static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

pub enum Mode {
    Askpass { prompt: String, value: String, secret: bool, confirm: bool },
    Editor { path: PathBuf, text: String },
    Rebase(Box<RebaseView>),
    Merge(Box<MergeView>),
    Diff(Box<DiffView>),
}

pub struct HelperApp {
    mode: Mode,
    repo: Option<Repo>,
    data: RepoData,
    settings: Settings,
    pal: Palette,
    themed: bool,
    error: Option<String>,
    /// Exit code and any text for stdout, once the window has an answer.
    pub result: Option<(i32, Option<String>)>,
    shot: Option<(PathBuf, std::time::Instant, bool)>,
    /// Testing aid: answer without a click once the window has drawn some frames.
    accept: Option<(String, u32)>,
    #[cfg(windows)]
    styled_window: bool,
}

/// A repository context for views. Outside any repository, git commands that
/// don't need one (like `diff --no-index`) still work from the current folder.
fn repo_here() -> Option<Repo> {
    let git = GitExe::discover(None).ok()?;
    let cwd = std::env::current_dir().ok()?;
    let found = Repo::open(git.clone(), &cwd).ok();
    found.or_else(|| Some(Repo::outside_any(git, cwd.clone(), cwd.join(".git"))))
}

/// Reads a switch that only tests and visual checks use. Release builds ignore
/// them: `GITEM_TEST_ACCEPT` would otherwise let a variable answer a password
/// prompt or a merge window with nobody there.
fn test_switch(name: &str) -> Option<std::ffi::OsString> {
    if cfg!(debug_assertions) { std::env::var_os(name) } else { None }
}

/// Where eframe keeps window and egui state. `None` means its default, the real
/// profile, which only an ordinary run may touch.
fn storage_in(config_dir: Option<PathBuf>, shot_dir: PathBuf, name: &str, is_shot: bool) -> Option<PathBuf> {
    match config_dir {
        Some(dir) => Some(dir.join(name)),
        None => is_shot.then(|| shot_dir.join(name)),
    }
}

fn shot_dir() -> PathBuf {
    std::env::temp_dir().join(format!("gitem-shot-{}", std::process::id()))
}

/// Picks the storage file and makes its folder, since eframe doesn't when given a path.
pub fn eframe_storage(name: &str, is_shot: bool) -> Option<PathBuf> {
    let path = storage_in(gitem_core::settings::config_dir_override(), shot_dir(), name, is_shot)?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    Some(path)
}

/// A screenshot without `GITEM_CONFIG_DIR` kept its state in a temp folder.
pub fn forget_shot_storage(storage: Option<&std::path::Path>, is_shot: bool) {
    if is_shot && storage.is_some_and(|p| p.starts_with(shot_dir())) {
        let _ = std::fs::remove_dir_all(shot_dir());
    }
}

impl HelperApp {
    pub fn new(mode: Mode) -> Self {
        let settings = Settings::load();
        let pal = Palette::for_settings(settings.theme, &settings.accent);
        let shot = test_switch("GITEM_SHOT").map(|p| (PathBuf::from(p), std::time::Instant::now(), false));
        Self { mode, repo: repo_here(), data: RepoData::default(), settings, pal, themed: false, error: None, result: None, shot, accept: test_switch("GITEM_TEST_ACCEPT").map(|v| (v.to_string_lossy().into_owned(), 0)), #[cfg(windows)] styled_window: false }
    }

    fn auto_accept(&mut self, ctx: &egui::Context) {
        let Some((value, frames)) = &mut self.accept else { return };
        *frames += 1;
        ctx.request_repaint();
        if *frames == 2
            && let Mode::Merge(v) = &mut self.mode {
                v.auto_accept = true;
            }
        if *frames < 10 {
            return;
        }
        let value = value.clone();
        let waited = *frames;
        match &mut self.mode {
            Mode::Askpass { .. } => self.result = Some((0, Some(value))),
            Mode::Editor { path, text } => {
                let saved = format!("{value}\n{text}");
                self.result = Some(std::fs::write(&*path, saved).map_or((1, None), |()| (0, None)));
            }
            Mode::Rebase(v) => {
                if value == "squash" {
                    gitem_core::rebase::squash_all_into_first(&mut v.items);
                }
                let body = gitem_core::rebase::render_todo(&v.items);
                let ok = v.todo_file.as_ref().is_some_and(|p| std::fs::write(p, body).is_ok());
                self.result = Some((if ok { 0 } else { 1 }, None));
            }
            // Answers only once the diff has loaded, and fails unless it holds the text
            // asked for, so a test can tell a real diff from an empty window.
            Mode::Diff(v) => match v.loaded_changes() {
                Some(Ok(lines)) => self.result = Some((if lines.contains(&value) { 0 } else { 1 }, None)),
                Some(Err(_)) => self.result = Some((1, None)),
                None if waited > 20_000 => self.result = Some((1, None)),
                None => {}
            },
            Mode::Merge(_) => {}
        }
    }

    fn askpass(ui: &mut Ui, prompt: &str, value: &mut String, secret: bool, confirm: bool, p: &Palette, result: &mut Option<(i32, Option<String>)>) {
        ui.add_space(6.0);
        ui.label(RichText::new(prompt.trim()).color(p.text));
        ui.add_space(6.0);
        if confirm {
            ui.horizontal(|ui| {
                if ui.button("Yes").clicked() {
                    *result = Some((0, Some("yes".to_string())));
                }
                if ui.button("No").clicked() {
                    *result = Some((0, Some("no".to_string())));
                }
            });
            return;
        }
        let r = ui.add(crate::widgets::input(value).password(secret).desired_width(f32::INFINITY));
        r.request_focus();
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("OK").color(p.on_accent)).fill(p.accent)).clicked() || enter {
                *result = Some((0, Some(value.clone())));
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                *result = Some((1, None));
            }
        });
    }
}

impl eframe::App for HelperApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if !std::mem::replace(&mut self.styled_window, true) {
            chrome::native_window_style(frame);
        }
        #[cfg(not(windows))]
        let _ = frame;
        self.frame(ui);
        let ctx = ui.ctx().clone();
        if let Some((code, out)) = self.result.take() {
            if let Some(text) = out {
                println!("{text}");
            }
            EXIT_CODE.store(code, Ordering::SeqCst);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if let Some((path, at, sent)) = &mut self.shot {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            if !*sent && at.elapsed() > std::time::Duration::from_secs(2) {
                *sent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let image = ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }));
            if let Some(img) = image {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Some(buf) = ::image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                    let _ = buf.save(&*path);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl HelperApp {
    /// One frame of the window. Separate from eframe so tests can drive it.
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.themed {
            self.themed = true;
            theme::install_fonts(&ctx);
            theme::apply(&ctx, &self.pal, self.settings.ui_scale);
        }
        self.auto_accept(&ctx);
        let p = self.pal.clone();
        let (title, compact) = match &self.mode {
            Mode::Askpass { .. } => ("Git needs your input", true),
            Mode::Editor { .. } => ("Edit message", false),
            Mode::Rebase(_) => ("Interactive rebase", false),
            Mode::Merge(_) => ("Merge", false),
            Mode::Diff(_) => ("Diff", false),
        };
        egui::Panel::top("helper-title").frame(egui::Frame::new().fill(p.bg)).resizable(false).exact_size(chrome::TITLE_BAR_HEIGHT).show_separator_line(false).show(ui, |ui| chrome::helper_title_bar(ui, title, compact, &p));
        egui::CentralPanel::default().show(ui, |ui| {
            match &mut self.mode {
                Mode::Askpass { prompt, value, secret, confirm } => Self::askpass(ui, prompt, value, *secret, *confirm, &p, &mut self.result),
                Mode::Editor { path, text } => {
                    egui::Panel::bottom("editor-actions").show(ui, |ui| {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            let save = ui.add(egui::Button::new(RichText::new("Save and continue").color(p.on_accent)).fill(p.accent)).clicked() || ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
                            if save {
                                match std::fs::write(&*path, text.as_bytes()) {
                                    Ok(()) => self.result = Some((0, None)),
                                    Err(e) => self.error = Some(e.to_string()),
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                self.result = Some((1, None));
                            }
                            ui.label(RichText::new("Lines starting with # are ignored. Ctrl+Enter saves.").small().color(p.faint));
                        });
                        ui.add_space(2.0);
                    });
                    if let Some(e) = &self.error {
                        error_box(ui, e, &p);
                    }
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.add(crate::widgets::text_area(text).code_editor().desired_width(f32::INFINITY).desired_rows(20));
                    });
                }
                mode => {
                    let Some(repo) = self.repo.as_ref() else {
                        error_box(ui, "Git could not be found.", &p);
                        return;
                    };
                    self.data.poll(&ctx, repo);
                    let mut out: Vec<Action> = Vec::new();
                    let mut cx = Cx { repo, data: &self.data, settings: &self.settings, pal: &p, out: &mut out, dialog_open: false };
                    match mode {
                        Mode::Rebase(v) => {
                            v.ui(ui, &mut cx);
                            if let Some(ok) = v.finished {
                                self.result = Some((if ok { 0 } else { 1 }, None));
                            }
                        }
                        Mode::Merge(v) => {
                            v.ui(ui, &mut cx);
                            if let Some(ok) = v.finished {
                                self.result = Some((if ok { 0 } else { 1 }, None));
                            }
                        }
                        Mode::Diff(v) => v.ui(ui, &mut cx),
                        _ => {}
                    }
                    for a in out {
                        match a {
                            Action::Error(e) => self.error = Some(e),
                            Action::Copy(s) => ctx.copy_text(s),
                            _ => {}
                        }
                    }
                    if let Some(e) = &self.error {
                        error_box(ui, e, &p);
                    }
                }
            }
        });
        chrome::window_edges(&ctx, &p);
    }
}

pub fn run(mode: Mode, title: &str, size: [f32; 2]) -> i32 {
    let is_shot = test_switch("GITEM_SHOT").is_some();
    // Its own file: a helper mustn't overwrite the main window's state.
    let storage = eframe_storage("helper.ron", is_shot);
    let options = eframe::NativeOptions { viewport: chrome::viewport(title, size), persist_window: false, persistence_path: storage.clone(), ..Default::default() };
    let result = eframe::run_native(title, options, Box::new(move |_cc| Ok(Box::new(HelperApp::new(mode)))));
    forget_shot_storage(storage.as_deref(), is_shot);
    if let Err(e) = result {
        eprintln!("gitem: {e}");
        return 1;
    }
    EXIT_CODE.load(Ordering::SeqCst)
}

/// Hidden unless git is plainly asking for a username, which it does by starting with the
/// word. Guessing from words anywhere in it showed unusual prompts in clear, and went by the
/// host's name too: `Password for 'https://username.example'`, or a host containing "pin".
fn prompt_is_secret(prompt: &str) -> bool {
    !prompt.trim_start().to_lowercase().starts_with("username")
}

pub fn askpass(prompt: String) -> i32 {
    let low = prompt.to_lowercase();
    let confirm = low.contains("(yes/no");
    let secret = prompt_is_secret(&prompt);
    // Closing the window is a cancel.
    EXIT_CODE.store(1, Ordering::SeqCst);
    run(Mode::Askpass { prompt, value: String::new(), secret, confirm }, "gitem: git needs your input", [460.0, 204.0])
}

pub fn edit_file(path: PathBuf) -> i32 {
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    EXIT_CODE.store(1, Ordering::SeqCst);
    run(Mode::Editor { path, text }, "gitem: edit message", [680.0, 474.0])
}

pub fn rebase_editor(path: PathBuf) -> i32 {
    EXIT_CODE.store(1, Ordering::SeqCst);
    run(Mode::Rebase(Box::new(RebaseView::for_todo_file(path))), "gitem: interactive rebase", [860.0, 634.0])
}

pub fn mergetool(base: PathBuf, local: PathBuf, remote: PathBuf, merged: PathBuf) -> i32 {
    EXIT_CODE.store(1, Ordering::SeqCst);
    run(Mode::Merge(Box::new(MergeView::with_source(MergeSource::Files { base, local, remote, merged }))), "gitem: merge", [1280.0, 820.0])
}

pub fn difftool(a: String, b: String) -> i32 {
    run(Mode::Diff(Box::new(DiffView::new(DiffSource::Files(a, b), None))), "gitem: diff", [1200.0, 800.0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_username_prompt_is_shown_as_typed() {
        assert!(!prompt_is_secret("Username for 'https://host': "));
        assert!(!prompt_is_secret("Username for 'https://shopping.example': "), "\"pin\" in a host name means nothing");
        for p in ["Password for 'https://me@host': ", "Password for 'https://username.example': ", "Enter passphrase for key 'x': ", "me@host's password: ", "Verification code: ", "One-time code: ", "Enter PIN for 'key': "] {
            assert!(prompt_is_secret(p), "{p}");
        }
    }

    #[test]
    fn eframe_storage_follows_the_config_folder() {
        let (config, shot) = (PathBuf::from("scratch"), PathBuf::from("temp"));
        assert_eq!(storage_in(Some(config.clone()), shot.clone(), "app.ron", false), Some(config.join("app.ron")));
        assert_eq!(storage_in(Some(config.clone()), shot.clone(), "app.ron", true), Some(config.join("app.ron")));
    }

    #[test]
    fn a_screenshot_never_uses_the_real_profile() {
        let shot = PathBuf::from("temp");
        assert_eq!(storage_in(None, shot.clone(), "app.ron", true), Some(shot.join("app.ron")));
    }

    #[test]
    fn an_ordinary_run_keeps_the_real_profile() {
        assert_eq!(storage_in(None, PathBuf::from("temp"), "app.ron", false), None);
    }

    #[test]
    fn release_builds_ignore_the_test_switches() {
        // Any variable that's always set will do, because the name isn't what's checked.
        assert_eq!(test_switch("PATH").is_some(), cfg!(debug_assertions));
    }
}
