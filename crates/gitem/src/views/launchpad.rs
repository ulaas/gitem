//! Launchpad: pull requests grouped by what they need from you, issues from the
//! host plus Jira and Linear, "start work", and pull-request creation.
use egui::{RichText, Ui};
use gitem_core::forge::{self, ForgeClient, Issue, LaunchpadGroup, PullRequest, RemoteRepo, launchpad_group};
use gitem_core::{GitExe, Repo, ai, cred, ops, util};

use crate::action::{Action, Cx, Later};
use crate::forms::{self, FormDialog};
use crate::task::Task;
use crate::views::Tab;
use crate::widgets::*;

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Prs,
    Issues,
    Create,
}

/// What a load of pull requests found.
struct LoadedPrs {
    /// Who you are on this host.
    me: String,
    list: Vec<PullRequest>,
    /// Why the host couldn't say who you are.
    whoami_failed: Option<String>,
    /// What the host said about what's missing from the list.
    partial: Option<String>,
}

pub struct LaunchpadView {
    page: Page,
    prs: Task<LoadedPrs>,
    /// What loaded, then what each source that failed said.
    issues: Task<(Vec<Issue>, Vec<String>)>,
    started: bool,
    /// What the lists were last loaded for: changes here make them out of date.
    loaded_for: Option<(u64, Option<String>)>,
    /// The branch the status bar's pull request was last looked up for.
    pr_branch: Option<String>,
    /// The source branch as filled in. It follows the current branch until someone types.
    auto_head: Option<String>,
    action: Task<String>,
    filter: String,
    // Create-PR form
    head: String,
    base: String,
    title: String,
    /// The title it came with, which is no loss.
    title_given: String,
    body: String,
    draft: bool,
    ai_text: Task<String>,
    create: Task<String>,
}

impl Default for LaunchpadView {
    fn default() -> Self {
        Self {
            page: Page::Prs,
            prs: Task::default(),
            issues: Task::default(),
            started: false,
            loaded_for: None,
            pr_branch: None,
            auto_head: None,
            action: Task::default(),
            filter: String::new(),
            head: String::new(),
            base: String::new(),
            title: String::new(),
            title_given: String::new(),
            body: String::new(),
            draft: false,
            ai_text: Task::default(),
            create: Task::default(),
        }
    }
}

/// Saved token for this host, or whatever git's credential helpers know about it.
pub fn resolve_client(git: &GitExe, repo: &RemoteRepo) -> Result<ForgeClient, String> {
    let host = repo.token_host();
    let target = format!("{}{host}", cred::PREFIX_FORGE);
    if let Ok(Some(c)) = cred::get(&target)
        && !c.secret.is_empty() {
            return Ok(ForgeClient::new(repo.clone(), c.secret, c.username));
        }
    if let Some((user, pass)) = cred::git_credential_fill(git, &repo.root()) {
        return Ok(ForgeClient::new(repo.clone(), pass, user));
    }
    Err(format!("No token for {host}. Add one with Connect."))
}

pub fn token_form(repo: &RemoteRepo) -> FormDialog {
    let (host, kind) = (repo.token_host(), repo.kind);
    let mut f = FormDialog::new(format!("Connect to {}", kind.label()), "Save token").info(format!("Create a token at {} and paste it here. It's stored in the {}.", repo.token_page(), cred::backend_name()));
    if kind == forge::ForgeKind::Bitbucket {
        // An API token goes with the Atlassian account's email, not the Bitbucket username.
        f = f.info("Give it read and write access to repositories and pull requests, and read access to your account and issues.").text("user", "Atlassian email", "");
    } else if kind == forge::ForgeKind::AzureDevOps {
        f = f.text("user", "Username", "");
    }
    f.password("token", "Token").on_submit(move |v| {
        let t = v.opt("token").ok_or("A token is required.")?;
        cred::set_forge_token(&host, &v.text("user"), &t).map_err(|e| e.to_string())?;
        Ok(vec![Action::Toast(format!("Saved token for {host}")), Action::Refresh])
    })
}

impl LaunchpadView {
    pub fn create_pr_for(head: &str, base: &str) -> Self {
        Self { page: Page::Create, head: head.to_string(), base: base.to_string(), title: head.replace(['-', '_', '/'], " "), ..Default::default() }
    }

    /// Fills in the pull request form, as typing would. For tests.
    pub fn write_for_test(&mut self, title: &str, body: &str) {
        self.title = title.to_string();
        self.body = body.to_string();
    }

    /// What closing this tab would lose: what's been written for a pull request. It's cleared
    /// once the pull request is made.
    pub fn unsaved(&self) -> Option<String> {
        (!self.body.trim().is_empty() || (!self.title.trim().is_empty() && self.title != self.title_given)).then(|| "A pull request you're writing.".to_string())
    }

    pub fn key(&self) -> String {
        "launchpad".into()
    }

    /// "Create pull request" with this tab open goes to the form, for that branch.
    pub fn retarget(&mut self, new: Self) {
        if new.page == Page::Create {
            self.page = Page::Create;
            self.head = new.head;
            self.base = new.base;
            self.auto_head = None;
            if self.body.trim().is_empty() {
                self.title = new.title;
            }
        }
    }

    pub fn title(&self) -> String {
        "Launchpad".into()
    }

    fn refresh(&mut self, ctx: &egui::Context, cx: &Cx) {
        self.started = true;
        self.loaded_for = Some((cx.data.forge_gen, cx.forge().map(|f| format!("{}/{}", f.host, f.path))));
        let trackers = cx.settings.issue_trackers.clone();
        let git = cx.repo.git.clone();
        let repo = cx.forge().cloned();
        if let Some(r) = repo.clone() {
            let g = git.clone();
            self.prs.spawn(ctx, move || -> Result<LoadedPrs, String> {
                let c = resolve_client(&g, &r)?;
                let (me, whoami_failed) = match c.whoami() {
                    Ok(me) if me.is_empty() => (me, Some("The host gave no name.".to_string())),
                    Ok(me) => (me, None),
                    Err(e) => (String::new(), Some(e.to_string())),
                };
                let (list, partial) = c.list_prs_noting_problems().map_err(|e| e.to_string())?;
                Ok(LoadedPrs { me, list, whoami_failed, partial })
            });
        }
        self.issues.spawn(ctx, move || -> Result<(Vec<Issue>, Vec<String>), String> {
            let mut all = Vec::new();
            let mut errs = Vec::new();
            if let Some(r) = &repo {
                match resolve_client(&git, r).and_then(|c| c.list_issues().map_err(|e| e.to_string())) {
                    Ok(v) => all.extend(v),
                    Err(e) => errs.push(e),
                }
            }
            if !trackers.jira_base_url.is_empty() {
                match cred::get("gitem:forge:jira").ok().flatten() {
                    Some(c) => match forge::jira_issues(&trackers.jira_base_url, &trackers.jira_email, &c.secret, &trackers.jira_jql, trackers.jira_server) {
                        Ok(v) => all.extend(v),
                        Err(e) => errs.push(e.to_string()),
                    },
                    None => errs.push("Jira is configured but has no API token.".to_string()),
                }
            }
            if trackers.linear_enabled {
                match cred::get("gitem:forge:linear").ok().flatten() {
                    Some(c) => match forge::linear_issues(&c.secret) {
                        Ok(v) => all.extend(v),
                        Err(e) => errs.push(e.to_string()),
                    },
                    None => errs.push("Linear is enabled but has no API key.".to_string()),
                }
            }
            if trackers.trello_enabled {
                match cred::get("gitem:forge:trello").ok().flatten() {
                    Some(c) => match forge::trello_issues("", &c.secret) {
                        Ok(v) => all.extend(v),
                        Err(e) => errs.push(e.to_string()),
                    },
                    None => errs.push("Trello is enabled but has no credential.".to_string()),
                }
            }
            // One bad token shouldn't hide what the other sources found, or go unmentioned.
            if all.is_empty() && !errs.is_empty() {
                return Err(errs.join("\n"));
            }
            Ok((all, errs))
        });
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        let ctx = ui.ctx().clone();
        let cur_branch = cx.current_branch();
        if self.prs.poll() || self.pr_branch != cur_branch {
            self.pr_branch = cur_branch.clone();
            let found = self.prs.get().and_then(|l| l.list.iter().find(|p| cur_branch.as_deref().is_some_and(|b| p.is_on_branch(b)))).map(|p| (cur_branch.clone().unwrap_or_default(), p.number, p.url.clone()));
            cx.out.push(Action::BranchPr(found));
        }
        self.issues.poll();
        if self.action.poll() {
            if let Some(m) = self.action.value.take() {
                cx.toast(m);
                self.refresh(&ctx, cx);
            }
            if let Some(e) = self.action.error.take() {
                cx.error(e);
            }
        }
        // A saved token, a merge, F5 or another remote: what's listed is out of date.
        if !self.started || self.loaded_for != Some((cx.data.forge_gen, cx.forge().map(|f| format!("{}/{}", f.host, f.path)))) {
            self.refresh(&ctx, cx);
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.page, Page::Prs, "Pull requests");
            ui.selectable_value(&mut self.page, Page::Issues, "Issues");
            // Issues can come from a tracker, but pull requests need a host.
            let hosted = cx.forge().is_some();
            ui.add_enabled_ui(hosted, |ui| ui.selectable_value(&mut self.page, Page::Create, "Create pull request")).inner.on_disabled_hover_text("None of this repository's remotes is on a supported host.");
            ui.separator();
            if ui.add_enabled(hosted, egui::Button::new("Refresh").small()).on_disabled_hover_text("There's no host to ask.").clicked() {
                self.refresh(&ctx, cx);
            }
            // With none, the page below says so, and what to do about it.
            if let Some(r) = cx.forge().cloned() {
                chip(ui, &format!("{} · {}", r.kind.label(), r.path), p.series_text(6));
                if ui.small_button("Connect…").clicked() {
                    cx.form(token_form(&r));
                }
            }
            if self.prs.busy() || self.issues.busy() || self.action.busy() {
                ui.add(spinner(14.0));
            }
        });
        ui.separator();
        match self.page {
            Page::Prs => self.prs_ui(ui, cx),
            Page::Issues => self.issues_ui(ui, cx),
            Page::Create => self.create_ui(ui, cx),
        }
    }

    fn prs_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if let Some(e) = &self.prs.error {
            error_box(ui, e, &p);
            if let Some(r) = cx.forge().cloned()
                && ui.button("Connect…").clicked() {
                    cx.form(token_form(&r));
                }
        }
        ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter").desired_width(f32::INFINITY));
        let Some(loaded) = self.prs.value.take() else {
            if self.prs.busy() {
                spinner_row(ui, "Loading pull requests…", &p);
            } else if cx.forge().is_none() {
                empty_state(ui, "Nothing to show yet", "Pull requests appear here for remotes on GitHub, GitLab, Bitbucket, Azure DevOps, and Gitea.", &p);
            }
            return;
        };
        let (me, prs) = (&loaded.me, &loaded.list);
        let cur = cx.current_branch();
        let q = self.filter.to_lowercase();
        if let Some(why) = &loaded.whoami_failed {
            ui.colored_label(p.warn, "Couldn't find out who you are on this host, so your pull requests are listed under Other.").on_hover_text(why);
        }
        if let Some(why) = &loaded.partial {
            ui.colored_label(p.warn, format!("The host left something out of this list: {why}"));
        }
        if prs.len() >= forge::LIST_LIMIT {
            ui.label(RichText::new(format!("Showing the first {} only.", forge::LIST_LIMIT)).small().color(p.dim));
        }
        egui::ScrollArea::vertical().id_salt("lp-prs").auto_shrink([false, false]).show(ui, |ui| {
            if prs.is_empty() {
                empty_state(ui, "No open pull requests", "Open ones appear here, grouped by what they need.", &p);
            }
            let path = cx.forge().map(|f| f.path.clone()).unwrap_or_default();
            let now = util::now_ts();
            let key_of = |n: u64| format!("{path}#{n}");
            let snoozed: Vec<&PullRequest> = prs.iter().filter(|x| cx.settings.snoozed_prs.iter().any(|(k, until)| *k == key_of(x.number) && *until > now)).collect();
            let awake = |x: &&PullRequest| !snoozed.iter().any(|s| s.number == x.number);
            let matching = |x: &&PullRequest| q.is_empty() || x.title.to_lowercase().contains(&q) || x.head.to_lowercase().contains(&q) || x.number.to_string() == q;
            let pinned: Vec<&PullRequest> = prs.iter().filter(awake).filter(matching).filter(|x| cx.settings.pinned_prs.contains(&key_of(x.number))).collect();
            if !pinned.is_empty() {
                egui::CollapsingHeader::new(RichText::new(format!("Pinned ({})", pinned.len())).strong()).id_salt("pinned").default_open(true).show(ui, |ui| {
                    for pr in pinned {
                        self.pr_card(ui, cx, pr, cur.as_deref(), prs, me);
                    }
                });
            }
            for g in LaunchpadGroup::ALL {
                let group: Vec<&PullRequest> = prs.iter().filter(|x| launchpad_group(x, me) == g).filter(awake).filter(matching).filter(|x| !cx.settings.pinned_prs.contains(&key_of(x.number))).collect();
                if group.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(RichText::new(format!("{} ({})", g.label(), group.len())).strong()).id_salt(g.label()).default_open(g != LaunchpadGroup::Other).show(ui, |ui| {
                    for pr in group {
                        self.pr_card(ui, cx, pr, cur.as_deref(), prs, me);
                    }
                });
            }
            if !snoozed.is_empty() {
                egui::CollapsingHeader::new(RichText::new(format!("Snoozed ({})", snoozed.len())).strong()).id_salt("snoozed").default_open(false).show(ui, |ui| {
                    for pr in snoozed {
                        self.pr_card(ui, cx, pr, cur.as_deref(), prs, me);
                    }
                });
            }
        });
        self.prs.value = Some(loaded);
    }

    fn pr_card(&mut self, ui: &mut Ui, cx: &mut Cx, pr: &PullRequest, cur: Option<&str>, all: &[PullRequest], me: &str) {
        let p = cx.pal.clone();
        let here = cur.is_some_and(|b| pr.is_on_branch(b));
        // A fork's branch can share a name with one here, without being what a pull request sits on.
        let parent = all.iter().find(|x| !x.from_fork && x.head == pr.base);
        egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                crate::widgets::web_link(ui, format!("#{}", pr.number), &pr.url);
                ui.add(egui::Label::new(RichText::new(&pr.title).strong()).truncate());
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(card_line(pr, cx.settings.date_relative)).small().color(p.dim));
                if pr.additions + pr.deletions > 0 {
                    ui.label(RichText::new(format!("+{}", pr.additions)).small().color(p.add_fg));
                    ui.label(RichText::new(format!("-{}", pr.deletions)).small().color(p.del_fg));
                }
                match pr.checks.as_str() {
                    "SUCCESS" => chip(ui, "\u{E182} Checks pass", p.good),
                    "FAILURE" | "ERROR" => chip(ui, "\u{E4F6} Checks failing", p.danger),
                    "PENDING" | "EXPECTED" => chip(ui, "Checks running", p.warn),
                    _ => {}
                }
                match pr.review_decision.as_str() {
                    "APPROVED" => chip(ui, "\u{E182} Approved", p.good),
                    "CHANGES_REQUESTED" => chip(ui, "Changes requested", p.warn),
                    _ => {}
                }
                if pr.mergeable == "CONFLICTING" {
                    chip(ui, "⚠ Conflicts", p.danger);
                }
                if pr.draft {
                    chip(ui, "Draft", p.dim);
                }
                if pr.from_fork {
                    chip(ui, "From a fork", p.dim);
                }
                if let Some(parent) = parent {
                    chip(ui, &format!("Stacked on #{}", parent.number), p.series[0]);
                }
                for l in &pr.labels {
                    chip(ui, l, p.series[6]);
                }
            });
            ui.horizontal(|ui| {
                let remote = cx.data.forge_remote.clone().unwrap_or_else(|| "origin".into());
                let repo = cx.forge().cloned();
                let local = format!("pr/{}", pr.number);
                let fetch_ref = repo.as_ref().and_then(|r| r.pr_ref(pr));
                if here {
                    chip(ui, "Current branch", p.accent);
                    // A branch Check out made. Its own branch here has Pull for this.
                    if cur == Some(local.as_str())
                        && let Some(fetch) = &fetch_ref
                    {
                        if ui.small_button("Update").on_hover_text("Bring in the pull request's new commits").clicked() {
                            cx.run(ops::update_pr_branch(&remote, fetch));
                        }
                        if ui.small_button("Reset to its tip…").on_hover_text("For when its author has force-pushed and Update refuses").clicked() {
                            cx.confirm("Reset to the pull request", &format!("Make {local} what the pull request holds now? Uncommitted changes are lost. Commits made here stay in the reflog."), "Reset", ops::reset_pr_branch(&remote, fetch));
                        }
                        if pr.from_fork && ui.small_button("Push to the fork…").on_hover_text(format!("Send {local} back to {} in the fork", pr.head)).clicked() {
                            cx.form(push_to_fork_form(pr, &local, repo.as_ref(), cx.data.remotes()));
                        }
                    }
                } else if let Some(fetch) = &fetch_ref {
                    let tracks = !pr.from_fork && forge::is_plain_branch_name(&pr.head);
                    let how = FetchPr { remote: remote.clone(), fetch: fetch.clone(), local: local.clone(), head: tracks.then(|| pr.head.clone()) };
                    if ui.small_button("Check out").on_hover_text(format!("Fetch into {local} and switch to it. Asks first if commits of yours are there")).clicked() {
                        cx.out.push(how.clone().start(cx.repo, false));
                    }
                    if ui.small_button("Open in worktree").clicked() {
                        cx.out.push(how.start(cx.repo, true));
                    }
                } else if repo.is_some() {
                    ui.label(RichText::new("No way to fetch this one from here").small().color(p.faint)).on_hover_text("It comes from a fork, and this host has no ref for it in this repository. Open it on the host.");
                }
                // A fork's branch isn't on this remote, but what Check out fetched is here.
                let base = format!("{remote}/{}", pr.base);
                let fetched = cx.data.refs.get().is_some_and(|r| r.branches.iter().any(|b| b.name == local));
                let head = if pr.from_fork { local.clone() } else { format!("{remote}/{}", pr.head) };
                let compare = ui.add_enabled(!pr.from_fork || fetched, egui::Button::new("Compare").small()).on_hover_text("Requires the branches to be fetched").on_disabled_hover_text("This comes from a fork. Check it out first.");
                if compare.clicked() {
                    cx.open(Tab::compare(&base, &head));
                }
                if ui.small_button("Open").clicked() {
                    cx.url(pr.url.clone());
                }
                if ui.small_button("Show details").on_hover_text("Its files, checks and conversation, and a review from here").clicked() {
                    cx.open(Tab::PullRequest(Box::new(crate::views::pullrequest::PullRequestView::new(pr.clone(), me))));
                }
                let key = format!("{}#{}", repo.as_ref().map(|f| f.path.as_str()).unwrap_or(""), pr.number);
                let is_pinned = cx.settings.pinned_prs.contains(&key);
                if ui.small_button(if is_pinned { "Unpin" } else { "Pin" }).clicked() {
                    let k = key.clone();
                    cx.out.push(Action::Settings(Box::new(move |s| {
                        if is_pinned {
                            s.pinned_prs.retain(|x| *x != k);
                        } else {
                            s.pinned_prs.push(k);
                        }
                    })));
                }
                let is_snoozed = cx.settings.snoozed_prs.iter().any(|(k, u)| *k == key && *u > util::now_ts());
                if ui.small_button(if is_snoozed { "Wake" } else { "Snooze a day" }).clicked() {
                    let k = key.clone();
                    cx.out.push(Action::Settings(Box::new(move |s| {
                        s.snoozed_prs.retain(|(x, _)| *x != k);
                        if !is_snoozed {
                            s.snoozed_prs.push((k, util::now_ts() + 86400));
                        }
                    })));
                }
                if here && repo.as_ref().is_some_and(|r| r.kind == forge::ForgeKind::GitHub) && ui.small_button("Suggest my changes").on_hover_text("Post your uncommitted edits as suggested changes on this pull request").clicked()
                    && let Some(r2) = repo.clone() {
                        let (g, rp, n) = (cx.repo.git.clone(), cx.repo.clone(), pr.number);
                        cx.out.push(Action::Background(Box::new(move || {
                            let files = rp.diff(&gitem_core::repo::DiffSource::WorkingVsHead, None, &gitem_core::repo::DiffOpts::default()).map_err(|e| e.to_string())?;
                            let s = forge::suggestions_from_diff(&files);
                            if s.is_empty() {
                                return Err("There are no edits to existing lines to suggest.".to_string());
                            }
                            resolve_client(&g, &r2)?.suggest_changes(n, &s).map_err(|e| e.to_string())?;
                            Ok(format!("Posted {} on #{n}", plural(s.len(), "suggestion", "suggestions")))
                        })));
                    }
                if let Some(parent) = parent
                    && here && ui.small_button("Rebase stack").on_hover_text(format!("Rebase onto {}, moving the branches between with it", parent.head)).clicked() {
                        cx.form(forms::rebase_stack_form(parent.head.clone()));
                    }
                let stack = stack_of(pr, all);
                if here && stack.len() > 1 && ui.small_button("Push stack…").on_hover_text("Force-with-lease every branch below this one, all or none. For after a rebase moved them").clicked() {
                    cx.confirm("Push the stack", &format!("Push {} to {remote} with force-with-lease? It only overwrites what was last fetched from there, and either all of them go or none does.", stack.join(", ")), "Push stack", ops::push_stack(&remote, &stack));
                }
                let agent = cx.settings.agent_command.clone();
                if here && !agent.trim().is_empty() && ui.small_button("Review with agent").on_hover_text(format!("Run {agent} here, in a new terminal")).clicked() {
                    cx.out.push(Action::OpenTerminal(cx.repo.root.clone(), Some(agent)));
                }
                if let Some(r) = repo {
                    let n = pr.number;
                    // A host refuses your own, and saying so here beats a failed request.
                    if !pr.is_by(me) && ui.small_button("Approve").clicked() {
                        let g = cx.repo.git.clone();
                        let r2 = r.clone();
                        self.action.spawn(ui.ctx(), move || -> Result<String, String> {
                            resolve_client(&g, &r2)?.approve_pr(n).map_err(|e| e.to_string())?;
                            Ok(format!("Approved #{n}"))
                        });
                    }
                    if ui.small_button("Comment…").clicked() {
                        let g = cx.repo.git.clone();
                        let r2 = r.clone();
                        cx.form(FormDialog::new(format!("Comment on #{n}"), "Post").multiline("body", "Comment", "").on_submit(move |v| {
                            let body = v.opt("body").ok_or("A comment is required.")?;
                            let (g, r2) = (g.clone(), r2.clone());
                            Ok(vec![Action::Later(Box::new(move || {
                                resolve_client(&g, &r2)?.comment_pr(n, &body).map_err(|e| e.to_string())?;
                                Ok(Later::ForgeChanged(format!("Commented on #{n}")))
                            }))])
                        }));
                    }
                    let refusal = pr.merge_refusal();
                    if ui.add_enabled(refusal.is_none(), egui::Button::new("Merge…").small()).on_disabled_hover_text(refusal.unwrap_or_default()).clicked() {
                        cx.form(crate::views::pullrequest::merge_form(&cx.repo.git, &r, pr));
                    }
                }
            });
        });
        ui.add_space(3.0);
    }

    fn issues_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if let Some(e) = &self.issues.error {
            error_box(ui, e, &p);
        }
        ui.add(crate::widgets::input(&mut self.filter).hint_text("Filter").desired_width(f32::INFINITY));
        let Some((issues, failed)) = self.issues.value.take() else {
            if self.issues.busy() {
                spinner_row(ui, "Loading issues…", &p);
            }
            return;
        };
        for e in &failed {
            error_box(ui, e, &p);
        }
        if issues.len() >= forge::LIST_LIMIT {
            ui.label(RichText::new(format!("Showing the first {} from each source only.", forge::LIST_LIMIT)).small().color(p.dim));
        }
        let q = self.filter.to_lowercase();
        egui::ScrollArea::vertical().id_salt("lp-issues").auto_shrink([false, false]).show(ui, |ui| {
            if issues.is_empty() {
                empty_state(ui, "No open issues", "Issues from the host, Jira, and Linear appear here.", &p);
            }
            for i in issues.iter().filter(|i| q.is_empty() || i.title.to_lowercase().contains(&q) || i.id.to_lowercase().contains(&q)) {
                egui::Frame::new().fill(p.panel2).inner_margin(6.0).corner_radius(4.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        crate::widgets::web_link(ui, &i.id, &i.url);
                        ui.add(egui::Label::new(RichText::new(&i.title).strong()).truncate());
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(issue_line(i, cx.settings.date_relative)).small().color(p.dim));
                        if !i.assignees.is_empty() {
                            ui.label(RichText::new(i.assignees.join(", ")).small().color(p.dim));
                        }
                        for l in &i.labels {
                            chip(ui, l, p.series[6]);
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button("Start work…").on_hover_text("Create a branch or worktree for this issue").clicked() {
                            let base = cx.data.refs.get().and_then(|r| r.default_branch.clone());
                            cx.form(start_work_form_with_agent(cx.repo, i, &cx.settings.start_work_format, base, &cx.settings.agent_command));
                        }
                        if ui.small_button("Open").clicked() {
                            cx.url(i.url.clone());
                        }
                        if ui.small_button("Copy reference").clicked() {
                            cx.copy(i.id.clone());
                        }
                    });
                });
                ui.add_space(3.0);
            }
        });
        self.issues.value = Some((issues, failed));
    }

    fn create_ui(&mut self, ui: &mut Ui, cx: &mut Cx) {
        let p = cx.pal.clone();
        if self.ai_text.poll() {
            if let Some(t) = self.ai_text.value.take() {
                let t = ai::clean_message(&t);
                let (title, body) = t.split_once('\n').unwrap_or((&t, ""));
                self.title = title.trim().trim_start_matches('#').trim().to_string();
                self.body = body.trim().to_string();
            }
            if let Some(e) = self.ai_text.error.take() {
                cx.error(e);
            }
        }
        if self.create.poll() {
            if let Some(url) = self.create.value.take() {
                cx.toast(format!("Created {url}"));
                cx.url(url);
                self.title.clear();
                self.body.clear();
                self.page = Page::Prs;
                self.refresh(&ui.ctx().clone(), cx);
            }
            if let Some(e) = self.create.error.take() {
                cx.error(e);
            }
        }
        let Some(repo) = cx.forge().cloned() else {
            empty_state(ui, "No hosting provider", "Add a remote on a supported host to create pull requests.", &p);
            return;
        };
        if self.head.is_empty() || self.auto_head.as_ref() == Some(&self.head) {
            self.head = cx.current_branch().unwrap_or_default();
            self.auto_head = Some(self.head.clone());
        }
        if self.base.is_empty() {
            self.base = cx.data.base_branch();
        }
        ui.horizontal(|ui| {
            ui.label("From");
            ui.add(crate::widgets::input(&mut self.head).desired_width(200.0));
            ui.label("into");
            ui.add(crate::widgets::input(&mut self.base).desired_width(200.0));
            ui.checkbox(&mut self.draft, "Draft");
        });
        ui.label(RichText::new("The source branch has to be pushed first.").small().color(p.faint));
        ui.add_space(4.0);
        ui.add(crate::widgets::input(&mut self.title).hint_text("Title").desired_width(f32::INFINITY));
        ui.add(crate::widgets::text_area(&mut self.body).hint_text("Description (Markdown)").desired_rows(12).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            let ok = !self.title.trim().is_empty() && !self.head.is_empty() && !self.create.busy();
            let b = egui::Button::new(RichText::new("Create").color(if ok { p.on_accent } else { p.faint })).fill(if ok { p.accent } else { p.panel2 });
            if ui.add_enabled(ok, b).on_disabled_hover_text("It needs a title and a source branch, and one being created has to finish first.").clicked() {
                let g = cx.repo.git.clone();
                let r = repo.clone();
                let (t, bd, h, ba, d) = (self.title.clone(), self.body.clone(), self.head.clone(), self.base.clone(), self.draft);
                self.create.spawn(ui.ctx(), move || -> Result<String, String> { resolve_client(&g, &r)?.create_pr(&t, &bd, &h, &ba, d).map_err(|e| e.to_string()) });
            }
            if ui.button("Open in browser instead").clicked() {
                cx.url(repo.new_pr_url(&self.base, &self.head));
            }
            if ui.add_enabled(!self.ai_text.busy(), egui::Button::new("Write with AI")).on_disabled_hover_text("Already writing this.").clicked() {
                let r = cx.repo.clone();
                let cfg = cx.settings.ai.clone();
                let (h, ba) = (self.head.clone(), self.base.clone());
                self.ai_text.spawn(ui.ctx(), move || -> Result<String, String> {
                    let e = |x: gitem_core::Error| x.to_string();
                    let commits = r.log(&gitem_core::LogQuery { revs: vec![format!("{ba}..{h}")], ..Default::default() }).map_err(e)?;
                    let text: String = commits.iter().map(|c| format!("- {}\n", c.subject)).collect();
                    let diff = r.diff_for_ai(&gitem_core::repo::DiffSource::MergeBase(ba, h)).map_err(e)?;
                    let (s, u) = ai::pr_prompt(&text, &diff);
                    let key = cred::ai_key(cfg.provider.key_name());
                    ai::complete(&cfg, key.as_deref(), &s, &u).map_err(e)
                });
            }
            if self.ai_text.busy() || self.create.busy() {
                ui.add(spinner(14.0));
            }
        });
    }
}

/// The branches of a stack from its bottom up to this pull request, following each one's base to
/// the pull request whose branch that is. Only branches in this repository.
pub fn stack_of(pr: &PullRequest, all: &[PullRequest]) -> Vec<String> {
    let mut chain = vec![pr.head.clone()];
    let mut at = pr;
    while let Some(parent) = all.iter().find(|x| !x.from_fork && x.head == at.base) {
        if chain.contains(&parent.head) {
            break;
        }
        chain.insert(0, parent.head.clone());
        at = parent;
    }
    chain
}

/// A host that sends no date for it gets none shown, rather than one from 1970.
fn when(ts: i64, relative: bool) -> Option<String> {
    (ts > 0).then(|| time_label(ts, relative))
}

fn card_line(pr: &PullRequest, relative: bool) -> String {
    let mut parts = vec![pr.author.clone(), format!("{} → {}", pr.head, pr.base)];
    parts.extend(when(pr.updated, relative));
    parts.join(" · ")
}

fn issue_line(i: &Issue, relative: bool) -> String {
    let mut parts = vec![i.source.clone(), i.state.clone()];
    parts.extend(when(i.updated, relative));
    parts.join(" · ")
}

pub fn start_work_form(repo: &gitem_core::Repo, i: &Issue, format: &str, base: Option<String>) -> FormDialog {
    start_work_form_with_agent(repo, i, format, base, "")
}

/// With a coding agent set up, it can start there too. Only the command from settings goes to the
/// terminal: the issue's words came from elsewhere, and a shell would run what's in them.
pub fn start_work_form_with_agent(repo: &gitem_core::Repo, i: &Issue, format: &str, base: Option<String>, agent: &str) -> FormDialog {
    let name = i.branch_name_as(format);
    let base = base.unwrap_or_else(|| "HEAD".to_string());
    let wt = repo.root.parent().map(|x| x.join(format!("{}.worktrees/{name}", repo.name()))).unwrap_or_default().to_string_lossy().replace('\\', "/");
    let (agent_command, root) = (agent.to_string(), repo.root.clone());
    FormDialog::new(format!("Start work on {}", i.id), "Start")
        .info(i.title.clone())
        .text("name", "Branch name", &name)
        .text("base", "Based on", &base)
        .check("worktree", "Use a new worktree", false)
        .path("path", "Worktree location", &wt, true)
        .check_if(!agent.trim().is_empty(), "agent", &format!("Start {agent} there, in a new terminal"), false)
        .on_submit(move |v| {
            let n = v.opt("name").ok_or("A branch name is required.")?;
            let b = v.opt("base");
            let with_agent = v.on("agent").then(|| agent_command.clone());
            Ok(if v.on("worktree") {
                let p = v.opt("path").ok_or("A worktree location is required.")?;
                let op = ops::worktree_start_work(&p, &n, b.as_deref());
                match with_agent {
                    // Opening it comes first, or a terminal would look for a folder not yet there.
                    Some(a) => vec![Action::RunThen(op, Box::new(Action::Many(vec![Action::OpenRepo(p.clone().into()), Action::OpenTerminal(p.into(), Some(a))])))],
                    None => vec![Action::RunThenOpen(op, p.into())],
                }
            } else {
                let op = ops::branch_start_work(&n, b.as_deref());
                match with_agent {
                    Some(a) => vec![Action::RunThen(op, Box::new(Action::OpenTerminal(root.clone(), Some(a))))],
                    None => vec![Action::Run(op)],
                }
            })
        })
}

/// Bringing a pull request into its local branch, `pr/N`.
#[derive(Clone)]
struct FetchPr {
    remote: String,
    fetch: String,
    local: String,
    /// Its branch on the remote, for a branch in this repository, to follow from there.
    head: Option<String>,
}

impl FetchPr {
    /// Fetching replaces what `pr/N` holds, so first find out if that's anything of the user's.
    fn start(self, repo: &Repo, worktree: bool) -> Action {
        let repo = repo.clone();
        Action::Later(Box::new(move || {
            let found = repo.commits_a_fetch_would_replace(&self.local).map_err(|e| e.to_string())?;
            let Some((sha, subjects)) = found.filter(|(_, s)| !s.is_empty()) else {
                return Ok(Later::Many(self.steps(None, &repo, worktree)));
            };
            let keep = self.free_name(&repo);
            Ok(Later::Form(Box::new(move || self.ask(&repo, sha, subjects, keep, worktree))))
        }))
    }

    /// A name for keeping commits under that isn't taken.
    fn free_name(&self, repo: &Repo) -> String {
        let first = format!("{}-saved", self.local);
        std::iter::once(first.clone()).chain((2..100).map(|n| format!("{first}-{n}"))).find(|n| repo.resolve(&format!("refs/heads/{n}")).is_err()).unwrap_or(first)
    }

    fn steps(&self, expected: Option<&str>, repo: &Repo, worktree: bool) -> Vec<Later> {
        let mut fetch = ops::fetch_pr(&self.remote, &self.fetch, &self.local);
        if let Some(sha) = expected {
            fetch = fetch.expect_rev(&format!("refs/heads/{}", self.local), sha, "It has new commits since you were asked. Nothing was replaced.");
        }
        let mut all = vec![Later::Run(fetch)];
        if let Some(head) = &self.head {
            all.push(Later::Run(ops::track_pr(&self.remote, head, &self.local)));
        }
        if worktree {
            let (repo, local) = (repo.clone(), self.local.clone());
            all.push(Later::Form(Box::new(move || forms::add_worktree(&repo, vec![local.clone()], Some(local)))));
        } else {
            all.push(Later::Run(ops::checkout(&self.local)));
        }
        all
    }

    fn ask(self, repo: &Repo, sha: String, subjects: Vec<String>, keep: String, worktree: bool) -> FormDialog {
        const SHOWN: usize = 8;
        let mut list: Vec<String> = subjects.iter().take(SHOWN).map(|s| format!("  {s}")).collect();
        if subjects.len() > SHOWN {
            list.push(format!("  and {} more", subjects.len() - SHOWN));
        }
        let keep_label = format!("Keep them on a new branch, {keep}, then fetch");
        let repo = repo.clone();
        FormDialog::new(format!("{} has commits of yours", self.local), "Continue")
            .info(format!("Fetching replaces what {} holds, and {} aren't in what was last fetched or on any remote:\n{}", self.local, plural(subjects.len(), "commit", "commits"), list.join("\n")))
            .choice("what", "What to do", &[&keep_label, "Replace them. The reflog has them for a while"], 0)
            .on_submit(move |v| {
                let mut out = Vec::new();
                if v.index("what") == 0 {
                    out.push(Action::Run(ops::keep_branch(&keep, &sha)));
                }
                out.extend(self.steps(Some(&sha), &repo, worktree).into_iter().map(Later::into_action));
                Ok(out)
            })
    }
}

/// A pull request from a fork goes back to its branch there, through a remote for that fork.
fn push_to_fork_form(pr: &forge::PullRequest, local: &str, repo: Option<&forge::RemoteRepo>, remotes: &[gitem_core::model::Remote]) -> FormDialog {
    let owner = pr.head_repo.split('/').next().filter(|o| !o.is_empty()).unwrap_or("fork").to_string();
    let url = repo.and_then(|f| remotes.iter().find_map(|r| forge::fork_remote_url(&r.push_url, &f.path, &pr.head_repo))).unwrap_or_default();
    let (local, head) = (local.to_string(), pr.head.clone());
    let known: Vec<(String, String)> = remotes.iter().map(|r| (r.name.clone(), r.push_url.clone())).collect();
    let mut form = FormDialog::new(format!("Push {local} to the fork"), "Push").text("name", "Remote name", &owner).text("url", "Fork's address", &url).info(format!("Pushes to its branch, {head}, which becomes what {local} follows. The host decides whether you may: its author has to allow edits by maintainers."));
    if url.is_empty() {
        form = form.info("This host doesn't say where the fork is, so its address has to be typed.");
    }
    form.on_submit(move |v| {
        let name = v.opt("name").ok_or("Give the remote a name.")?;
        let url = v.opt("url").ok_or("Give the fork's address.")?;
        let mut out = Vec::new();
        match known.iter().find(|(n, _)| *n == name) {
            Some((_, there)) if *there != url => return Err(format!("A remote called {name} already points at {there}. Choose another name.")),
            Some(_) => {}
            None => out.push(Action::Run(ops::remote_add(&name, &url))),
        }
        out.push(Action::Run(ops::push_pr_to_fork(&name, &local, &head)));
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_with_no_date_says_none_rather_than_56_years_ago() {
        let pr = PullRequest { author: "ann".into(), head: "t".into(), base: "main".into(), ..Default::default() };
        assert_eq!(card_line(&pr, true), "ann · t → main");
        let dated = PullRequest { updated: gitem_core::util::now_ts() - 120, ..pr };
        assert!(card_line(&dated, true).contains("ago"), "{}", card_line(&dated, true));
        let i = Issue { source: "Jira".into(), state: "Open".into(), ..Default::default() };
        assert_eq!(issue_line(&i, true), "Jira · Open");
    }

    #[test]
    fn a_stack_runs_from_its_bottom_to_this_one_and_stops_at_a_fork_or_a_loop() {
        let pr = |n, head: &str, base: &str| PullRequest { number: n, head: head.into(), base: base.into(), ..Default::default() };
        let all = vec![pr(1, "a", "main"), pr(2, "b", "a"), pr(3, "c", "b"), PullRequest { from_fork: true, ..pr(4, "main", "x") }];
        assert_eq!(stack_of(&all[2], &all), ["a", "b", "c"]);
        assert_eq!(stack_of(&all[0], &all), ["a"]);
        let round = vec![pr(1, "a", "b"), pr(2, "b", "a")];
        assert_eq!(stack_of(&round[0], &round), ["b", "a"]);
    }
}
