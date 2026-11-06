//! Interactive-rebase todo files: parsing, editing and writing them back.
use crate::model::Commit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
    Exec,
    Break,
    /// `label`, `reset`, `merge`, `update-ref`: preserved verbatim.
    Other,
}

impl RebaseAction {
    pub const CHOICES: [RebaseAction; 6] = [Self::Pick, Self::Reword, Self::Edit, Self::Squash, Self::Fixup, Self::Drop];

    pub fn keyword(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
            Self::Exec => "exec",
            Self::Break => "break",
            Self::Other => "",
        }
    }

    pub fn parse(w: &str) -> Option<Self> {
        Some(match w {
            "p" | "pick" => Self::Pick,
            "r" | "reword" => Self::Reword,
            "e" | "edit" => Self::Edit,
            "s" | "squash" => Self::Squash,
            "f" | "fixup" => Self::Fixup,
            "d" | "drop" => Self::Drop,
            "x" | "exec" => Self::Exec,
            "b" | "break" => Self::Break,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub action: RebaseAction,
    pub sha: String,
    /// Commit subject, or the shell command for `exec`, or the raw line for `Other`.
    pub text: String,
}

impl TodoItem {
    pub fn from_commit(c: &Commit) -> Self {
        Self { action: RebaseAction::Pick, sha: c.sha.clone(), text: c.subject.clone() }
    }

    fn render(&self) -> String {
        match self.action {
            RebaseAction::Exec => format!("exec {}", self.text),
            RebaseAction::Break => "break".to_string(),
            RebaseAction::Other => self.text.clone(),
            a => format!("{} {} {}", a.keyword(), self.sha, self.text),
        }
    }
}

/// Parse a todo file. Comments are dropped; blank lines are ignored.
pub fn parse_todo(text: &str) -> Vec<TodoItem> {
    let mut v = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match RebaseAction::parse(word) {
            Some(RebaseAction::Exec) => v.push(TodoItem { action: RebaseAction::Exec, sha: String::new(), text: rest.to_string() }),
            Some(RebaseAction::Break) => v.push(TodoItem { action: RebaseAction::Break, sha: String::new(), text: String::new() }),
            Some(action) => {
                // `fixup -C sha` / `fixup -c sha` keep their flag in the raw form.
                if rest.starts_with('-') {
                    v.push(TodoItem { action: RebaseAction::Other, sha: String::new(), text: line.to_string() });
                } else {
                    let (sha, subject) = rest.split_once(' ').unwrap_or((rest, ""));
                    v.push(TodoItem { action, sha: sha.to_string(), text: subject.to_string() });
                }
            }
            None => v.push(TodoItem { action: RebaseAction::Other, sha: String::new(), text: line.to_string() }),
        }
    }
    v
}

pub fn render_todo(items: &[TodoItem]) -> String {
    let mut s = String::new();
    for i in items {
        s.push_str(&i.render());
        s.push('\n');
    }
    s
}

/// Problems that make git reject or misinterpret the list. Empty means valid.
pub fn validate(items: &[TodoItem]) -> Vec<String> {
    let mut errs = Vec::new();
    let first_real = items.iter().find(|i| !matches!(i.action, RebaseAction::Exec | RebaseAction::Break | RebaseAction::Other | RebaseAction::Drop));
    if let Some(f) = first_real
        && matches!(f.action, RebaseAction::Squash | RebaseAction::Fixup) {
            errs.push("The first commit cannot be squashed or fixed up: there is nothing before it to combine with.".to_string());
        }
    errs
}

/// Combine the user's plan with the list git generated. The plan decides commits
/// and their order, while steps only git can produce, the `update-ref` lines that
/// `--update-refs` adds, stay attached to the commit they followed.
pub fn merge_plan(generated: &str, plan: &str) -> String {
    let same = |a: &str, b: &str| !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a));
    let mut owners: Vec<(String, Vec<String>)> = Vec::new();
    let mut unowned: Vec<String> = Vec::new();
    for item in parse_todo(generated) {
        if item.action == RebaseAction::Other && item.text.starts_with("update-ref ") {
            match owners.last_mut() {
                Some((_, refs)) => refs.push(item.text),
                None => unowned.push(item.text),
            }
        } else if !item.sha.is_empty() {
            owners.push((item.sha, Vec::new()));
        }
    }
    let mut out = String::new();
    for item in parse_todo(plan) {
        out.push_str(&item.render());
        out.push('\n');
        if let Some((_, refs)) = owners.iter_mut().find(|(sha, _)| same(sha, &item.sha)) {
            for r in refs.drain(..) {
                out.push_str(&r);
                out.push('\n');
            }
        }
    }
    // Anything whose commit isn't in the plan still has to move its branch.
    for r in unowned.into_iter().chain(owners.into_iter().flat_map(|(_, refs)| refs)) {
        out.push_str(&r);
        out.push('\n');
    }
    out
}

/// Place `fixup!` and `squash!` commits straight after the commit they target,
/// with the matching action, as `--autosquash` would. The result stays editable.
pub fn arrange_autosquash(items: &mut Vec<TodoItem>) {
    let mut i = 0;
    let mut done: Vec<String> = Vec::new();
    while i < items.len() {
        let parsed = {
            let t = &items[i].text;
            t.strip_prefix("fixup! ").map(|r| (RebaseAction::Fixup, r.to_string())).or_else(|| t.strip_prefix("squash! ").map(|r| (RebaseAction::Squash, r.to_string())))
        };
        if let Some((action, target)) = parsed {
            let sha = items[i].sha.clone();
            if !done.contains(&sha)
                && let Some(t) = items.iter().position(|x| x.text == target || x.text.starts_with(&target))
                    && t < i {
                        // After the target and anything already attached to it.
                        let mut at = t + 1;
                        while at < i && matches!(items[at].action, RebaseAction::Fixup | RebaseAction::Squash) {
                            at += 1;
                        }
                        let mut item = items.remove(i);
                        item.action = action;
                        items.insert(at, item);
                        done.push(sha);
                        continue;
                    }
        }
        i += 1;
    }
}

/// Toggle all commits between pick and `action`, as the bulk buttons do.
pub fn squash_all_into_first(items: &mut [TodoItem]) {
    let mut seen_first = false;
    for i in items.iter_mut() {
        if matches!(i.action, RebaseAction::Exec | RebaseAction::Break | RebaseAction::Other | RebaseAction::Drop) {
            continue;
        }
        if seen_first {
            i.action = RebaseAction::Squash;
        } else {
            i.action = RebaseAction::Pick;
            seen_first = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let t = "pick abc first commit\n# comment\n\nreword def second\nf 123 third\nexec cargo test\nbreak\nlabel onto\nfixup -C 999 x\n";
        let items = parse_todo(t);
        assert_eq!(items.len(), 7);
        assert_eq!(items[0].action, RebaseAction::Pick);
        assert_eq!(items[2].action, RebaseAction::Fixup);
        assert_eq!(items[3].text, "cargo test");
        assert_eq!(items[5].action, RebaseAction::Other);
        assert_eq!(items[6].action, RebaseAction::Other);
        let r = render_todo(&items);
        assert_eq!(r, "pick abc first commit\nreword def second\nfixup 123 third\nexec cargo test\nbreak\nlabel onto\nfixup -C 999 x\n");
        assert_eq!(parse_todo(&r), items);
    }

    #[test]
    fn plan_keeps_update_ref_lines_with_their_commit() {
        let generated = "pick aaa1 one\nupdate-ref refs/heads/stack\n\npick bbb2 two\n# Rebase comment\n";
        let plan = "pick bbb2222 two\ndrop aaa1111 one\n";
        assert_eq!(merge_plan(generated, plan), "pick bbb2222 two\ndrop aaa1111 one\nupdate-ref refs/heads/stack\n");
    }

    #[test]
    fn plan_without_special_lines_is_used_as_is() {
        assert_eq!(merge_plan("pick a x\n", "pick a x\nexec make\n"), "pick a x\nexec make\n");
    }

    #[test]
    fn fixups_move_to_their_target() {
        let mut items = parse_todo("pick a feature\npick b other\npick c fixup! feature\npick d squash! other\n");
        arrange_autosquash(&mut items);
        let got: Vec<(&str, RebaseAction)> = items.iter().map(|i| (i.sha.as_str(), i.action)).collect();
        assert_eq!(got, vec![("a", RebaseAction::Pick), ("c", RebaseAction::Fixup), ("b", RebaseAction::Pick), ("d", RebaseAction::Squash)]);
    }

    #[test]
    fn validation() {
        let mut items = parse_todo("squash a x\npick b y\n");
        assert_eq!(validate(&items).len(), 1);
        items[0].action = RebaseAction::Pick;
        assert!(validate(&items).is_empty());
    }

    #[test]
    fn squash_bulk() {
        let mut items = parse_todo("pick a x\ndrop c z\npick b y\npick d w\n");
        squash_all_into_first(&mut items);
        assert_eq!(items[0].action, RebaseAction::Pick);
        assert_eq!(items[1].action, RebaseAction::Drop);
        assert_eq!(items[2].action, RebaseAction::Squash);
        assert_eq!(items[3].action, RebaseAction::Squash);
    }
}
