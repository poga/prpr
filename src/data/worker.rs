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

use anyhow::{Context, Result};

use crate::data::blame::{Blame, parse_blame};
use crate::data::cache::PrPackage;
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
                // Streaming pipeline added in Task 7. For now keep the
                // atomic build_package shape so the UI still works:
                let result = build_package(&*gh, &*git, &repo_root, number, window_size);
                match result {
                    Ok(pkg) => {
                        let head = pkg.detail.head_ref_oid.clone();
                        let _ = res_tx.send(Response::PrDetail {
                            number,
                            result: Ok(pkg.detail.clone()),
                        });
                        let _ = res_tx.send(Response::PrDiff {
                            number,
                            result: Ok(pkg.files.clone()),
                        });
                        for (path, lc) in pkg.colors {
                            let _ = res_tx.send(Response::PrFileColors {
                                number,
                                head_oid: head.clone(),
                                path,
                                colors: lc,
                                stats: HashMap::new(),
                            });
                        }
                        let _ = res_tx.send(Response::PrColorsDone {
                            number,
                            head_oid: head,
                        });
                    }
                    Err(e) => {
                        let _ = res_tx.send(Response::PrLoadError {
                            number,
                            error: e.to_string(),
                        });
                    }
                }
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

/// Build the full `PrPackage` (detail + parsed diff + per-line blame colors)
/// for one PR. Pure orchestration: no shared state, safe to call from any
/// thread.
pub fn build_package(
    gh: &dyn GhClient,
    git: &dyn GitClient,
    repo_root: &Path,
    number: u32,
    window_size: usize,
) -> Result<PrPackage> {
    // Phase 1: gh pr view, gh pr diff, and git fetch are all keyed only on
    // `number` and don't depend on each other's output. Run them in parallel.
    let (detail_res, fetch_res, diff_res) = thread::scope(|s| {
        let detail_h = s.spawn(|| gh.view_pr(repo_root, number));
        let fetch_h = s.spawn(|| git.fetch_pr(repo_root, number));
        let diff_h = s.spawn(|| gh.diff_pr(repo_root, number));
        (
            detail_h.join().unwrap(),
            fetch_h.join().unwrap(),
            diff_h.join().unwrap(),
        )
    });
    let detail = detail_res?;
    fetch_res.with_context(|| format!("fetching PR #{number}"))?;
    let raw = diff_res?;
    let files = parse_diff(&raw)?;

    let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();
    let mut commit_stats: HashMap<String, CommitStats> = commits
        .iter()
        .map(|oid| (oid.clone(), CommitStats::default()))
        .collect();

    // Phase 2: per-file blame + log_patches are independent across files.
    // Fan out to a bounded pool of workers that pull file indexes off an
    // atomic counter — gives work-stealing behavior without dragging in a
    // dep, and caps the number of concurrent git subprocesses on huge PRs.
    let per_file = parallel_per_file(
        git,
        repo_root,
        &files,
        &commits,
        &detail.head_ref_oid,
        &detail.base_ref_oid,
        window_size,
    );

    let mut colors = HashMap::new();
    for (path, lc, per) in per_file {
        colors.insert(path, lc);
        for (oid, s) in per {
            let entry = commit_stats.entry(oid).or_default();
            entry.adds += s.adds;
            entry.dels += s.dels;
        }
    }

    Ok(PrPackage {
        detail,
        files,
        colors,
        commit_stats,
    })
}

type FileResult = (String, LineColors, HashMap<String, CommitStats>);

fn parallel_per_file(
    git: &dyn GitClient,
    repo_root: &Path,
    files: &[crate::data::diff::FileDiff],
    commits: &[String],
    head_oid: &str,
    base_oid: &str,
    window_size: usize,
) -> Vec<FileResult> {
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }
    let n_workers = thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .min(n);
    let next_idx = AtomicUsize::new(0);

    thread::scope(|s| {
        let workers: Vec<_> = (0..n_workers)
            .map(|_| {
                s.spawn(|| {
                    let mut local: Vec<FileResult> = Vec::new();
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let f = &files[i];
                        if f.binary {
                            continue;
                        }
                        let head = git
                            .blame(repo_root, head_oid, &f.path)
                            .map(|s| parse_blame(&s))
                            .unwrap_or_else(|_| Blame { line_shas: vec![] });
                        // Walk the PR commits' patches to find which commit's
                        // diff contained each removed line. Deleted lines used
                        // to be blamed against the base commit, which always
                        // resolved to pre-PR commits (i.e., gray) — never the
                        // PR commit that actually did the deletion.
                        let log_out = git
                            .log_patches(repo_root, base_oid, head_oid, &f.path)
                            .unwrap_or_default();
                        let deletes = parse_deletions(&log_out);
                        let lc = attribute_file(commits, window_size, &head, &deletes);
                        let per = commit_stats_for_file(commits, &head, &deletes);
                        local.push((f.path.clone(), lc, per));
                    }
                    local
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    })
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
    fn build_package_assembles_diff_and_colors() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();

        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(
            detail.number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha, "src/sched.rs".into()), porcelain.clone());
        git.blames.insert(
            (detail.base_ref_oid.clone(), "src/sched.rs".into()),
            porcelain,
        );

        let pkg = build_package(&gh, &git, Path::new("/tmp/repo"), detail.number, 7).unwrap();
        assert_eq!(pkg.files.len(), 2);
        assert!(pkg.colors.contains_key("src/sched.rs"));
    }

    #[test]
    fn build_package_populates_commit_stats() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();

        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(
            detail.number,
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );

        let mut git = FakeGit::new("/tmp/repo");
        let porcelain = include_str!("../../tests/fixtures/blame_porcelain.txt").to_string();
        git.blames
            .insert((head_sha, "src/sched.rs".into()), porcelain);

        let pkg = build_package(&gh, &git, Path::new("/tmp/repo"), detail.number, 7).unwrap();

        // Every PR commit gets an entry, even if it didn't touch any tracked file.
        for c in &detail.commits {
            assert!(
                pkg.commit_stats.contains_key(&c.oid),
                "missing stats entry for commit {}",
                c.oid,
            );
        }
        // Sanity: at least one commit has nonzero adds (the basic fixture
        // includes head-blame entries).
        assert!(
            pkg.commit_stats.values().any(|s| s.adds > 0),
            "expected at least one commit with adds > 0",
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
