//! Home's "Git on this computer" card, which writes the user's global git config. On its own
//! binary, as that file is process-wide and the other test binaries read it.
gitem_testenv::isolate!();

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, HarnessBuilder};
use gitem::app::GitemApp;
use gitem_core::GitExe;

const PATIENCE: Duration = Duration::from_secs(180);

fn harness<'a>(git: GitExe, root: PathBuf) -> Harness<'a, GitemApp> {
    let ctx = egui::Context::default();
    let mut app = GitemApp::with_settings(&ctx, gitem_core::settings::Settings::default());
    app.git = Ok(git);
    let mut opened = false;
    let builder: HarnessBuilder<GitemApp> = Harness::builder().with_size(vec2(1500.0, 1300.0)).with_max_steps(5);
    builder.build_ui_state(
        move |ui, app: &mut GitemApp| {
            if !opened {
                opened = true;
                let c = ui.ctx().clone();
                app.open_repo(&c, &root, false);
            }
            app.frame(ui);
        },
        app,
    )
}

fn step_until(h: &mut Harness<GitemApp>, what: &str, cond: impl Fn(&Harness<GitemApp>) -> bool) {
    let t = Instant::now();
    while !cond(h) {
        h.step();
        assert!(t.elapsed() < PATIENCE, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn global(git: &GitExe, key: &str) -> String {
    git.run_raw(None, &["config", "--global", "--get", key]).unwrap().text().trim().to_string()
}

/// One of the card's choices: a switch or a segment, by its name. Other things in the app
/// can share a name, like Rebase, but not the role.
fn press(h: &mut Harness<GitemApp>, label: &str, role: Role) {
    step_until(h, label, |h| h.query_all_by_label(label).any(|n| n.accesskit_node().role() == role));
    for _ in 0..3 {
        h.step();
    }
    h.query_all_by_label(label).find(|n| n.accesskit_node().role() == role).unwrap().click();
    h.step();
    h.step();
}

#[test]
fn home_sets_git_config_for_the_whole_machine() {
    // What a new user has: nothing at all.
    std::fs::write(std::env::var_os("GIT_CONFIG_GLOBAL").expect("isolated"), "").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let mut git = GitExe::discover(None).unwrap();
    git.helper = Some(PathBuf::from(env!("CARGO_BIN_EXE_gitem")));
    git.run(Some(&root), &["init", "-q", "-b", "main"]).unwrap();
    // This repository has its own name, which isn't the machine's: the card still asks.
    git.run(Some(&root), &["config", "user.name", "Local Only"]).unwrap();

    let mut h = harness(git.clone(), root);
    step_until(&mut h, "the repository to open", |h| h.state().repos.first().is_some_and(|r| r.data.status.get().is_some()));
    let ctx = h.ctx.clone();
    h.state_mut().apply_action(&ctx, gitem::action::Action::OpenTab(gitem::views::Tab::home()));
    step_until(&mut h, "the card", |h| h.query_by_label_contains("doesn't know who you are").is_some());

    press(&mut h, "Prune on fetch", Role::CheckBox);
    step_until(&mut h, "fetch.prune", |_| global(&git, "fetch.prune") == "true");

    press(&mut h, "Rebase", Role::RadioButton);
    step_until(&mut h, "pull.rebase", |_| global(&git, "pull.rebase") == "true");
    press(&mut h, "Fast-forward only", Role::RadioButton);
    step_until(&mut h, "pull.ff", |_| global(&git, "pull.ff") == "only" && global(&git, "pull.rebase") == "false");

    press(&mut h, "main", Role::RadioButton);
    step_until(&mut h, "init.defaultBranch", |_| global(&git, "init.defaultBranch") == "main");

    // Once read back, the switch shows what git has, not only what was clicked.
    step_until(&mut h, "the switch to read back as on", |h| h.query_all_by_label("Prune on fetch").any(|n| n.accesskit_node().role() == Role::CheckBox && n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)));
    assert!(h.state().errors_showing().is_empty(), "{:?}", h.state().errors_showing());
}
