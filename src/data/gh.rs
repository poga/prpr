//! `gh` CLI subprocess wrappers. The `GhClient` trait is what the cache
//! depends on; tests substitute a fake. The production binary uses
//! `GhCli`, which shells out to `gh`.

use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};

use crate::data::pr::{Pr, PrDetail};

pub trait GhClient: Send + Sync {
    fn list_prs(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>>;
    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail>;
    fn diff_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<String>;
    /// `method` is one of "merge", "squash", "rebase".
    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()>;
    fn auth_status(&self) -> Result<()>;
}

pub struct GhCli;

const PR_LIST_FIELDS: &str =
    "number,title,author,isDraft,state,createdAt,labels,statusCheckRollup,reviewDecision";
const PR_VIEW_FIELDS: &str = "number,title,author,isDraft,state,createdAt,baseRefName,baseRefOid,headRefName,headRefOid,mergeable,labels,statusCheckRollup,reviewDecision,commits,files";

fn run(cmd: &mut Command) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn: {cmd:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(anyhow!("gh exited with {}: {}", out.status, stderr.trim()));
    }
    Ok(out)
}

impl GhClient for GhCli {
    fn list_prs(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "all",
            "--json",
            PR_LIST_FIELDS,
        ]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` output")?;
        Ok(prs)
    }

    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail> {
        let n = number.to_string();
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "view",
            &n,
            "--json",
            PR_VIEW_FIELDS,
        ]))?;
        let pr: PrDetail = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr view --json` output")?;
        Ok(pr)
    }

    fn diff_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<String> {
        let n = number.to_string();
        let out = run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "diff", &n]))?;
        let s = String::from_utf8(out.stdout)
            .with_context(|| "`gh pr diff` produced non-UTF-8 output")?;
        Ok(s)
    }

    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()> {
        let n = number.to_string();
        let flag = match method {
            "merge" => "--merge",
            "squash" => "--squash",
            "rebase" => "--rebase",
            other => return Err(anyhow!("unknown merge method: {other}")),
        };
        run(Command::new("gh")
            .current_dir(repo_root)
            .args(["pr", "merge", &n, flag]))?;
        Ok(())
    }

    fn auth_status(&self) -> Result<()> {
        run(Command::new("gh").args(["auth", "status"]))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory fake. Tests load JSON fixtures and stuff them into this.
    pub struct FakeGh {
        pub prs: Vec<Pr>,
        pub views: HashMap<u32, PrDetail>,
        pub diffs: HashMap<u32, String>,
        pub merges: Mutex<Vec<(u32, String)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self {
                prs: vec![],
                views: HashMap::new(),
                diffs: HashMap::new(),
                merges: Mutex::new(vec![]),
            }
        }
    }

    impl GhClient for FakeGh {
        fn list_prs(&self, _root: &std::path::Path) -> Result<Vec<Pr>> {
            Ok(self.prs.clone())
        }
        fn view_pr(&self, _root: &std::path::Path, n: u32) -> Result<PrDetail> {
            self.views
                .get(&n)
                .cloned()
                .ok_or_else(|| anyhow!("no fake view for #{n}"))
        }
        fn diff_pr(&self, _root: &std::path::Path, n: u32) -> Result<String> {
            self.diffs
                .get(&n)
                .cloned()
                .ok_or_else(|| anyhow!("no fake diff for #{n}"))
        }
        fn merge_pr(&self, _root: &std::path::Path, n: u32, m: &str) -> Result<()> {
            self.merges.lock().unwrap().push((n, m.to_string()));
            Ok(())
        }
        fn auth_status(&self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fixture_view_round_trips_committed_date() {
        // Guards that the shared fixture carries the field the modal uses.
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let pr: crate::data::pr::PrDetail = serde_json::from_str(json).unwrap();
        assert!(
            pr.commits.iter().all(|c| c.committed_date.is_some()),
            "every commit in the fixture must have committed_date set",
        );
    }
}
