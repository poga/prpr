//! Worker thread + request/response channels.
//!
//! All blocking subprocess work (`gh pr list`, `gh pr view`, `gh pr diff`,
//! `git fetch`, `git blame`, `gh pr merge`) runs on a single worker thread.
//! The UI thread sends `Request`s and drains `Response`s every iteration of
//! its event loop, so the screen stays redraw-able while subprocess calls
//! run.
//!
//! There is exactly one worker. Requests are processed FIFO. The worker
//! exits cleanly when `Worker` is dropped (the request channel closes).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use anyhow::Result;

use crate::data::blame::{Blame, parse_blame};
use crate::data::diff::parse_diff;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::log_patches::parse_deletions;
use crate::render::attribution::{attribute_file, commit_stats_for_file};

#[derive(Debug)]
pub enum Request {
    RefreshList { generation: u32 },
    OpenPr(crate::data::pr::Pr),
    BlameFile {
        number: u32,
        head_oid: String,
        base_oid: String,
        path: String,
        commits: Vec<String>,
    },
    Merge { number: u32, method: String },
}

/// Pipeline stage emitted by the worker while servicing `RefreshList`.
/// Lets the UI replace the generic "loading PRs…" indicator with the
/// step that's currently running, so a slow `gh` or `git fetch` never
/// looks like a hang.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStage {
    /// `gh pr list` is running.
    FetchingList,
    /// `git fetch` for open-PR head refs is running.
    FetchingRefs,
}

impl ListStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::FetchingList => "fetching PR list (gh)",
            Self::FetchingRefs => "fetching branches (git)",
        }
    }
}

// PrPackage-derived variants are larger than the others. The channel
// is low-volume per cycle so the size disparity isn't worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Response {
    /// Emitted before each blocking step of `RefreshList` so the UI can
    /// show what's running. Carries the same `generation` as the
    /// terminal `ListFast` event so stale-cycle updates can be dropped.
    ListProgress {
        generation: u32,
        stage: ListStage,
    },
    ListFast {
        generation: u32,
        result: anyhow::Result<Vec<crate::data::pr::Pr>>,
    },
    ListEnriched {
        generation: u32,
        result: anyhow::Result<Vec<crate::data::pr::PrEnrichment>>,
    },
    /// Granular PR-load events (see worker pipeline).
    PrDetail {
        number: u32,
        result: anyhow::Result<crate::data::pr::PrDetail>,
    },
    PrDiff {
        number: u32,
        result: anyhow::Result<Vec<crate::data::diff::FileDiff>>,
    },
    PrFileColors {
        number: u32,
        head_oid: String,
        path: String,
        colors: crate::render::attribution::LineColors,
        stats: HashMap<String, crate::render::attribution::CommitStats>,
    },
    PrLoadError {
        number: u32,
        error: String,
    },
    MergeDone {
        number: u32,
        result: Result<()>,
    },
}

pub struct Worker {
    /// Wrapped in `Option` so `Drop` can take and drop the sender BEFORE
    /// joining the thread. Otherwise `recv()` in the worker would never
    /// return — the sender it's waiting on is the one we're holding here.
    tx: Option<Sender<Request>>,
    pub rx: Receiver<Response>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
    ) -> Self {
        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        let handle = thread::spawn(move || {
            run_worker(req_rx, res_tx, repo_root, gh, git, window_size);
        });
        Self {
            tx: Some(req_tx),
            rx: res_rx,
            handle: Some(handle),
        }
    }

    /// Send a request to the worker. Silently no-ops if the worker has
    /// already been torn down (channel closed).
    pub fn send(&self, req: Request) {
        if let Some(tx) = self.tx.as_ref() {
            let _ = tx.send(req);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Close the request channel first so the worker's `recv()` returns
        // an error and the loop exits. Then join.
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_worker(
    req_rx: Receiver<Request>,
    res_tx: Sender<Response>,
    repo_root: PathBuf,
    gh: Arc<dyn GhClient>,
    git: Arc<dyn GitClient>,
    window_size: usize,
) {
    while let Ok(req) = req_rx.recv() {
        match req {
            Request::RefreshList { generation } => {
                // The list renders only after every OPEN PR's head ref
                // is locally fetched, so subsequent OpenPr is guaranteed
                // zero-network. Sequence on the worker thread:
                //   1. list_prs_fast — get the rows and their states.
                //   2. fetch_pr_refs for open PR numbers (+ origin/*).
                //   3. emit ListFast.
                // list_prs_enriched is fired on a detached thread so it
                // doesn't gate the fast path; its response carries the
                // same generation and is merged when it arrives.
                let gh_enr = Arc::clone(&gh);
                let repo_enr = repo_root.clone();
                let tx_enr = res_tx.clone();
                let gen_enr = generation;
                thread::spawn(move || {
                    let result = gh_enr.list_prs_enriched(&repo_enr);
                    let _ = tx_enr.send(Response::ListEnriched {
                        generation: gen_enr,
                        result,
                    });
                });

                let _ = res_tx.send(Response::ListProgress {
                    generation,
                    stage: ListStage::FetchingList,
                });
                let combined = match gh.list_prs_fast(&repo_root) {
                    Err(e) => Err(e),
                    Ok(prs) => {
                        let open: Vec<u32> = prs
                            .iter()
                            .filter(|p| p.state == crate::data::pr::PrState::Open)
                            .map(|p| p.number)
                            .collect();
                        let _ = res_tx.send(Response::ListProgress {
                            generation,
                            stage: ListStage::FetchingRefs,
                        });
                        match git.fetch_pr_refs(&repo_root, &open) {
                            Ok(()) => Ok(prs),
                            Err(e) => Err(anyhow::anyhow!("fetching open PR refs: {e:#}")),
                        }
                    }
                };
                let _ = res_tx.send(Response::ListFast {
                    generation,
                    result: combined,
                });
            }
            Request::OpenPr(pr) => {
                run_open_pr(&*gh, &*git, &repo_root, &res_tx, pr);
            }
            Request::BlameFile { number, head_oid, base_oid, path, commits } => {
                run_blame_file(&*git, &repo_root, &res_tx, number, &head_oid, &base_oid, &path, &commits, window_size);
            }
            Request::Merge { number, method } => {
                let result = gh.merge_pr(&repo_root, number, &method);
                if res_tx.send(Response::MergeDone { number, result }).is_err() {
                    break;
                }
            }
        }
    }
}

fn run_open_pr(
    _gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    pr: crate::data::pr::Pr,
) {
    let number = pr.number;
    let head_ref = format!("refs/prpr/pr-{number}");
    let base_ref = format!("origin/{}", pr.base_ref_name);
    let head_oid = match git.rev_parse(repo_root, &head_ref) {
        Ok(o) => o,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("resolving {head_ref} (try `r` to refresh): {e:#}"),
            });
            return;
        }
    };
    let base_oid = match git.rev_parse(repo_root, &base_ref) {
        Ok(o) => o,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("resolving {base_ref}: {e:#}"),
            });
            return;
        }
    };

    let (commits_res, diff_res) = thread::scope(|s| {
        let commits_h = s.spawn(|| git.log_commits(repo_root, &base_oid, &head_oid));
        let diff_h = s.spawn(|| {
            git.diff(repo_root, &base_oid, &head_oid)
                .and_then(|s| parse_diff(&s))
        });
        (commits_h.join().unwrap(), diff_h.join().unwrap())
    });
    let commits = match commits_res {
        Ok(c) => c,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError { number, error: format!("{e:#}") });
            return;
        }
    };
    let files = match diff_res {
        Ok(f) => f,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError { number, error: format!("{e:#}") });
            return;
        }
    };

    let detail = crate::data::pr::PrDetail {
        number: pr.number,
        title: pr.title.clone(),
        is_draft: pr.is_draft,
        state: pr.state,
        author: pr.author.clone(),
        base_ref_name: pr.base_ref_name.clone(),
        base_ref_oid: base_oid.clone(),
        head_ref_name: pr.head_ref_name.clone(),
        head_ref_oid: head_oid.clone(),
        mergeable: pr.mergeable.clone(),
        status_check_rollup: pr.status_check_rollup.clone(),
        review_decision: pr.review_decision,
        commits,
        files: files
            .iter()
            .map(|f| crate::data::pr::FileMeta {
                path: f.path.clone(),
                additions: 0,
                deletions: 0,
            })
            .collect(),
    };
    let _ = res_tx.send(Response::PrDetail { number, result: Ok(detail) });
    let _ = res_tx.send(Response::PrDiff { number, result: Ok(files) });
}

fn run_blame_file(
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    number: u32,
    head_oid: &str,
    base_oid: &str,
    path: &str,
    commits: &[String],
    window_size: usize,
) {
    let head = git
        .blame(repo_root, head_oid, path)
        .map(|s| parse_blame(&s))
        .unwrap_or_else(|_| Blame { line_shas: vec![] });
    let log_out = git
        .log_patches(repo_root, base_oid, head_oid, path)
        .unwrap_or_default();
    let deletes = parse_deletions(&log_out);
    let lc = attribute_file(commits, window_size, &head, &deletes);
    let per = commit_stats_for_file(commits, &head, &deletes);
    let _ = res_tx.send(Response::PrFileColors {
        number,
        head_oid: head_oid.to_string(),
        path: path.to_string(),
        colors: lc,
        stats: per,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gh::fakes::FakeGh;
    use crate::data::git::fakes::FakeGit;
    use crate::data::pr::PrDetail;
    use pretty_assertions::assert_eq;

    fn fixture_detail() -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        serde_json::from_str(json).unwrap()
    }

    fn pr_from_fixture(detail: &crate::data::pr::PrDetail) -> crate::data::pr::Pr {
        crate::data::pr::Pr {
            number: detail.number,
            title: detail.title.clone(),
            is_draft: detail.is_draft,
            state: detail.state,
            author: detail.author.clone(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: detail.base_ref_name.clone(),
            head_ref_name: detail.head_ref_name.clone(),
            labels: vec![],
            status_check_rollup: detail.status_check_rollup.clone(),
            review_decision: detail.review_decision,
            mergeable: detail.mergeable.clone(),
        }
    }

    #[test]
    fn open_pr_emits_only_detail_and_diff_no_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;
        let pr = pr_from_fixture(&detail);

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        git.refs.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs.insert(format!("origin/{}", pr.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::OpenPr(pr));

        let mut got_detail = false;
        let mut got_diff = false;
        let mut color_events = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrDiff { number: n, result: Ok(_) }) if n == number => {
                    got_diff = true;
                }
                Ok(Response::PrFileColors { .. }) => color_events += 1,
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) => {}
                Err(_) => {
                    if got_detail && got_diff { break; }
                }
            }
        }
        assert!(got_detail, "never received PrDetail");
        assert!(got_diff, "never received PrDiff");
        assert_eq!(color_events, 0, "OpenPr must not emit color events");
    }

    #[test]
    fn blame_file_emits_one_pr_file_colors_for_requested_path() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames.insert((head_sha.clone(), "src/sched.rs".into()), porcelain);

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::BlameFile {
            number,
            head_oid: head_sha.clone(),
            base_oid: base_sha.clone(),
            path: "src/sched.rs".into(),
            commits: detail.commits.iter().map(|c| c.oid.clone()).collect(),
        });

        let mut got = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrFileColors { number: n, path, .. }) if n == number => {
                    assert_eq!(path, "src/sched.rs");
                    got += 1;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert_eq!(got, 1, "BlameFile should emit exactly one PrFileColors for the requested path");
    }

    #[test]
    fn load_pr_emits_load_error_when_refs_missing() {
        // FakeGit.refs empty → rev_parse fails → cold-start fallback
        // also can't populate (FakeGit.fetch_pr is a no-op) → PrLoadError.
        let gh = FakeGh::new();
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        let pr = crate::data::pr::Pr {
            number: 1,
            title: "t".into(),
            is_draft: false,
            state: crate::data::pr::PrState::Open,
            author: crate::data::pr::Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: "feature".into(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        };
        worker.send(Request::OpenPr(pr));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut saw_error = false;
        while std::time::Instant::now() < deadline && !saw_error {
            if let Ok(Response::PrLoadError { number: 1, .. }) =
                worker.rx.recv_timeout(std::time::Duration::from_millis(500))
            {
                saw_error = true;
            }
        }
        assert!(saw_error, "did not receive PrLoadError");
    }

    #[test]
    fn refresh_emits_progress_stages_before_list_fast() {
        use crate::data::pr::{Author, Pr, PrState};

        let mut gh = FakeGh::new();
        gh.prs_fast = vec![Pr {
            number: 1,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: "feature".into(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }];
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);

        worker.send(Request::RefreshList { generation: 9 });

        // Collect every progress stage observed before ListFast lands.
        // ListEnriched runs on a detached thread so it may interleave —
        // ignore it here.
        let mut stages: Vec<ListStage> = vec![];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let resp = worker
                .rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("worker stalled");
            match resp {
                Response::ListProgress { generation: 9, stage } => stages.push(stage),
                Response::ListFast { generation: 9, .. } => break,
                Response::ListEnriched { .. } => {}
                other => panic!("unexpected response: {other:?}"),
            }
            assert!(std::time::Instant::now() < deadline, "ListFast never arrived");
        }
        assert_eq!(
            stages,
            vec![ListStage::FetchingList, ListStage::FetchingRefs],
            "expected fetch-list → fetch-refs in order before ListFast"
        );
    }

    #[test]
    fn worker_emits_list_fast_then_enriched_with_matching_gen() {
        use crate::data::pr::{Author, Label, Pr, PrEnrichment, PrState, StatusCheck};

        let mut gh = FakeGh::new();
        gh.prs_fast = vec![Pr {
            number: 7,
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
        gh.enrichments = vec![PrEnrichment {
            number: 7,
            status_check_rollup: vec![StatusCheck {
                status: Some("COMPLETED".into()),
                conclusion: Some("SUCCESS".into()),
            }],
            review_decision: None,
            mergeable: Some("MERGEABLE".into()),
        }];
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);

        worker.send(Request::RefreshList { generation: 42 });

        // `ListEnriched` is fired on a detached thread so it can land
        // anywhere in the stream; `ListProgress` events are emitted
        // before `ListFast`. Track both terminal events and skip the
        // progress noise.
        let mut got_fast = false;
        let mut got_enriched = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !(got_fast && got_enriched) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for ListFast + ListEnriched (got_fast={got_fast}, got_enriched={got_enriched})"
            );
            let resp = worker
                .rx
                .recv_timeout(std::time::Duration::from_millis(500))
                .expect("worker channel closed unexpectedly");
            match resp {
                Response::ListProgress { generation: 42, .. } => {}
                Response::ListFast { generation: 42, result: Ok(prs) } => {
                    assert_eq!(prs.len(), 1);
                    assert_eq!(prs[0].number, 7);
                    got_fast = true;
                }
                Response::ListEnriched { generation: 42, result: Ok(e) } => {
                    assert_eq!(e.len(), 1);
                    assert_eq!(e[0].number, 7);
                    assert_eq!(e[0].status_check_rollup.len(), 1);
                    got_enriched = true;
                }
                other => panic!("unexpected response on generation 42: {other:?}"),
            }
        }
    }
}
