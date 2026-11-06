//! Application settings. Edits go to a working copy and apply immediately through
//! `Action::Settings`, which also saves them.
use egui::{RichText, Ui};
use gitem_core::ai::AiProvider;
use gitem_core::forge::ForgeKind;
use gitem_core::settings::{Autolink, CustomHost, Settings, ThemeChoice};
use gitem_core::{ai, cred};

use crate::action::{Action, Cx};
use crate::forms::FormDialog;
use crate::task::Task;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy, Default)]
enum Page {
    #[default]
    General,
    Blame,
    Integrations,
    Ai,
}

#[derive(Default)]
pub struct SettingsView {
    page: Page,
    draft: Option<Settings>,
    ai_test: Task<String>,
    git_version: Option<String>,
}

impl SettingsView {
    pub fn key(&self) -> String {
        "settings".into()
    }
    pub fn title(&self) -> String {
        "Settings".into()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let mut s = self.draft.take().unwrap_or_else(|| cx.settings.clone());
        let before = serde_json::to_string(&s).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::General, "General");
            ui.selectable_value(&mut self.page, Page::Blame, "Blame");
            ui.selectable_value(&mut self.page, Page::Integrations, "Integrations");
            ui.selectable_value(&mut self.page, Page::Ai, "AI");
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("settings").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(720.0);
            match self.page {
                Page::General => self.general(ui, cx, &mut s),
                Page::Blame => self.blame(ui, cx, &mut s),
                Page::Integrations => self.integrations(ui, cx, &mut s),
                Page::Ai => self.ai(ui, cx, &mut s),
            }
        });
        if serde_json::to_string(&s).unwrap_or_default() != before {
            let c = s.clone();
            cx.out.push(Action::Settings(Box::new(move |t| *t = c)));
        }
        self.draft = Some(s);
    }

    fn general(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings) {
        let p = cx.pal.clone();
        section(ui, "Appearance", &p);
        ui.horizontal(|ui| {
            ui.label("Theme");
            ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
            ui.selectable_value(&mut s.theme, ThemeChoice::Midnight, "Midnight");
            ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
        });
        ui.horizontal(|ui| {
            ui.label("Language (experimental)");
            for (code, name) in crate::i18n::LANGUAGES {
                ui.selectable_value(&mut s.language, code.to_string(), name);
            }
        });
        ui.add(egui::Slider::new(&mut s.ui_scale, 0.75..=2.0).text("Interface zoom"));
        ui.add(egui::Slider::new(&mut s.editor_font_size, 9.0..=22.0).text("Code font size"));
        ui.checkbox(&mut s.date_relative, "Show relative dates (\"3 hours ago\")");
        ui.checkbox(&mut s.gravatar, "Show Gravatar avatars").on_hover_text("Sends a hash of each author's email to gravatar.com. Otherwise initials are used.");

        section(ui, "Git", &p);
        ui.label(RichText::new(format!("In use: {} ({:?})", cx.repo.git.path.display(), cx.repo.git.source)).small().color(p.dim));
        let version = self.git_version.get_or_insert_with(|| cx.repo.git.version());
        ui.label(RichText::new(version.as_str()).small().color(p.dim));
        ui.horizontal(|ui| {
            ui.label("Custom git path");
            ui.add(egui::TextEdit::singleline(&mut s.git_path).hint_text("Leave blank to discover").desired_width(360.0));
            if ui.small_button("Browse…").clicked()
                && let Some(f) = rfd::FileDialog::new().pick_file() {
                    s.git_path = f.to_string_lossy().into_owned();
                }
        });
        ui.label(RichText::new("Search order: this setting, a git/ folder beside gitem (portable), PATH, standard install folders. Restart to apply.").small().color(p.faint));

        section(ui, "External editor", &p);
        ui.add(egui::TextEdit::singleline(&mut s.default_external_editor).hint_text("Path to your editor. Leave blank to reveal files in the file manager instead.").desired_width(f32::INFINITY));

        section(ui, "Behaviour", &p);
        ui.checkbox(&mut s.confirm_destructive, "Ask before destructive commands");
        ui.checkbox(&mut s.prune_on_fetch, "Prune when fetching");
        ui.horizontal(|ui| {
            ui.label("Pull button");
            for (v, t) in [("default", "Git default"), ("ff-only", "Fast-forward only"), ("rebase", "Rebase"), ("merge", "Merge")] {
                if ui.selectable_label(s.pull_mode == v, t).clicked() {
                    s.pull_mode = v.to_string();
                }
            }
        });
        ui.checkbox(&mut s.commit_sign, "Sign commits by default");
        ui.checkbox(&mut s.commit_signoff, "Add Signed-off-by by default");
        ui.checkbox(&mut s.auto_maintenance, "Run git's safe maintenance when opening a repository");
        ui.horizontal(|ui| {
            ui.label("Start Work branch name");
            ui.add(egui::TextEdit::singleline(&mut s.start_work_format).hint_text("${id}-${title}").desired_width(240.0));
        });
        ui.horizontal(|ui| {
            ui.label("Agent command");
            ui.add(egui::TextEdit::singleline(&mut s.agent_command).desired_width(180.0)).on_hover_text("Run in a new terminal by \"Start agent session\" on a worktree");
            ui.label("Worktree task");
            ui.add(egui::TextEdit::singleline(&mut s.worktree_task).hint_text("npm test").desired_width(180.0));
        });
        ui.horizontal(|ui| {
            ui.label("Fetch automatically every");
            ui.add(egui::DragValue::new(&mut s.auto_fetch_minutes).range(0..=240));
            ui.label("minutes (0 = off)");
        });
        ui.horizontal(|ui| {
            ui.label("Graph page size");
            ui.add(egui::DragValue::new(&mut s.graph_page_size).range(100..=50000).speed(100));
        });
        ui.label(RichText::new(format!("Settings file: {}", Settings::path().display())).small().color(p.faint));
    }

    fn blame(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings) {
        let p = cx.pal.clone();
        section(ui, "Blame", &p);
        ui.checkbox(&mut s.inline_blame, "Show blame at the end of the current line");
        ui.label("Inline and status-bar format");
        ui.add(egui::TextEdit::singleline(&mut s.inline_blame_format).desired_width(f32::INFINITY));
        ui.label(RichText::new("Tokens: {author} {ago} {date} {subject} {sha}").small().color(p.faint));
        ui.checkbox(&mut s.code_lens, "Show recent-change lens above symbols");
    }

    fn integrations(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings) {
        let p = cx.pal.clone();
        section(ui, "Hosting providers", &p);
        ui.label(RichText::new("github.com, gitlab.com, bitbucket.org, dev.azure.com, and codeberg.org work automatically. Add self-hosted servers here. Tokens are managed in Credentials.").small().color(p.faint));
        let mut remove = None;
        for (i, h) in s.custom_hosts.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut h.host).hint_text("git.example.com").desired_width(180.0));
                egui::ComboBox::from_id_salt(("kind", i)).selected_text(h.kind.label()).show_ui(ui, |ui| {
                    for k in ForgeKind::ALL {
                        ui.selectable_value(&mut h.kind, k, k.label());
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut h.api_base).hint_text("API URL (optional)").desired_width(220.0));
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
            });
            ui.collapsing(format!("Custom links for {}", if h.host.is_empty() { "this server" } else { &h.host }), |ui| {
                ui.label(RichText::new("Optional. Tokens: ${repo} ${sha} ${file} ${line} ${branch} ${base} ${head}").small().color(p.faint));
                ui.add(egui::TextEdit::singleline(&mut h.templates.commit).hint_text("Commit link").desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(&mut h.templates.file).hint_text("File link").desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(&mut h.templates.branch).hint_text("Branch link").desired_width(f32::INFINITY));
                ui.add(egui::TextEdit::singleline(&mut h.templates.compare).hint_text("Compare link").desired_width(f32::INFINITY));
            });
        }
        if let Some(i) = remove {
            s.custom_hosts.remove(i);
        }
        if ui.small_button("Add server").clicked() {
            s.custom_hosts.push(CustomHost { host: String::new(), kind: ForgeKind::GitLab, api_base: String::new(), templates: Default::default() });
        }

        section(ui, "Coding agents", &p);
        ui.label(RichText::new("Lets agents query this repository through gitem, read-only. Run this once to add it to Claude Code:").small().color(p.faint));
        let exe = std::env::current_exe().map(|e| e.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let mcp = format!("claude mcp add gitem -- \"{exe}\" --mcp");
        ui.horizontal(|ui| {
            ui.label(RichText::new(&mcp).monospace().small());
            if ui.small_button("Copy").clicked() {
                cx.copy(mcp.clone());
            }
        });
        section(ui, "gitem:// links", &p);
        ui.label(RichText::new("Lets \"Copy gitem link\" URLs open here. Adds a per-user entry to the Windows registry.").small().color(p.faint));
        if ui.small_button("Register link handler").clicked() {
            match crate::views::settings::register_links(&exe) {
                Ok(()) => cx.toast("gitem:// links now open here"),
                Err(e) => cx.error(e),
            }
        }

        section(ui, "Autolinks", &p);
        ui.label(RichText::new("Turn references in commit messages into links. Use <num> in the URL for the matched id.").small().color(p.faint));
        let mut remove = None;
        for (i, a) in s.autolinks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut a.prefix).hint_text("JIRA-").desired_width(90.0));
                ui.add(egui::TextEdit::singleline(&mut a.url).hint_text("https://example.com/browse/JIRA-<num>").desired_width(340.0));
                ui.checkbox(&mut a.alphanumeric, "Letters too");
                if ui.small_button("✖").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            s.autolinks.remove(i);
        }
        if ui.small_button("Add autolink").clicked() {
            s.autolinks.push(Autolink { prefix: String::new(), url: String::new(), alphanumeric: false });
        }

        section(ui, "Jira", &p);
        ui.add(egui::TextEdit::singleline(&mut s.issue_trackers.jira_base_url).hint_text("https://you.atlassian.net").desired_width(360.0));
        ui.add(egui::TextEdit::singleline(&mut s.issue_trackers.jira_email).hint_text("Account email").desired_width(360.0));
        ui.add(egui::TextEdit::singleline(&mut s.issue_trackers.jira_jql).hint_text("JQL (default: unresolved, assigned to you)").desired_width(f32::INFINITY));
        if ui.small_button("Set Jira API token…").clicked() {
            cx.form(secret_form("Jira API token", "jira"));
        }
        ui.checkbox(&mut s.issue_trackers.jira_server, "Self-hosted Jira (Data Center or Server)");
        section(ui, "Trello", &p);
        ui.checkbox(&mut s.issue_trackers.trello_enabled, "Show my Trello cards in Launchpad");
        if ui.small_button("Set Trello credential (apikey:token)…").clicked() {
            cx.form(secret_form("Trello credential", "trello"));
        }
        section(ui, "Linear", &p);
        ui.checkbox(&mut s.issue_trackers.linear_enabled, "Show assigned Linear issues in Launchpad");
        if ui.small_button("Set Linear API key…").clicked() {
            cx.form(secret_form("Linear API key", "linear"));
        }
    }

    fn ai(&mut self, ui: &mut Ui, cx: &mut Cx, s: &mut Settings) {
        let p = cx.pal.clone();
        self.ai_test.poll();
        section(ui, "Provider", &p);
        egui::ComboBox::from_id_salt("ai-provider").selected_text(s.ai.provider.label()).width(320.0).show_ui(ui, |ui| {
            for a in AiProvider::ALL {
                ui.selectable_value(&mut s.ai.provider, a, a.label());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Model");
            ui.add(egui::TextEdit::singleline(&mut s.ai.model).hint_text(s.ai.provider.default_model()).desired_width(260.0));
        });
        ui.horizontal(|ui| {
            ui.label("API URL");
            ui.add(egui::TextEdit::singleline(&mut s.ai.base_url).hint_text("Leave blank for the provider default").desired_width(360.0));
        });
        let provider = s.ai.provider;
        let has = cred::ai_key(provider.key_name()).is_some();
        ui.horizontal(|ui| {
            ui.label(RichText::new(if has { "API key is set" } else if provider.needs_key() { "No API key" } else { "API key optional" }).color(if has { p.good } else { p.dim }));
            if ui.small_button("Set key…").clicked() {
                let name = provider.key_name();
                cx.form(FormDialog::new(format!("{} API key", provider.label()), "Save").password("key", "Key").on_submit(move |v| {
                    let k = v.opt("key").ok_or("A key is required.")?;
                    cred::set_ai_key(name, &k).map_err(|e| e.to_string())?;
                    Ok(vec![Action::Toast("API key saved".into())])
                }));
            }
            if has && ui.small_button("Remove key").clicked() {
                let _ = cred::delete(&format!("{}{}", cred::PREFIX_AI, provider.key_name()));
            }
            if ui.small_button("Test").clicked() {
                let cfg = s.ai.clone();
                self.ai_test.spawn(ui.ctx(), move || -> Result<String, String> {
                    let k = cred::ai_key(cfg.provider.key_name());
                    ai::complete(&cfg, k.as_deref(), "Reply with the single word: ready", "Are you there?").map_err(|e| e.to_string())
                });
            }
            if self.ai_test.busy() {
                ui.add(egui::Spinner::new().size(14.0));
            }
        });
        if let Some(e) = &self.ai_test.error {
            error_box(ui, e, &p);
        } else if let Some(t) = self.ai_test.get() {
            ui.label(RichText::new(format!("Response: {}", t.trim())).color(p.good));
        }
        ui.label(RichText::new("AI features send the relevant diff to this provider, only when you ask.").small().color(p.faint));
        ui.label(RichText::new("Files matched by .aiignore, .cursorignore, or .aiexclude are left out of every request.").small().color(p.faint));
        section(ui, "Instructions for reviews and conflict resolution", &p);
        ui.add(egui::TextEdit::multiline(&mut s.ai.custom_instructions).desired_rows(2).desired_width(f32::INFINITY));
        section(ui, "Commit message conventions", &p);
        ui.add(egui::TextEdit::multiline(&mut s.ai.commit_style).hint_text("e.g. Conventional Commits: feat:, fix:, chore:").desired_rows(3).desired_width(f32::INFINITY));
    }
}

fn secret_form(title: &str, host: &'static str) -> FormDialog {
    FormDialog::new(title, "Save").password("key", "Value").on_submit(move |v| {
        let k = v.opt("key").ok_or("A value is required.")?;
        cred::set_forge_token(host, "", &k).map_err(|e| e.to_string())?;
        Ok(vec![Action::Toast("Saved".into())])
    })
}

/// Register the `gitem://` scheme for this user only, without a shell.
pub fn register_links(exe: &str) -> Result<(), String> {
    let exe = exe.replace('/', "\\");
    let root = r"HKCU\Software\Classes\gitem";
    let open_key = format!(r"{root}\shell\open\command");
    let open_cmd = format!("\"{exe}\" \"%1\"");
    let runs: [Vec<&str>; 3] = [vec!["add", root, "/ve", "/d", "URL:gitem", "/f"], vec!["add", root, "/v", "URL Protocol", "/d", "", "/f"], vec!["add", &open_key, "/ve", "/d", &open_cmd, "/f"]];
    for args in runs {
        let o = std::process::Command::new("reg").args(&args).output().map_err(|e| format!("Could not run reg: {e}"))?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
    }
    Ok(())
}
