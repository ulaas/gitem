//! Rebase with AI conflict resolution. It stops rather than guessing: when the
//! model is unsure or a conflict isn't plain text, the rebase pauses for you.
use crate::ai;
use crate::merge::{MergeDoc, Region, Resolution, has_conflict_markers};
use crate::model::RepoOp;
use crate::ops::{self, GitOp};
use crate::repo::Repo;
use crate::runner::RunOpts;
use crate::{Error, Result, util};

/// Upper bound on rebase steps, in case git keeps stopping.
const MAX_STEPS: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// HEAD before starting; resetting to it undoes everything.
    pub start: String,
    pub resolved: Vec<String>,
    /// Why the rebase stopped early, with the rebase left in progress.
    pub paused: Option<String>,
}

impl Summary {
    pub fn describe(&self, onto: &str) -> String {
        let mut s = match &self.paused {
            Some(why) => format!("The rebase onto {onto} stopped: {why}\n\nFinish it from the banner at the top of the window, or undo it below.\n"),
            None => format!("Rebased onto {onto}.\n"),
        };
        if self.resolved.is_empty() {
            s.push_str("\nNo conflicts needed resolving.\n");
        } else {
            s.push_str("\nResolved with AI, so please review these:\n");
            for f in &self.resolved {
                s.push_str(&format!("  {f}\n"));
            }
        }
        s
    }
}

/// Everything needed to get back to where the rebase started.
pub fn undo_op(start: &str) -> GitOp {
    GitOp::new("Undo auto-rebase").step(&["rebase", "--abort"]).allow_fail().step(&["reset", "--hard", start])
}

/// Run the rebase. `complete` sends (system, user) to the AI provider.
pub fn run(repo: &Repo, onto: &str, instructions: &str, complete: &dyn Fn(&str, &str) -> Result<String>) -> Result<Summary> {
    let mut sum = Summary { start: repo.resolve("HEAD")?, ..Default::default() };
    let root = Some(repo.root.as_path());
    let first = ops::rebase(&ops::RebaseOpts { onto: onto.to_string(), autostash: true, ..Default::default() }).run(&repo.git, root, None, &mut |_| {});
    if let Err(e) = first {
        // A conflict is expected. Anything else (bad ref, dirty state) is an error.
        if repo.status()?.op == RepoOp::None {
            return Err(e);
        }
    }
    for _ in 0..MAX_STEPS {
        let st = repo.status()?;
        if !matches!(st.op, RepoOp::Rebase | RepoOp::RebaseInteractive) {
            return Ok(sum);
        }
        let conflicts: Vec<String> = st.conflicts().map(|e| e.path.clone()).collect();
        for path in &conflicts {
            match resolve_file(repo, path, instructions, complete) {
                Ok(()) => {
                    ops::mark_resolved(path).run(&repo.git, root, None, &mut |_| {})?;
                    if !sum.resolved.contains(path) {
                        sum.resolved.push(path.clone());
                    }
                }
                Err(why) => {
                    sum.paused = Some(format!("{path}: {why}"));
                    return Ok(sum);
                }
            }
        }
        // The step's own commit message is reused, so no editor is wanted.
        let opts = RunOpts { env: vec![("GIT_EDITOR".to_string(), "true".to_string())], is_write: true, ..Default::default() };
        let args = vec!["rebase".to_string(), "--continue".to_string()];
        let o = repo.git.run_opts(root, &args, &opts, &mut |_| {})?;
        if !o.ok() && repo.status()?.conflicts().next().is_none() {
            sum.paused = Some(o.stderr.lines().next().unwrap_or("git stopped the rebase").to_string());
            return Ok(sum);
        }
    }
    sum.paused = Some("too many steps".to_string());
    Ok(sum)
}

fn resolve_file(repo: &Repo, path: &str, instructions: &str, complete: &dyn Fn(&str, &str) -> Result<String>) -> std::result::Result<(), String> {
    let (b, o, t) = (repo.stage_content(1, path), repo.stage_content(2, path), repo.stage_content(3, path));
    if o.is_empty() || t.is_empty() {
        return Err("one side deleted this file, which needs your decision".to_string());
    }
    let text = |x: &[u8]| -> Option<String> { if util::is_binary(x) { None } else { std::str::from_utf8(x).ok().map(String::from) } };
    let (Some(bt), Some(ot), Some(tt)) = (text(&b), text(&o), text(&t)) else {
        return Err("it isn't UTF-8 text".to_string());
    };
    let mut doc = MergeDoc::new(&bt, &ot, &tt);
    for r in &mut doc.regions {
        if let Region::Conflict { base, ours, theirs, resolution } = r {
            let (s, u) = ai::conflict_prompt(path, &base.join("\n"), &ours.join("\n"), &theirs.join("\n"), instructions);
            let reply = ai::clean_message(&complete(&s, &u).map_err(|e: Error| e.to_string())?);
            if reply.trim() == ai::UNSURE {
                return Err("the AI wasn't confident in a resolution".to_string());
            }
            *resolution = Resolution::Custom(util::split_lines(&reply));
        }
    }
    let merged = doc.render("ours", "theirs");
    if has_conflict_markers(&merged) {
        return Err("the AI's answer still had conflict markers".to_string());
    }
    std::fs::write(repo.root.join(path), merged).map_err(|e| e.to_string())
}
