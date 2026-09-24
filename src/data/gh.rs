//! `gh` CLI subprocess wrappers. The `GhClient` trait is what the cache
//! depends on; tests substitute a fake. The production binary uses
//! `GhCli`, which shells out to `gh`.

use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};

use crate::data::pr::{Pr, PrEnrichment};

pub trait GhClient: Send + Sync {
    /// First pass: light fields, no `statusCheckRollup`/`mergeable`/`reviewDecision`.
    fn list_prs_fast(&self, repo_root: &std::path::Path) -> Result<Vec<Pr>>;
    /// Second pass: only the heavy fields, keyed by `number` for merge.
    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>>;
    /// `method` is one of "merge", "squash", "rebase".
    fn merge_pr(&self, repo_root: &std::path::Path, number: u32, method: &str) -> Result<()>;
    /// Mark ready (draft=false) or convert to draft (draft=true).
    fn set_pr_draft(&self, repo_root: &std::path::Path, number: u32, draft: bool) -> Result<()>;
}

pub struct GhCli;

const PR_LIST_FAST_FIELDS: &str =
    "number,title,author,isDraft,state,createdAt,updatedAt,labels,baseRefName,headRefName";
const PR_LIST_ENRICHED_FIELDS: &str =
    "number,statusCheckRollup,reviewDecision,mergeable";

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
            "open",
            "--json",
            PR_LIST_FAST_FIELDS,
        ]))?;
        let prs: Vec<Pr> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (fast) output")?;
        Ok(prs.into_iter().filter(|p| p.state == crate::data::pr::PrState::Open).collect())
    }

    fn list_prs_enriched(&self, repo_root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
        let out = run(Command::new("gh").current_dir(repo_root).args([
            "pr",
            "list",
            "--limit",
            "200",
            "--state",
            "open",
            "--json",
            PR_LIST_ENRICHED_FIELDS,
        ]))?;
        let v: Vec<PrEnrichment> = serde_json::from_slice(&out.stdout)
            .with_context(|| "parsing `gh pr list --json` (enriched) output")?;
        Ok(v)
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

    fn set_pr_draft(&self, repo_root: &std::path::Path, number: u32, draft: bool) -> Result<()> {
        let n = number.to_string();
        let mut args = vec!["pr", "ready", n.as_str()];
        if draft {
            args.push("--undo");
        }
        run(Command::new("gh").current_dir(repo_root).args(&args))?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use crate::data::pr::PrEnrichment;
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// In-memory fake. Tests load JSON fixtures and stuff them into this.
    pub struct FakeGh {
        pub prs_fast: Vec<Pr>,
        pub enrichments: Vec<PrEnrichment>,
        pub enrichment_sequence: Mutex<VecDeque<Vec<PrEnrichment>>>,
        pub merges: Mutex<Vec<(u32, String)>>,
        pub set_drafts: Mutex<Vec<(u32, bool)>>,
        /// Ordered log across both mutating calls, so tests can pin sequencing.
        pub calls: Mutex<Vec<String>>,
        /// When set, `set_pr_draft` fails instead of recording.
        pub fail_set_draft: Mutex<bool>,
        /// Per-clone rows; a root not listed here falls back to `prs_fast`.
        pub prs_by_root: Mutex<HashMap<PathBuf, Vec<Pr>>>,
        /// Per-clone enrichments; falls back to `enrichments`.
        pub enrichments_by_root: HashMap<PathBuf, Vec<PrEnrichment>>,
        /// Roots whose `list_prs_fast` fails.
        pub fail_fast_roots: Mutex<HashSet<PathBuf>>,
        /// Roots whose `list_prs_enriched` fails.
        pub fail_enriched_roots: Mutex<HashSet<PathBuf>>,
        /// Roots passed to every list call, in order.
        pub list_roots: Mutex<Vec<PathBuf>>,
        /// Roots passed to mutating calls, in order.
        pub action_roots: Mutex<Vec<PathBuf>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            Self {
                prs_fast: vec![],
                enrichments: vec![],
                enrichment_sequence: Mutex::new(VecDeque::new()),
                merges: Mutex::new(vec![]),
                set_drafts: Mutex::new(vec![]),
                calls: Mutex::new(vec![]),
                fail_set_draft: Mutex::new(false),
                prs_by_root: Mutex::new(HashMap::new()),
                enrichments_by_root: HashMap::new(),
                fail_fast_roots: Mutex::new(HashSet::new()),
                fail_enriched_roots: Mutex::new(HashSet::new()),
                list_roots: Mutex::new(vec![]),
                action_roots: Mutex::new(vec![]),
            }
        }
        /// Queue successive enriched payloads; each call pops the next one.
        pub fn set_enrichment_sequence(&self, seq: Vec<Vec<PrEnrichment>>) {
            *self.enrichment_sequence.lock().unwrap() = seq.into();
        }
    }

    impl GhClient for FakeGh {
        fn list_prs_fast(&self, root: &std::path::Path) -> Result<Vec<Pr>> {
            self.list_roots.lock().unwrap().push(root.to_path_buf());
            if self.fail_fast_roots.lock().unwrap().contains(root) {
                return Err(anyhow!("gh failed in {}", root.display()));
            }
            let by_root = self.prs_by_root.lock().unwrap();
            let rows = by_root.get(root).unwrap_or(&self.prs_fast);
            Ok(rows
                .clone()
                .into_iter()
                .filter(|p| p.state == crate::data::pr::PrState::Open)
                .collect())
        }
        fn list_prs_enriched(&self, root: &std::path::Path) -> Result<Vec<PrEnrichment>> {
            if self.fail_enriched_roots.lock().unwrap().contains(root) {
                return Err(anyhow!("gh failed in {}", root.display()));
            }
            if let Some(rows) = self.enrichments_by_root.get(root) {
                return Ok(rows.clone());
            }
            if let Some(next) = self.enrichment_sequence.lock().unwrap().pop_front() {
                return Ok(next);
            }
            Ok(self.enrichments.clone())
        }
        fn merge_pr(&self, root: &std::path::Path, n: u32, m: &str) -> Result<()> {
            self.action_roots.lock().unwrap().push(root.to_path_buf());
            self.merges.lock().unwrap().push((n, m.to_string()));
            self.calls.lock().unwrap().push(format!("merge {n} {m}"));
            Ok(())
        }
        fn set_pr_draft(&self, root: &std::path::Path, n: u32, draft: bool) -> Result<()> {
            self.action_roots.lock().unwrap().push(root.to_path_buf());
            if *self.fail_set_draft.lock().unwrap() {
                return Err(anyhow!("set draft failed"));
            }
            self.set_drafts.lock().unwrap().push((n, draft));
            self.calls.lock().unwrap().push(format!("ready {n} {draft}"));
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
            repo: "repo".into(),
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: "feature".into(),
            labels: vec![Label { name: "bug".into() }],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }];
        fake.enrichments = vec![PrEnrichment {
            number: 7,
            repo: "repo".into(),
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

    #[test]
    fn fake_drops_non_open_rows_after_parse() {
        use super::fakes::FakeGh;
        use crate::data::pr::{Author, Pr, PrState};
        let mut g = FakeGh::new();
        g.prs_fast = vec![
            Pr { number: 1, repo: "repo".into(), title: "open".into(), is_draft: false, state: PrState::Open,
                 author: Author { login: "a".into() },
                 created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 base_ref_name: "main".into(), head_ref_name: "f".into(),
                 labels: vec![], status_check_rollup: vec![],
                 review_decision: None, mergeable: None },
            Pr { number: 2, repo: "repo".into(), title: "merged".into(), is_draft: false, state: PrState::Merged,
                 author: Author { login: "a".into() },
                 created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                 base_ref_name: "main".into(), head_ref_name: "f2".into(),
                 labels: vec![], status_check_rollup: vec![],
                 review_decision: None, mergeable: None },
        ];
        let got = super::GhClient::list_prs_fast(&g, std::path::Path::new("/x")).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].number, 1);
    }

    #[test]
    fn fake_records_set_pr_draft_calls() {
        use super::GhClient;
        use super::fakes::FakeGh;
        let g = FakeGh::new();
        g.set_pr_draft(std::path::Path::new("/x"), 7, true).unwrap();
        g.set_pr_draft(std::path::Path::new("/x"), 7, false).unwrap();
        let calls = g.set_drafts.lock().unwrap().clone();
        assert_eq!(calls, vec![(7, true), (7, false)]);
    }
}
