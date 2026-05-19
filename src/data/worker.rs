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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use anyhow::Result;

use crate::data::blame::{Blame, parse_blame};
use crate::data::diff::parse_diff;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::log_patches::parse_deletions;
use crate::render::attribution::{CommitStats, LineColors, attribute_file, commit_stats_for_file};

#[derive(Debug)]
pub enum Request {
    /// Refresh the PR list. `generation` is echoed in both responses so the UI
    /// can drop stale results from a superseded refresh cycle.
    RefreshList { generation: u32 },
    /// Build the streaming PR data set for one PR. Carries the cached
    /// `Pr` row so the worker can compute everything (oids, commits,
    /// diff) from local git refs — no `gh pr view` round trip.
    LoadPr(crate::data::pr::Pr),
    /// Run `gh pr merge <number> --<method>`.
    Merge { number: u32, method: String },
}

// PrPackage-derived variants are larger than the others. The channel
// is low-volume per cycle so the size disparity isn't worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Response {
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
    PrColorsDone {
        number: u32,
        head_oid: String,
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
                // Run fast + enriched in parallel; each thread emits its
                // own response as soon as it completes, so rows appear
                // immediately while the heavier enrichment is still in
                // flight. Also kick off a bulk fetch of every PR's head
                // ref so subsequent diff/blame can be served from local
                // refs without per-PR network round trips.
                let gh_ref = &*gh;
                let git_ref = &*git;
                let repo_ref = &repo_root;
                thread::scope(|s| {
                    let tx1 = res_tx.clone();
                    s.spawn(move || {
                        let result = gh_ref.list_prs_fast(repo_ref);
                        let _ = tx1.send(Response::ListFast { generation, result });
                    });
                    let tx2 = res_tx.clone();
                    s.spawn(move || {
                        let result = gh_ref.list_prs_enriched(repo_ref);
                        let _ = tx2.send(Response::ListEnriched { generation, result });
                    });
                    s.spawn(move || {
                        // Fire-and-forget: failure (network, auth) just
                        // means subsequent LoadPr falls back to fetch_pr.
                        let _ = git_ref.fetch_all_prs(repo_ref);
                    });
                });
            }
            Request::LoadPr(pr) => {
                run_load(&*gh, &*git, &repo_root, &res_tx, pr, window_size);
            }
            Request::Merge { number, method } => {
                let result = gh.merge_pr(&repo_root, number, &method);
                if res_tx
                    .send(Response::MergeDone { number, result })
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn run_load(
    _gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    pr: crate::data::pr::Pr,
    window_size: usize,
) {
    let number = pr.number;

    // Stage 1: resolve head/base from local refs. The bulk fetch from
    // the most recent RefreshList primes these; on a cold start where
    // it hasn't completed yet, fall back to a single-PR fetch.
    let head_ref = format!("refs/prpr/pr-{number}");
    let base_ref = format!("origin/{}", pr.base_ref_name);
    let head_oid = match git.rev_parse(repo_root, &head_ref) {
        Ok(o) => o,
        Err(_) => {
            // Cold-start fallback. Single fetch, then retry.
            if let Err(e) = git.fetch_pr(repo_root, number) {
                let _ = res_tx.send(Response::PrLoadError {
                    number,
                    error: format!("fetching PR #{number}: {e:#}"),
                });
                return;
            }
            match git.rev_parse(repo_root, &head_ref) {
                Ok(o) => o,
                Err(e) => {
                    let _ = res_tx.send(Response::PrLoadError {
                        number,
                        error: format!("resolving {head_ref}: {e:#}"),
                    });
                    return;
                }
            }
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

    // Stage 2: pull commits + diff in parallel (both local).
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
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("{e:#}"),
            });
            return;
        }
    };
    let files = match diff_res {
        Ok(f) => f,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("{e:#}"),
            });
            return;
        }
    };

    // Build PrDetail from cached Pr + locally-derived data. No gh round
    // trip — `gh pr view` is no longer in the hot path.
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
        commits: commits.clone(),
        files: files
            .iter()
            .map(|f| crate::data::pr::FileMeta {
                path: f.path.clone(),
                additions: 0,
                deletions: 0,
            })
            .collect(),
    };
    let _ = res_tx.send(Response::PrDetail {
        number,
        result: Ok(detail),
    });
    let _ = res_tx.send(Response::PrDiff {
        number,
        result: Ok(files.clone()),
    });

    let commits: Vec<String> = commits.iter().map(|c| c.oid.clone()).collect();

    // Stage 2: blame files[0] synchronously and emit its colors first.
    if let Some(f) = files.first() {
        if !f.binary {
            let (lc, per) = blame_file(git, repo_root, &commits, &head_oid, &base_oid, f, window_size);
            let _ = res_tx.send(Response::PrFileColors {
                number,
                head_oid: head_oid.clone(),
                path: f.path.clone(),
                colors: lc,
                stats: per,
            });
        }
    }

    // Stage 3: parallel pool for the remainder.
    let remainder: Vec<&crate::data::diff::FileDiff> = files.iter().skip(1).filter(|f| !f.binary).collect();
    let n = remainder.len();
    if n > 0 {
        let n_workers = thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4)
            .min(n);
        let next_idx = AtomicUsize::new(0);
        thread::scope(|s| {
            for _ in 0..n_workers {
                let tx = res_tx.clone();
                let head_oid = head_oid.clone();
                let base_oid = base_oid.clone();
                let commits = commits.clone();
                let remainder = &remainder;
                let next_idx = &next_idx;
                s.spawn(move || {
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= remainder.len() {
                            break;
                        }
                        let f = remainder[i];
                        let (lc, per) = blame_file(
                            git, repo_root, &commits, &head_oid, &base_oid, f, window_size,
                        );
                        let _ = tx.send(Response::PrFileColors {
                            number,
                            head_oid: head_oid.clone(),
                            path: f.path.clone(),
                            colors: lc,
                            stats: per,
                        });
                    }
                });
            }
        });
    }

    let _ = res_tx.send(Response::PrColorsDone {
        number,
        head_oid,
    });
}

/// Blame + log-patches for one file. Returns the file's `LineColors`
/// and its per-commit `CommitStats` contribution.
fn blame_file(
    git: &dyn GitClient,
    repo_root: &Path,
    commits: &[String],
    head_oid: &str,
    base_oid: &str,
    f: &crate::data::diff::FileDiff,
    window_size: usize,
) -> (LineColors, HashMap<String, CommitStats>) {
    let head = git
        .blame(repo_root, head_oid, &f.path)
        .map(|s| parse_blame(&s))
        .unwrap_or_else(|_| Blame { line_shas: vec![] });
    let log_out = git
        .log_patches(repo_root, base_oid, head_oid, &f.path)
        .unwrap_or_default();
    let deletes = parse_deletions(&log_out);
    let lc = attribute_file(commits, window_size, &head, &deletes);
    let per = commit_stats_for_file(commits, &head, &deletes);
    (lc, per)
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
    fn load_pr_streams_detail_diff_then_per_file_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;
        let pr = pr_from_fixture(&detail);

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        git.refs
            .insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs
            .insert(format!("origin/{}", pr.base_ref_name), base_sha.clone());
        git.commits
            .insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames
            .insert((head_sha.clone(), "README.md".into()), porcelain);

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::LoadPr(pr));

        // Drain everything until PrColorsDone, with a deadline so the test
        // can't hang.
        let mut got_detail = false;
        let mut got_diff = false;
        let mut color_paths: Vec<String> = vec![];
        let mut done = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !done {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrDiff { number: n, result: Ok(_) }) if n == number => {
                    got_diff = true;
                }
                Ok(Response::PrFileColors {
                    number: n,
                    head_oid,
                    path,
                    ..
                }) if n == number => {
                    assert_eq!(head_oid, head_sha);
                    color_paths.push(path);
                }
                Ok(Response::PrColorsDone { number: n, .. }) if n == number => {
                    done = true;
                }
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) | Err(_) => {}
            }
        }
        assert!(got_detail, "never received PrDetail");
        assert!(got_diff, "never received PrDiff");
        assert!(done, "never received PrColorsDone");
        // First color event is for files[0] (the visible file).
        assert_eq!(color_paths.first().map(String::as_str), Some("src/sched.rs"));
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
        worker.send(Request::LoadPr(pr));

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

        let resp1 = worker.rx.recv().unwrap();
        match resp1 {
            Response::ListFast { generation: 42, result: Ok(prs) } => {
                assert_eq!(prs.len(), 1);
                assert_eq!(prs[0].number, 7);
            }
            other => panic!("expected ListFast{{generation:42}}, got {:?}", other),
        }

        let resp2 = worker.rx.recv().unwrap();
        match resp2 {
            Response::ListEnriched { generation: 42, result: Ok(e) } => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].number, 7);
                assert_eq!(e[0].status_check_rollup.len(), 1);
            }
            other => panic!("expected ListEnriched{{generation:42}}, got {:?}", other),
        }
    }
}
