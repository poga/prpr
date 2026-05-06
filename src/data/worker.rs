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

use anyhow::{Context, Result};

use crate::data::blame::{Blame, parse_blame};
use crate::data::cache::PrPackage;
use crate::data::diff::parse_diff;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::pr::Pr;
use crate::render::attribution::attribute_file;

#[derive(Debug)]
pub enum Request {
    /// Refresh the PR list.
    RefreshList,
    /// Build the PrPackage for one PR (view + diff + blame + attribution).
    LoadPr(u32),
    /// Run `gh pr merge <number> --<method>`.
    Merge { number: u32, method: String },
}

// PrPackage is much larger than the other variants. The channel is
// low-volume (one response per UI action), so the size disparity isn't
// worth boxing for.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Response {
    ListLoaded(Result<Vec<Pr>>),
    PrLoaded {
        number: u32,
        result: Result<PrPackage>,
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
        let response = match req {
            Request::RefreshList => Response::ListLoaded(gh.list_prs(&repo_root)),
            Request::LoadPr(number) => {
                let result = build_package(&*gh, &*git, &repo_root, number, window_size);
                Response::PrLoaded { number, result }
            }
            Request::Merge { number, method } => {
                let result = gh.merge_pr(&repo_root, number, &method);
                Response::MergeDone { number, result }
            }
        };
        if res_tx.send(response).is_err() {
            // UI dropped the receiver; nothing left to do.
            break;
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
    let detail = gh.view_pr(repo_root, number)?;
    git.fetch_pr(repo_root, number)
        .with_context(|| format!("fetching PR #{number}"))?;
    let raw = gh.diff_pr(repo_root, number)?;
    let files = parse_diff(&raw)?;

    let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();
    let mut colors = HashMap::new();
    for f in &files {
        if f.binary {
            continue;
        }
        let head = git
            .blame(repo_root, &detail.head_ref_oid, &f.path)
            .map(|s| parse_blame(&s))
            .unwrap_or_else(|_| Blame { line_shas: vec![] });
        let base = git
            .blame(repo_root, &detail.base_ref_oid, &f.path)
            .map(|s| parse_blame(&s))
            .unwrap_or_else(|_| Blame { line_shas: vec![] });
        let lc = attribute_file(&commits, window_size, &head, &base);
        colors.insert(f.path.clone(), lc);
    }

    Ok(PrPackage {
        detail,
        files,
        colors,
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
    fn worker_round_trip() {
        // Spawn the worker, send a refresh, receive the result.
        let mut gh = FakeGh::new();
        gh.prs = {
            let json = include_str!("../../tests/fixtures/pr_list.json");
            serde_json::from_str(json).unwrap()
        };
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);

        worker.send(Request::RefreshList);
        let resp = worker.rx.recv().unwrap();
        match resp {
            Response::ListLoaded(Ok(prs)) => {
                assert_eq!(prs.len(), 2);
                assert_eq!(prs[0].number, 482);
            }
            other => panic!("unexpected response: {:?}", other),
        }
    }
}
