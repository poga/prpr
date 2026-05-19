//! `gh` CLI subprocess wrappers. The `GhClient` trait is what the cache
//! depends on; tests substitute a fake. The production binary uses
//! `GhCli`, which shells out to `gh`.

use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};

use crate::data::pr::{Pr, PrDetail, PrEnrichment};

pub trait GhClient: Send + Sync {
    /// First pass: light fields, no `statusCheckRollup`/`mergeable`/`reviewDecision`.
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>>;
    /// Second pass: only the heavy fields, keyed by `number` for merge.
    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>>;
    fn view_pr(&self, repo_root: &std::path::Path, number: u32) -> Result<PrDetail>;
    /// `method` is one of "merge", "squash", "rebase".
    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()>;
}

pub struct GhCli;

const PR_LIST_FAST_FIELDS: &str =
    "number,title,author,isDraft,state,createdAt,updatedAt,labels";
const PR_LIST_ENRICHED_FIELDS: &str =
    "number,statusCheckRollup,reviewDecision,mergeable";
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
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "all",
            "--json",
            PR_LIST_FAST_FIELDS,
        ]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (fast) output")?;
        Ok(prs)
    }

    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "all",
            "--json",
            PR_LIST_ENRICHED_FIELDS,
        ]))?;
        let v: Vec<PrEnrichment> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (enriched) output")?;
        Ok(v)
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
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use crate::data::pr::PrEnrichment;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory fake. Tests load JSON fixtures and stuff them into this.
    pub struct FakeGh {
        pub prs_fast: Vec<Pr>,
        pub enrichments: Vec<PrEnrichment>,
        pub views: HashMap<u32, PrDetail>,
        pub merges: Mutex<Vec<(u32, String)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self {
                prs_fast: vec![],
                enrichments: vec![],
                views: HashMap::new(),
                merges: Mutex::new(vec![]),
            }
        }
    }

    impl GhClient for FakeGh {
        fn list_prs_fast(&self, _root: &std::path::Path) -> Result<Vec<Pr>> {
            Ok(self.prs_fast.clone())
        }
        fn list_prs_enriched(&self, _root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
            Ok(self.enrichments.clone())
        }
        fn view_pr(&self, _root: &std::path::Path, n: u32) -> Result<PrDetail> {
            self.views
                .get(&n)
                .cloned()
                .ok_or_else(|| anyhow!("no fake view for #{n}"))
        }
        fn merge_pr(&self, _root: &std::path::Path, n: u32, m: &str) -> Result<()> {
            self.merges.lock().unwrap().push((n, m.to_string()));
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

    #[test]
    fn fake_returns_separate_fast_and_enriched_payloads() {
        use super::GhClient;
        use super::fakes::FakeGh;
        use crate::data::pr::{Author, Label, Pr, PrEnrichment, PrState, StatusCheck};
        let mut fake = FakeGh::new();
        fake.prs_fast = vec![Pr {
            number: 7,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![Label { name: "bug".into() }],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }];
        fake.enrichments = vec![PrEnrichment {
            number: 7,
            status_check_rollup: vec![StatusCheck {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
            }],
            review_decision: None,
            mergeable: Some("MERGEABLE".into()),
        }];
        let fast = fake.list_prs_fast(std::path::Path::new("/x")).unwrap();
        assert_eq!(fast.len(), 1);
        assert!(fast[0].status_check_rollup.is_empty());
        let enriched = fake.list_prs_enriched(std::path::Path::new("/x")).unwrap();
        assert_eq!(enriched.len(), 1);
        assert_eq!(enriched[0].number, 7);
        assert_eq!(enriched[0].status_check_rollup.len(), 1);
    }
}
