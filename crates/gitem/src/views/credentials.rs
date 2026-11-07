//! Credential manager: forge tokens, git HTTPS credentials, SSH keys, the askpass
//! and credential-helper wiring, and commit-signing setup.
use std::time::{Duration, Instant};

use egui::{RichText, Ui};
use gitem_core::cred::{self, PREFIX_FORGE, PREFIX_GIT};
use gitem_core::ops;

use crate::action::{Action, Cx};
use crate::forms::FormDialog;
use crate::task::Task;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy, Default)]
enum Page {
    #[default]
    Vault,
    Ssh,
    Helper,
    Signing,
}

#[derive(Default)]
pub struct CredentialsView {
    show_names: bool,
    page: Page,
    entries: Vec<(String, String)>,
    store_problem: Option<String>,
    revealed: Option<(String, String)>,
    loaded: bool,
    ssh_keys: Vec<cred::SshKey>,
    ssh_loaded: bool,
    agent: Task<String>,
    test_host: String,
    test: Task<String>,
    helper_value: Option<String>,
    /// Format, key, commits signed, tags signed, and the allowed signers file.
    signing: Task<(String, String, String, String, Option<String>)>,
    /// What it was read for: the status, and how many times it's been changed here.
    signing_for: Option<(u64, u32)>,
    signing_changes: u32,
    signers: Option<String>,
    pending_delete: Option<String>,
    revealed_at: Option<Instant>,
    last_frame: u64,
    /// With no repository open: what's about this repository, or reads through it, stays out.
    no_repo: bool,
}

/// A revealed secret goes away by itself, in case the screen is left or being shared.
const REVEAL_FOR: Duration = Duration::from_secs(30);

fn reveal_is_over(shown: Duration) -> bool {
    shown >= REVEAL_FOR
}

fn add_token_form() -> FormDialog {
    FormDialog::new("Add API token", "Save")
        .info("Used for pull requests and issues. Jira and Linear use the hosts \"jira\" and \"linear\".")
        .text("host", "Host", "github.com")
        .text("user", "Atlassian email for Bitbucket, username for Azure DevOps", "")
        .password("token", "Token")
        .on_submit(|v| {
            let h = v.opt("host").ok_or("A host is required.")?;
            let t = v.opt("token").ok_or("A token is required.")?;
            cred::set_forge_token(&h, &v.text("user"), &t).map_err(|e| e.to_string())?;
            Ok(vec![Action::Toast(format!("Saved token for {h}"))])
        })
}

fn add_git_cred_form() -> FormDialog {
    FormDialog::new("Add git credential", "Save")
        .info("Served to git over HTTPS when gitem is the credential helper.")
        .text("url", "URL", "https://github.com")
        .text("user", "Username", "")
        .password("pass", "Password or token")
        .check("path", "Only for this path (needs git's credential.useHttpPath)", false)
        .on_submit(|v| {
            let u = v.opt("url").ok_or("A URL is required.")?;
            let target = cred::git_credential_target(&u, v.on("path")).map_err(|e| e.to_string())?;
            cred::store(&target, &v.text("user"), &v.text("pass")).map_err(|e| e.to_string())?;
            let saved = target.strip_prefix(PREFIX_GIT).unwrap_or(&target).to_string();
            Ok(vec![Action::Toast(format!("Credential saved for {saved}"))])
        })
}

fn generate_key_form(helper: Option<std::path::PathBuf>) -> FormDialog {
    FormDialog::new("Generate SSH key", "Generate")
        .info("Creates an ed25519 key pair in your .ssh folder.")
        .text("name", "File name", "id_ed25519")
        .text("comment", "Comment", "")
        .hint("you@example.com")
        .password("pass", "Passphrase (leave blank for none)")
        .on_submit(move |v| {
            let name = v.opt("name").ok_or("A file name is required.")?;
            let path = cred::ssh_generate(&name, &v.text("comment"), &v.text("pass"), helper.as_deref()).map_err(|e| e.to_string())?;
            Ok(vec![Action::Toast(format!("Created {path}"))])
        })
}

/// Enough to tell two names apart.
fn mask_name(name: &str) -> String {
    let first = name.chars().next().unwrap_or('?');
    format!("{first}\u{2026}")
}

impl CredentialsView {
    /// What works with no repository: saved secrets and SSH keys.
    pub fn outside_repo() -> Self {
        Self { no_repo: true, ..Self::default() }
    }

    /// On its commit signing page, for screenshots.
    pub fn on_signing() -> Self {
        Self { page: Page::Signing, ..Self::default() }
    }

    pub fn key(&self) -> String {
        "credentials".into()
    }
    pub fn title(&self) -> String {
        "Credentials".into()
    }

    /// Secrets stay on screen only while someone is looking at them on this page.
    fn hide_secret_if_due(&mut self, ctx: &egui::Context, page_before: Page) {
        // This view draws only while its tab shows, so a gap in frames means it was hidden.
        let frame = ctx.cumulative_frame_nr();
        let tab_was_hidden = frame > self.last_frame.saturating_add(1);
        self.last_frame = frame;
        let timed_out = self.revealed_at.is_some_and(|t| reveal_is_over(t.elapsed()));
        if tab_was_hidden || self.page != page_before || timed_out {
            self.revealed = None;
            self.revealed_at = None;
        }
        if self.revealed.is_some() {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let page_before = self.page;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::Vault, "Stored secrets");
            ui.selectable_value(&mut self.page, Page::Ssh, "SSH keys");
            if !self.no_repo {
                ui.selectable_value(&mut self.page, Page::Helper, "Git integration");
                ui.selectable_value(&mut self.page, Page::Signing, "Commit signing");
            }
        });
        self.hide_secret_if_due(ui.ctx(), page_before);
        if self.page != Page::Vault {
            self.pending_delete = None;
        }
        ui.separator();
        egui::ScrollArea::vertical().id_salt("creds").auto_shrink([false, false]).show(ui, |ui| match self.page {
            Page::Vault => self.vault(ui, cx),
            Page::Ssh => self.ssh(ui, cx),
            Page::Helper => self.helper(ui, cx),
            Page::Signing => self.signing(ui, cx),
        });
    }

    fn vault(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if !self.loaded {
            self.loaded = true;
            self.entries = cred::list_all();
            self.store_problem = cred::store_problem();
        }
        ui.label(RichText::new(format!("Stored in: {}", cred::backend_name())).small().color(p.faint));
        if let Some(problem) = &self.store_problem {
            error_box(ui, problem, &p);
        }
        ui.horizontal(|ui| {
            if ui.button("Add API token…").clicked() {
                cx.form(add_token_form());
                self.loaded = false;
            }
            if ui.button("Add git credential…").clicked() {
                cx.form(add_git_cred_form());
                self.loaded = false;
            }
            if ui.button("Import from GitHub CLI").on_hover_text("Reads the token from `gh auth token`").clicked() {
                match import_gh() {
                    Ok(()) => {
                        cx.toast("Imported the GitHub CLI token");
                        self.loaded = false;
                    }
                    Err(e) => cx.error(e),
                }
            }
            if ui.button("Refresh").clicked() {
                self.loaded = false;
            }
            toggle(ui, &mut self.show_names, "Show usernames", "They're partly hidden until asked for, in case this screen is being shared");
        });
        ui.add_space(6.0);
        let groups: [(&str, &str, &str); 4] = [("API tokens", PREFIX_FORGE, "Tokens for hosts and issue trackers"), ("Git HTTPS credentials", PREFIX_GIT, "Served by gitem's credential helper"), ("AI keys", cred::PREFIX_AI, "Set these in Settings > AI"), ("Git Credential Manager", "git:", "Stored by another helper; shown so you can remove stale entries")];
        let entries = self.entries.clone();
        let mut reload = false;
        for (title, prefix, hint) in groups {
            let items: Vec<&(String, String)> = entries.iter().filter(|(t, _)| t.starts_with(prefix)).collect();
            section(ui, &format!("{title} ({})", items.len()), &p);
            ui.label(RichText::new(hint).small().color(p.faint));
            for (target, user) in items {
                ui.horizontal(|ui| {
                    let name = target.strip_prefix(prefix).unwrap_or(target);
                    ui.add(egui::Label::new(RichText::new(name).monospace()).truncate());
                    if !user.is_empty() {
                        // Screen shares happen. A name shows when asked for.
                        ui.label(RichText::new(if self.show_names { user.clone() } else { mask_name(user) }).small().color(p.dim));
                    }
                    let is_rev = self.revealed.as_ref().is_some_and(|(t, _)| t == target);
                    if is_rev {
                        if let Some((_, s)) = &self.revealed {
                            ui.label(RichText::new(s).monospace().color(p.warn));
                            if ui.small_button("Copy").clicked() {
                                cx.copy(s.clone());
                            }
                        }
                        if ui.small_button("Hide").clicked() {
                            self.revealed = None;
                        }
                    } else if ui.small_button("Reveal").on_hover_text("Shows it for 30 seconds").clicked() {
                        match cred::get(target) {
                            Ok(Some(c)) => {
                                self.revealed = Some((target.clone(), c.secret));
                                self.revealed_at = Some(Instant::now());
                            }
                            _ => cx.error("Could not read this secret."),
                        }
                    }
                    if ui.small_button("Delete").clicked() {
                        if cx.settings.confirm_destructive {
                            self.pending_delete = Some(target.clone());
                        } else {
                            reload |= delete_secret(cx, target);
                        }
                    }
                });
            }
        }
        reload |= self.delete_dialog(ui.ctx(), cx);
        if reload {
            self.loaded = false;
        }
    }

    /// The usual confirmation runs a git command. This one has none to run or show.
    /// Returns true if a secret went.
    fn delete_dialog(&mut self, ctx: &egui::Context, cx: &mut Cx) -> bool {
        let Some(target) = self.pending_delete.clone() else { return false };
        let (mut delete, mut keep) = (false, ctx.input(|i| i.key_pressed(egui::Key::Escape)));
        egui::Window::new("Delete this secret?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_TOP, [0.0, 90.0]).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.label(RichText::new(&target).monospace());
            ui.add_space(4.0);
            ui.label(delete_warning(&target));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                delete = ui.add(egui::Button::new(RichText::new("Delete secret").color(crate::theme::ink_on(cx.pal.danger))).fill(cx.pal.danger)).clicked();
                keep |= ui.button("Keep it").clicked();
            });
        });
        if delete {
            self.pending_delete = None;
            return delete_secret(cx, &target);
        }
        if keep {
            self.pending_delete = None;
        }
        false
    }

    fn ssh(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.agent.poll();
        self.test.poll();
        if !self.ssh_loaded {
            self.ssh_loaded = true;
            self.ssh_keys = cred::list_ssh_keys();
            self.agent.spawn(ui.ctx(), cred::ssh_agent_list);
        }
        ui.horizontal(|ui| {
            if ui.button("Generate key…").clicked() {
                cx.form(generate_key_form(cx.repo.git.helper.clone()));
                self.ssh_loaded = false;
            }
            if ui.button("Refresh").clicked() {
                self.ssh_loaded = false;
            }
        });
        section(ui, &format!("Keys in {}", cred::ssh_dir().display()), &p);
        if self.ssh_keys.is_empty() {
            ui.label(RichText::new("No public keys found.").color(p.dim));
        }
        for k in self.ssh_keys.clone() {
            egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&k.name).strong());
                    chip(ui, &k.kind, p.series[6]);
                    ui.label(RichText::new(&k.comment).small().color(p.dim));
                });
                ui.horizontal(|ui| {
                    if ui.small_button("Copy public key").clicked() {
                        cx.copy(k.public.clone());
                    }
                    if !k.private_path.is_empty() && ui.small_button("Add to agent").clicked() {
                        match cred::ssh_agent_add(&k.private_path) {
                            Ok(m) => {
                                cx.toast(m.trim().to_string());
                                self.ssh_loaded = false;
                            }
                            Err(e) => cx.error(e.to_string()),
                        }
                    }
                    if ui.small_button("Use for signing").clicked() {
                        cx.run(ops::config_set("gpg.format", "ssh", "global"));
                        cx.run(ops::config_set("user.signingkey", &k.public_path.replace('\\', "/"), "global"));
                    }
                });
            });
            ui.add_space(3.0);
        }
        section(ui, "SSH agent", &p);
        match (&self.agent.value, &self.agent.error) {
            (_, Some(e)) => {
                ui.colored_label(p.warn, format!("Agent unavailable: {e}"));
            }
            (Some(t), _) => {
                ui.label(RichText::new(t.trim()).monospace().small());
            }
            _ => spinner_row(ui, "Asking the agent…", &p),
        }
        section(ui, "Test a connection", &p);
        ui.horizontal(|ui| {
            ui.add(crate::widgets::input(&mut self.test_host).hint_text("github.com").desired_width(220.0));
            let host = self.test_host.trim().to_string();
            if ui.add_enabled(!host.is_empty(), egui::Button::new("Test")).on_disabled_hover_text("Type a host to test first.").clicked() {
                self.test.spawn(ui.ctx(), move || -> Result<String, String> { Ok(cred::ssh_test(&host)) });
            }
            if self.test.busy() {
                ui.add(spinner(14.0));
            }
        });
        if let Some(t) = self.test.get() {
            ui.label(RichText::new(t.trim()).monospace().small());
        }
    }

    fn helper(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let current = self.helper_value.get_or_insert_with(|| cx.repo.config_get("credential.helper").unwrap_or_default()).clone();
        section(ui, "Credential helper", &p);
        ui.label(RichText::new(if current.is_empty() { "No credential helper is configured.".to_string() } else { format!("Current: {current}") }).color(p.dim));
        if let Some(exe) = cx.repo.git.helper.clone() {
            let value = cred::helper_config_value(&exe);
            ui.horizontal(|ui| {
                if ui.button("Use gitem for this repository").clicked() {
                    cx.run(ops::config_set("credential.helper", &value, "local"));
                    self.helper_value = None;
                }
                if ui.button("Use gitem for all repositories").clicked() {
                    cx.confirm("Change global credential helper", "Set gitem as the credential helper for every repository on this machine?", "Set globally", ops::config_set("credential.helper", &value, "global"));
                    self.helper_value = None;
                }
                if ui.button("Stop using it here").clicked() {
                    cx.run(ops::config_unset("credential.helper", "local"));
                    self.helper_value = None;
                }
            });
            section(ui, "Git tools", &p);
            ui.label(RichText::new("Make gitem git's diff tool, merge tool, and rebase editor everywhere, including the command line.").color(p.dim));
            let h = exe.to_string_lossy().replace('\\', "/");
            ui.horizontal(|ui| {
                if ui.button("Register as difftool and mergetool").clicked() {
                    cx.run(
                        ops::GitOp::new("Register gitem as difftool and mergetool")
                            .step(&["config", "--global", "difftool.gitem.cmd", &format!("\"{h}\" --difftool \"$LOCAL\" \"$REMOTE\"")])
                            .step(&["config", "--global", "diff.tool", "gitem"])
                            .step(&["config", "--global", "mergetool.gitem.cmd", &format!("\"{h}\" --mergetool \"$BASE\" \"$LOCAL\" \"$REMOTE\" \"$MERGED\"")])
                            .step(&["config", "--global", "mergetool.gitem.trustExitCode", "true"])
                            .step(&["config", "--global", "merge.tool", "gitem"]),
                    );
                }
                if ui.button("Register as rebase editor").clicked() {
                    cx.run(ops::config_set("sequence.editor", &format!("\"{h}\" --rebase-editor"), "global"));
                }
                if ui.button("Register as commit message editor").clicked() {
                    cx.run(ops::config_set("core.editor", &format!("\"{h}\" --edit-file"), "global"));
                }
            });
        }
        section(ui, "Prompts", &p);
        ui.label(RichText::new("When git asks for a username, password, or SSH passphrase, gitem shows a small dialog. Nothing you type there is stored unless a credential helper saves it.").color(p.dim));
    }

    fn signing(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        self.signing.poll();
        let want = (cx.data.status_gen, self.signing_changes);
        if self.signing_for != Some(want) {
            self.signing_for = Some(want);
            self.signing.spawn_git(ui.ctx(), cx.repo, |r| -> Result<_, String> {
                let get = |k: &str| r.config_get(k).unwrap_or_default();
                // Global config only. A repository's own config could otherwise pick which file
                // this page shows and overwrites.
                Ok((get("gpg.format"), get("user.signingkey"), get("commit.gpgsign"), get("tag.gpgsign"), r.global_config_path("gpg.ssh.allowedSignersFile")))
            });
        }
        let Some((fmt, key, sign, tag, configured)) = self.signing.get().cloned() else {
            spinner_row(ui, "Reading signing settings…", &p);
            return;
        };
        egui::Grid::new("sign-grid").num_columns(2).show(ui, |ui| {
            ui.label("Format");
            ui.label(if fmt.is_empty() { "openpgp (default)" } else { &fmt });
            ui.end_row();
            ui.label("Signing key");
            ui.label(if key.is_empty() { "not set" } else { &key });
            ui.end_row();
            ui.label("Sign all commits");
            ui.label(if sign == "true" { "yes" } else { "no" });
            ui.end_row();
            ui.label("Sign all tags");
            ui.label(if tag == "true" { "yes" } else { "no" });
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(if sign == "true" { "Stop signing commits" } else { "Sign all commits" }).clicked() {
                cx.run(ops::config_set("commit.gpgsign", if sign == "true" { "false" } else { "true" }, "global"));
                self.signing_changes += 1;
            }
            if ui.button(if tag == "true" { "Stop signing tags" } else { "Sign all tags" }).clicked() {
                cx.run(ops::config_set("tag.gpgsign", if tag == "true" { "false" } else { "true" }, "global"));
                self.signing_changes += 1;
            }
            if ui.button("Set signing key…").clicked() {
                cx.form(FormDialog::new("Set signing key", "Save").text("key", "GPG key ID, or path to an SSH public key", &key).check("ssh", "It's an SSH key", fmt == "ssh").on_submit(|v| {
                    let k = v.opt("key").ok_or("A key is required.")?;
                    Ok(vec![Action::Run(ops::config_set("user.signingkey", &k, "global")), Action::Run(ops::config_set("gpg.format", if v.on("ssh") { "ssh" } else { "openpgp" }, "global"))])
                }));
            }
        });
        ui.label(RichText::new("Signing status appears on each commit in the Details panel.").small().color(p.faint));
        section(ui, "SSH allowed signers", &p);
        ui.label(RichText::new("Whose SSH signatures to trust, one per line: email, then the public key.").small().color(p.faint));
        let path = configured.clone().map(std::path::PathBuf::from).unwrap_or_else(|| cred::ssh_dir().join("allowed_signers"));
        let text = self.signers.get_or_insert_with(|| std::fs::read_to_string(&path).unwrap_or_default());
        ui.add(egui::Label::new(RichText::new(path.display().to_string()).monospace().small().color(p.dim)).selectable(true));
        ui.add(crate::widgets::text_area(text).code_editor().desired_rows(5).desired_width(f32::INFINITY));
        if ui.button("Save allowed signers").clicked() {
            match std::fs::write(&path, text.as_bytes()) {
                Ok(()) => {
                    if configured.is_none() {
                        cx.run(ops::config_set("gpg.ssh.allowedSignersFile", &path.to_string_lossy().replace('\\', "/"), "global"));
                    }
                    cx.toast("Saved allowed signers");
                }
                Err(e) => cx.error(format!("Could not save: {e}")),
            }
        }
    }
}

fn delete_secret(cx: &mut Cx, target: &str) -> bool {
    if let Err(e) = cred::delete(target) {
        cx.error(e.to_string());
    }
    true
}

fn delete_warning(target: &str) -> &'static str {
    // Git Credential Manager and others share the prefix-less `git:` space with gitem's list.
    if target.starts_with("git:") && !target.starts_with(PREFIX_GIT) {
        "Another tool stored this, probably Git Credential Manager. Whatever uses it will ask you to sign in again, and gitem can't bring it back."
    } else {
        "Whatever uses it stops working until you add it again, and gitem can't bring it back. A token may not be retrievable from its host."
    }
}

fn import_gh() -> Result<(), String> {
    let mut c = std::process::Command::new("gh");
    c.args(["auth", "token"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let o = c.output().map_err(|e| format!("Could not run gh: {e}"))?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
    if t.is_empty() {
        return Err("GitHub CLI returned no token.".to_string());
    }
    cred::set_forge_token("github.com", "", &t).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reveal_lasts_thirty_seconds() {
        assert!(!reveal_is_over(Duration::from_secs(29)));
        assert!(reveal_is_over(Duration::from_secs(30)));
    }

    #[test]
    fn the_warning_says_when_a_secret_belongs_to_another_tool() {
        assert!(delete_warning("git:https://host.invalid").contains("Another tool"));
        assert!(!delete_warning(&format!("{PREFIX_GIT}https://host.invalid")).contains("Another tool"));
        assert!(!delete_warning(&format!("{PREFIX_FORGE}host.invalid")).contains("Another tool"));
    }
}
