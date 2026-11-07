//! An opt-in branch model, in the spirit of git flow: named kinds of branch that start from one
//! place and finish by merging into others. Only plain git underneath, with names from settings,
//! and nothing enforces it: every branch stays an ordinary branch.
use serde::{Deserialize, Serialize};

use crate::ops::GitOp;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchModel {
    pub enabled: bool,
    /// What's released.
    pub main: String,
    /// Where work collects. The same as `main` if there's only the one.
    pub develop: String,
    pub feature_prefix: String,
    pub release_prefix: String,
    pub hotfix_prefix: String,
    /// Goes before a release's or hotfix's name in its tag.
    pub tag_prefix: String,
}

impl Default for BranchModel {
    fn default() -> Self {
        Self { enabled: false, main: "main".to_string(), develop: "develop".to_string(), feature_prefix: "feature/".to_string(), release_prefix: "release/".to_string(), hotfix_prefix: "hotfix/".to_string(), tag_prefix: "v".to_string() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Feature,
    Release,
    Hotfix,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Feature, Kind::Release, Kind::Hotfix];

    pub fn word(self) -> &'static str {
        match self {
            Kind::Feature => "feature",
            Kind::Release => "release",
            Kind::Hotfix => "hotfix",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Kind::Feature => "Feature",
            Kind::Release => "Release",
            Kind::Hotfix => "Hotfix",
        }
    }
}

impl BranchModel {
    fn prefix(&self, kind: Kind) -> &str {
        match kind {
            Kind::Feature => &self.feature_prefix,
            Kind::Release => &self.release_prefix,
            Kind::Hotfix => &self.hotfix_prefix,
        }
    }

    /// Where this kind starts from.
    pub fn base(&self, kind: Kind) -> &str {
        match kind {
            Kind::Hotfix => &self.main,
            Kind::Feature | Kind::Release => &self.develop,
        }
    }

    pub fn branch(&self, kind: Kind, name: &str) -> String {
        format!("{}{name}", self.prefix(kind))
    }

    /// What this branch is, by its prefix, and its name without it. An empty prefix says nothing.
    pub fn kind_of(&self, branch: &str) -> Option<(Kind, String)> {
        Kind::ALL.iter().find_map(|k| {
            let prefix = self.prefix(*k);
            if prefix.is_empty() {
                return None;
            }
            branch.strip_prefix(prefix).filter(|n| !n.is_empty()).map(|n| (*k, n.to_string()))
        })
    }

    /// A new branch of this kind from where it starts, making `develop` first if it isn't there.
    pub fn start(&self, kind: Kind, name: &str, make_develop: bool) -> GitOp {
        let branch = self.branch(kind, name);
        let base = self.base(kind).to_string();
        let mut op = GitOp::new(format!("Start {} {name}", kind.word()));
        op.bad_arg = [name, branch.as_str(), base.as_str(), self.main.as_str()].iter().find(|a| a.starts_with('-')).map(|a| a.to_string());
        if make_develop {
            op = op.step(&["branch", "--no-track", self.develop.as_str(), self.main.as_str()]);
        }
        op.step(&["switch", "--no-track", "-c", branch.as_str(), base.as_str()])
    }

    /// What finishing does, in order. Feature: merge into `develop`, then delete. Release and
    /// hotfix: merge into `main`, tag it, merge into `develop`, then delete. Each merge makes a
    /// merge commit, so that what was done together stays together in the history.
    ///
    /// A conflict stops it there, with the merge in progress for the user to finish. Asking again
    /// then carries on: a merge already done says so and moves on. `tag_exists` keeps that second
    /// time from failing on the tag it made the first time, or moving one someone else made.
    pub fn finish(&self, kind: Kind, name: &str, tag_exists: bool) -> GitOp {
        let branch = self.branch(kind, name);
        let tag = self.tag(name);
        let mut op = GitOp::new(format!("Finish {} {name}", kind.word())).lossless();
        let targets: Vec<&str> = match kind {
            Kind::Feature => vec![&self.develop],
            _ if self.main == self.develop => vec![&self.main],
            _ => vec![&self.main, &self.develop],
        };
        op.bad_arg = [branch.as_str(), tag.as_str(), self.main.as_str(), self.develop.as_str()].iter().find(|a| a.starts_with('-')).map(|a| a.to_string());
        for (i, target) in targets.iter().enumerate() {
            op = op.step(&["switch", *target]).step(&["merge", "--no-ff", "--no-edit", branch.as_str()]);
            if i == 0 && kind != Kind::Feature && !tag_exists {
                op = op.step(&["tag", "-a", tag.as_str(), "-m", &format!("{} {name}", kind.title())]);
            }
        }
        op.step(&["branch", "-d", branch.as_str()])
    }

    pub fn tag(&self, name: &str) -> String {
        format!("{}{name}", self.tag_prefix)
    }

    /// In words, for the question before finishing.
    pub fn finish_says(&self, kind: Kind, name: &str, tag_exists: bool) -> String {
        let branch = self.branch(kind, name);
        let tag = self.tag(name);
        let tagging = if tag_exists { format!("leaves the tag {tag} that's already there as it is") } else { format!("tags that {tag}") };
        match kind {
            Kind::Feature => format!("Merges {branch} into {}, stays there, and deletes {branch}. There's no Undo for this, as it does several things: the reflog has them.", self.develop),
            _ if self.main == self.develop => format!("Merges {branch} into {}, {tagging}, stays there, and deletes {branch}. There's no Undo for this, as it does several things: the reflog has them.", self.main),
            _ => format!("Merges {branch} into {}, {tagging}, merges it into {} too, stays there, and deletes {branch}. There's no Undo for this, as it does several things: the reflog has them.", self.main, self.develop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn its_off_unless_turned_on_and_names_come_from_settings() {
        let d = BranchModel::default();
        assert!(!d.enabled);
        assert_eq!(d.branch(Kind::Feature, "x"), "feature/x");
        assert_eq!(d.base(Kind::Feature), "develop");
        assert_eq!(d.base(Kind::Release), "develop");
        assert_eq!(d.base(Kind::Hotfix), "main");
        let m = BranchModel { main: "trunk".into(), develop: "next".into(), feature_prefix: "f-".into(), ..d.clone() };
        assert_eq!(m.branch(Kind::Feature, "x"), "f-x");
        assert_eq!(m.base(Kind::Hotfix), "trunk");
        assert_eq!(m.kind_of("f-x"), Some((Kind::Feature, "x".to_string())));
        assert_eq!(m.kind_of("feature/x"), None);
        assert_eq!(d.kind_of("hotfix/1.2"), Some((Kind::Hotfix, "1.2".to_string())));
        assert_eq!(d.kind_of("main"), None);
        assert_eq!(d.kind_of("feature/"), None, "a prefix with no name");
        let no_prefix = BranchModel { feature_prefix: String::new(), ..d };
        assert_eq!(no_prefix.kind_of("anything"), None, "no prefix says nothing");
    }

    #[test]
    fn a_name_read_as_an_option_stops_it() {
        let d = BranchModel { develop: "--hard".into(), ..Default::default() };
        assert!(d.start(Kind::Feature, "x", false).bad_arg.is_some());
        assert!(d.finish(Kind::Feature, "x", false).bad_arg.is_some());
        let tag = BranchModel { tag_prefix: "-".into(), ..Default::default() };
        assert!(tag.finish(Kind::Release, "1", false).bad_arg.is_some());
    }
}
