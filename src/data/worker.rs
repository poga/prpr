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
    /// Build the streaming PR data set for one PR.
    LoadPr(u32),
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
                let fast = gh.list_prs_fast(&repo_root);
                if res_tx
                    .send(Response::ListFast { generation, result: fast })
                    .is_err()
                {
                    break;
                }
                let enriched = gh.list_prs_enriched(&repo_root);
                if res_tx
                    .send(Response::ListEnriched {
                        generation,
                        result: enriched,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Request::LoadPr(number) => {
                run_load(&*gh, &*git, &repo_root, &res_tx, number, window_size);
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
    gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    res_tx: &Sender<Response>,
    number: u32,
    window_size: usize,
) {
    // Stage 1: kick off view, diff, fetch in parallel; emit detail and
    // diff events as they complete.
    let (detail_res, files_res, fetch_res) = thread::scope(|s| {
        let view_tx = res_tx.clone();
        let diff_tx = res_tx.clone();
        let detail_h = s.spawn(move || {
            let r = gh.view_pr(repo_root, number);
            if let Ok(d) = &r {
                let _ = view_tx.send(Response::PrDetail {
                    number,
                    result: Ok(d.clone()),
                });
            }
            // On error, run_load surfaces PrLoadError after join; no need
            // to emit a redundant PrDetail{Err}.
            r
        });
        let diff_h = s.spawn(move || {
            let raw = gh.diff_pr(repo_root, number);
            let parsed = raw.and_then(|s| parse_diff(&s));
            if let Ok(f) = &parsed {
                let _ = diff_tx.send(Response::PrDiff {
                    number,
                    result: Ok(f.clone()),
                });
            }
            parsed
        });
        let fetch_h = s.spawn(|| git.fetch_pr(repo_root, number));
        (detail_h.join().unwrap(), diff_h.join().unwrap(), fetch_h.join().unwrap())
    });

    // If any prerequisite failed, surface PrLoadError and stop. The
    // already-emitted PrDetail/PrDiff (if any) is harmless — the cache
    // ignores stragglers.
    let detail = match detail_res {
        Ok(d) => d,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("{e:#}"),
            });
            return;
        }
    };
    let files = match files_res {
        Ok(f) => f,
        Err(e) => {
            let _ = res_tx.send(Response::PrLoadError {
                number,
                error: format!("{e:#}"),
            });
            return;
        }
    };
    if let Err(e) = fetch_res {
        let _ = res_tx.send(Response::PrLoadError {
            number,
            error: format!("fetching PR #{number}: {e}"),
        });
        return;
    }

    let head_oid = detail.head_ref_oid.clone();
    let base_oid = detail.base_ref_oid.clone();
    let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();

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

    #[test]
    fn load_pr_streams_detail_diff_then_per_file_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let number = detail.number;

        let mut gh = FakeGh::new();
        gh.views.insert(number, detail.clone());
        gh.diffs.insert(
            number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames
            .insert((head_sha.clone(), "README.md".into()), porcelain);

        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::LoadPr(number));

        // Drain everything until PrColorsDone, with a deadline so the test
        // can't hang. Order between PrDetail and PrDiff is "whoever finishes
        // first" but in the fake both are instant, so we accept either
        // order. We assert by counting.
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
    fn load_pr_emits_load_error_when_view_fails() {
        let mut gh = FakeGh::new();
        // No fixture inserted → fake returns an error.
        gh.diffs.insert(
            1,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::LoadPr(1));

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
