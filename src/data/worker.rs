//! Worker thread + request/response channels.
//!
//! Interactive subprocess work (`git diff`, `git blame`, `gh pr merge`)
//! runs on a single worker thread, FIFO. `RefreshList` is the exception:
//! its gh/network/fetch pipeline runs on detached threads (one for the
//! fast list + ref fetch, one for enrichment) so a slow refresh never
//! delays opening a PR. All `git fetch` calls share one lock.
//!
//! The UI thread sends `Request`s and drains `Response`s every iteration
//! of its event loop. The worker exits cleanly when `Worker` is dropped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;

use crate::data::blame::{Blame, parse_blame};
use crate::data::diff::parse_diff;
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::log_patches::parse_deletions;
use crate::data::pr::Pr;
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
    /// `mark_ready` clears the draft flag first, since a draft can't merge.
    Merge { number: u32, method: String, mark_ready: bool },
    SetDraft { number: u32, draft: bool },
    ListFiles { number: u32, base_ref: String },
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

// Some variants are larger than the others. The channel is low-volume
// per cycle so the size disparity isn't worth boxing for.
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
    /// Refs for the current generation are fetched and conflict-checked.
    /// Carries `(number, "MERGEABLE" | "CONFLICTING")` per open PR git could
    /// merge; refs git can't merge are absent (enrichment fills them).
    ListRefsReady {
        generation: u32,
        result: anyhow::Result<Vec<(u32, String)>>,
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
    SetDraftDone {
        number: u32,
        draft: bool,
        result: Result<()>,
    },
    /// Inline file list emitted in response to `ListFiles`. `number` is the
    /// staleness key — the UI matches it against the current PR before applying.
    ListFiles {
        number: u32,
        result: anyhow::Result<Vec<crate::data::pr::FileMeta>>,
    },
}

/// GitHub computes mergeability lazily; re-poll to resolve "UNKNOWN".
const ENRICH_MAX_ROUNDS: usize = 3;
const ENRICH_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Upper bound on concurrent `git merge-tree` subprocesses.
const MERGE_CHECK_THREADS: usize = 8;

/// Locally computed conflict verdicts for the refs just fetched: GitHub
/// computes `mergeable` lazily and answers UNKNOWN to a cold query. Refs
/// git can't merge are absent from the result (enrichment fills them).
fn local_merge_states(git: &dyn GitClient, repo_root: &Path, prs: &[Pr]) -> Vec<(u32, String)> {
    let targets: Vec<(u32, String, String)> = prs
        .iter()
        .filter(|p| p.state == crate::data::pr::PrState::Open)
        .map(|p| {
            (p.number, format!("origin/{}", p.base_ref_name), format!("refs/prpr/pr-{}", p.number))
        })
        .collect();
    if targets.is_empty() {
        return vec![];
    }
    // ~12ms per PR; serial checks would stall a 200-PR repo for seconds.
    let chunk = targets.len().div_ceil(MERGE_CHECK_THREADS.min(targets.len()));
    thread::scope(|s| {
        let handles: Vec<_> = targets
            .chunks(chunk)
            .map(|c| {
                s.spawn(move || {
                    c.iter()
                        .filter_map(|(n, base, head)| {
                            git.merge_conflicts(repo_root, base, head).ok().map(|conflicting| {
                                let v = if conflicting { "CONFLICTING" } else { "MERGEABLE" };
                                (*n, v.to_string())
                            })
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
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
        Self::spawn_with_retry(repo_root, gh, git, window_size, ENRICH_RETRY_DELAY)
    }

    pub fn spawn_with_retry(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
        enrich_retry_delay: Duration,
    ) -> Self {
        let (req_tx, req_rx) = channel();
        let (res_tx, res_rx) = channel();
        let handle = thread::spawn(move || {
            run_worker(req_rx, res_tx, repo_root, gh, git, window_size, enrich_retry_delay);
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
    enrich_retry_delay: Duration,
) {
    // Serializes all `git fetch` invocations: concurrent fetches of the same
    // ref would race on git's per-ref lock files.
    let fetch_lock = Arc::new(std::sync::Mutex::new(()));
    while let Ok(req) = req_rx.recv() {
        match req {
            Request::RefreshList { generation } => {
                // Two detached threads, neither gating this worker loop:
                //   - fast: list_prs_fast → emit ListFast rows right away,
                //     then fetch_pr_refs and emit ListRefsReady with local
                //     conflict verdicts once refs land.
                //   - enrichment: list_prs_enriched, re-polled while
                //     GitHub's mergeable answer is still UNKNOWN.
                // Both carry `generation`; the UI merges whichever lands.
                let gh_enr = Arc::clone(&gh);
                let repo_enr = repo_root.clone();
                let tx_enr = res_tx.clone();
                let gen_enr = generation;
                let retry_delay = enrich_retry_delay;
                thread::spawn(move || {
                    let mut round = 0usize;
                    loop {
                        let result = gh_enr.list_prs_enriched(&repo_enr);
                        let has_unknown = matches!(
                            &result,
                            Ok(es) if es.iter().any(|e| e.mergeable.as_deref() == Some("UNKNOWN"))
                        );
                        if tx_enr
                            .send(Response::ListEnriched { generation: gen_enr, result })
                            .is_err()
                        {
                            return;
                        }
                        round += 1;
                        if !has_unknown || round >= ENRICH_MAX_ROUNDS {
                            break;
                        }
                        thread::sleep(retry_delay);
                    }
                });

                // The fast pipeline also runs detached so interactive
                // requests (OpenPr, ListFiles) never queue behind network.
                let gh_fast = Arc::clone(&gh);
                let git_fast = Arc::clone(&git);
                let repo_fast = repo_root.clone();
                let tx_fast = res_tx.clone();
                let lock = Arc::clone(&fetch_lock);
                thread::spawn(move || {
                    let _ = tx_fast.send(Response::ListProgress {
                        generation,
                        stage: ListStage::FetchingList,
                    });
                    let prs = match gh_fast.list_prs_fast(&repo_fast) {
                        Ok(p) => p,
                        Err(e) => {
                            let _ = tx_fast.send(Response::ListFast { generation, result: Err(e) });
                            return;
                        }
                    };
                    let open: Vec<u32> = prs
                        .iter()
                        .filter(|p| p.state == crate::data::pr::PrState::Open)
                        .map(|p| p.number)
                        .collect();
                    let bases: Vec<String> = prs
                        .iter()
                        .filter(|p| p.state == crate::data::pr::PrState::Open)
                        .map(|p| p.base_ref_name.clone())
                        .collect();
                    // Rows first: the list draws now; refs and conflict
                    // verdicts stream in behind it.
                    let _ = tx_fast.send(Response::ListFast { generation, result: Ok(prs.clone()) });
                    let _ = tx_fast.send(Response::ListProgress {
                        generation,
                        stage: ListStage::FetchingRefs,
                    });
                    let fetched = {
                        // A poisoned lock must not permanently kill refresh.
                        let _g = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        git_fast.fetch_pr_refs(&repo_fast, &open, &bases)
                    };
                    let result = match fetched {
                        Ok(()) => Ok(local_merge_states(&*git_fast, &repo_fast, &prs)),
                        Err(e) => Err(anyhow::anyhow!("fetching open PR refs: {e:#}")),
                    };
                    let _ = tx_fast.send(Response::ListRefsReady { generation, result });
                });
            }
            Request::OpenPr(pr) => {
                run_open_pr(&*git, &repo_root, &fetch_lock, &res_tx, pr);
            }
            Request::BlameFile { number, head_oid, base_oid, path, commits } => {
                run_blame_file(&*git, &repo_root, &res_tx, number, &head_oid, &base_oid, &path, &commits, window_size);
            }
            Request::Merge { number, method, mark_ready } => {
                // Abort on a failed ready call: the PR is still a draft, so
                // merging on would report a success that never happened.
                let result = if mark_ready {
                    gh.set_pr_draft(&repo_root, number, false)
                        .and_then(|()| gh.merge_pr(&repo_root, number, &method))
                } else {
                    gh.merge_pr(&repo_root, number, &method)
                };
                if res_tx.send(Response::MergeDone { number, result }).is_err() {
                    break;
                }
            }
            Request::SetDraft { number, draft } => {
                let result = gh.set_pr_draft(&repo_root, number, draft);
                if res_tx.send(Response::SetDraftDone { number, draft, result }).is_err() {
                    break;
                }
            }
            Request::ListFiles { number, base_ref } => {
                let head_ref = format!("refs/prpr/pr-{number}");
                let base_ref_full = format!("origin/{base_ref}");
                let result = (|| -> Result<Vec<crate::data::pr::FileMeta>> {
                    let head = git.rev_parse(&repo_root, &head_ref)?;
                    let base = git.rev_parse(&repo_root, &base_ref_full)?;
                    git.diff_numstat(&repo_root, &base, &head)
                })();
                let _ = res_tx.send(Response::ListFiles { number, result });
            }
        }
    }
}

fn run_open_pr(
    git: &dyn GitClient,
    repo_root: &Path,
    fetch_lock: &std::sync::Mutex<()>,
    res_tx: &Sender<Response>,
    pr: crate::data::pr::Pr,
) {
    let number = pr.number;
    let head_ref = format!("refs/prpr/pr-{number}");
    let base_ref = format!("origin/{}", pr.base_ref_name);
    let resolve = |g: &dyn GitClient| -> Result<(String, String)> {
        Ok((g.rev_parse(repo_root, &head_ref)?, g.rev_parse(repo_root, &base_ref)?))
    };
    let (head_oid, base_oid) = match resolve(git) {
        Ok(oids) => oids,
        // Cold start: refs not primed yet. Take the fetch lock (waits out any
        // in-flight bulk fetch), re-check, and fetch just this PR if needed.
        Err(_) => {
            let fetched = {
                // A poisoned lock must not permanently kill OpenPr.
                let _g = fetch_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                resolve(git).or_else(|_| {
                    git.fetch_pr_ref(repo_root, number, &pr.base_ref_name)
                        .and_then(|()| resolve(git))
                })
            };
            match fetched {
                Ok(oids) => oids,
                Err(e) => {
                    let _ = res_tx.send(Response::PrLoadError {
                        number,
                        error: format!("resolving {head_ref} (try `r` to refresh): {e:#}"),
                    });
                    return;
                }
            }
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

#[allow(clippy::too_many_arguments)]
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
    fn open_pr_emits_load_error_when_refs_missing() {
        // FakeGit.refs empty → rev_parse fails → cold-start fallback
        // also can't populate (FakeGit.fetch_pr is a no-op) → PrLoadError.
        let gh = FakeGh::new();
        let git = Arc::new(FakeGit::new("/tmp/repo"));
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), git.clone(), 7);
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
        assert_eq!(
            git.fetched_prs.lock().unwrap().clone(),
            vec![1],
            "missing refs must attempt one on-demand fetch before erroring"
        );
    }

    /// Enter can beat the bulk fetch. A missing ref must trigger a targeted
    /// single-PR fetch and then load normally — not error out.
    #[test]
    fn open_pr_fetches_missing_ref_on_demand_then_loads() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let base_sha = detail.base_ref_oid.clone();
        let number = detail.number;
        let pr = pr_from_fixture(&detail);

        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        // Refs exist only after a fetch — simulates a cold start.
        git.refs_on_fetch.insert(format!("refs/prpr/pr-{number}"), head_sha.clone());
        git.refs_on_fetch.insert(format!("origin/{}", pr.base_ref_name), base_sha.clone());
        git.commits.insert((base_sha.clone(), head_sha.clone()), detail.commits.clone());
        git.diffs.insert(
            (base_sha.clone(), head_sha.clone()),
            include_str!("../../tests/fixtures/diff_basic.patch").to_string(),
        );
        let git = Arc::new(git);
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), git.clone(), 7);
        worker.send(Request::OpenPr(pr));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got_detail = false;
        while std::time::Instant::now() < deadline && !got_detail {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::PrDetail { number: n, result: Ok(_) }) if n == number => {
                    got_detail = true;
                }
                Ok(Response::PrLoadError { error, .. }) => panic!("unexpected error: {error}"),
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(got_detail, "cold-start OpenPr never produced PrDetail");
        assert_eq!(git.fetched_prs.lock().unwrap().clone(), vec![number]);
    }

    /// Rows must not wait for the ref fetch: ListFast comes right after the
    /// FetchingList stage, then FetchingRefs and the terminal ListRefsReady.
    #[test]
    fn refresh_emits_rows_before_ref_fetch_stage() {
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

        let mut events: Vec<String> = vec![];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "ListRefsReady never arrived");
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::ListProgress { generation: 9, stage }) => {
                    events.push(format!("stage:{stage:?}"));
                }
                Ok(Response::ListFast { generation: 9, result: Ok(_) }) => {
                    events.push("fast".into());
                }
                Ok(Response::ListRefsReady { generation: 9, result: Ok(_) }) => {
                    events.push("refs".into());
                    break;
                }
                Ok(Response::ListEnriched { .. }) => {}
                Ok(other) => panic!("unexpected response: {other:?}"),
                Err(_) => {}
            }
        }
        assert_eq!(
            events,
            vec!["stage:FetchingList", "fast", "stage:FetchingRefs", "refs"],
        );
    }

    #[test]
    fn list_files_emits_filemeta_for_resolvable_refs() {
        use crate::data::pr::FileMeta;
        let gh = FakeGh::new();
        let mut git = FakeGit::new("/tmp/repo");
        git.refs.insert("refs/prpr/pr-7".into(), "headoid".into());
        git.refs.insert("origin/main".into(), "baseoid".into());
        git.numstats.insert(
            ("baseoid".into(), "headoid".into()),
            vec![FileMeta { path: "a.rs".into(), additions: 1, deletions: 2 }],
        );
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::ListFiles { number: 7, base_ref: "main".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::ListFiles { number: 7, result: Ok(files) }) => {
                    assert_eq!(files.len(), 1);
                    assert_eq!(files[0].path, "a.rs");
                    return;
                }
                Ok(Response::ListFiles { result: Err(e), .. }) => panic!("unexpected err: {e}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received ListFiles ok");
    }

    #[test]
    fn list_files_emits_error_when_refs_missing() {
        let gh = FakeGh::new();
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::ListFiles { number: 7, base_ref: "main".into() });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::ListFiles { number: 7, result: Err(_) }) => return,
                Ok(Response::ListFiles { result: Ok(_), .. }) => panic!("expected err"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received ListFiles err");
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
                Response::ListRefsReady { generation: 42, .. } => {}
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

    #[test]
    fn set_draft_request_calls_gh_and_reports_done() {
        let gh = FakeGh::new();
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        worker.send(Request::SetDraft { number: 7, draft: true });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::SetDraftDone { number: 7, draft: true, result: Ok(()) }) => return,
                Ok(Response::SetDraftDone { result: Err(e), .. }) => panic!("unexpected err: {e}"),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received SetDraftDone");
    }

    /// Poll until MergeDone lands, asserting its ok-ness; loud on timeout.
    fn await_merge_done(worker: &Worker, expect_ok: bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::MergeDone { result, .. }) => {
                    assert_eq!(result.is_ok(), expect_ok, "MergeDone was {result:?}");
                    return;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("never received MergeDone");
    }

    /// A draft can't be merged, so the ready call must land *before* the
    /// merge — ordering is the contract, not just that both happened.
    #[test]
    fn merge_with_mark_ready_clears_draft_before_merging() {
        let gh = Arc::new(FakeGh::new());
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), gh.clone(), Arc::new(git), 7);
        worker.send(Request::Merge { number: 7, method: "squash".into(), mark_ready: true });

        await_merge_done(&worker, true);
        assert_eq!(
            gh.calls.lock().unwrap().clone(),
            vec!["ready 7 false".to_string(), "merge 7 squash".to_string()],
        );
    }

    #[test]
    fn merge_without_mark_ready_skips_the_ready_call() {
        let gh = Arc::new(FakeGh::new());
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), gh.clone(), Arc::new(git), 7);
        worker.send(Request::Merge { number: 7, method: "merge".into(), mark_ready: false });

        await_merge_done(&worker, true);
        assert_eq!(gh.calls.lock().unwrap().clone(), vec!["merge 7 merge".to_string()]);
    }

    /// A failed ready call leaves the PR a draft; merging on would report a
    /// success that never happened.
    #[test]
    fn merge_aborts_when_marking_ready_fails() {
        let gh = Arc::new(FakeGh::new());
        *gh.fail_set_draft.lock().unwrap() = true;
        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn("/tmp/repo".into(), gh.clone(), Arc::new(git), 7);
        worker.send(Request::Merge { number: 7, method: "squash".into(), mark_ready: true });

        await_merge_done(&worker, false);
        assert!(gh.merges.lock().unwrap().is_empty(), "merge must not be attempted");
    }

    /// GitHub answers UNKNOWN to a cold mergeable query, so the conflict
    /// verdicts must come from local git via ListRefsReady.
    #[test]
    fn refs_ready_carries_locally_computed_conflict_state() {
        use crate::data::pr::{Author, Pr, PrState};

        let mk = |number: u32, head: &str| Pr {
            number,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: head.into(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        };
        let mut gh = FakeGh::new();
        gh.prs_fast = vec![mk(7, "conflicting"), mk(8, "clean")];
        // Cold GitHub: the only thing enrichment can say is "still computing".
        gh.enrichments = vec![];

        let mut git = FakeGit::new("/tmp/repo");
        git.conflicts.insert(("origin/main".into(), "refs/prpr/pr-7".into()), true);
        git.conflicts.insert(("origin/main".into(), "refs/prpr/pr-8".into()), false);
        let worker = Worker::spawn("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);

        worker.send(Request::RefreshList { generation: 1 });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for ListRefsReady");
            match worker.rx.recv_timeout(std::time::Duration::from_millis(500)) {
                Ok(Response::ListRefsReady { generation: 1, result: Ok(states) }) => {
                    let by: std::collections::HashMap<u32, &str> =
                        states.iter().map(|(n, s)| (*n, s.as_str())).collect();
                    assert_eq!(by.get(&7), Some(&"CONFLICTING"));
                    assert_eq!(by.get(&8), Some(&"MERGEABLE"));
                    return;
                }
                Ok(Response::ListRefsReady { result: Err(e), .. }) => panic!("refs failed: {e}"),
                Ok(_) => {}
                Err(_) => {}
            }
        }
    }

    #[test]
    fn enrichment_repolls_until_mergeable_resolves() {
        use crate::data::pr::PrEnrichment;

        let mk_enr = |m: &str| PrEnrichment {
            number: 7,
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: Some(m.into()),
        };
        let gh = FakeGh::new();
        // First enriched fetch is UNKNOWN → worker must re-poll; second resolves.
        gh.set_enrichment_sequence(vec![vec![mk_enr("UNKNOWN")], vec![mk_enr("CONFLICTING")]]);

        let git = FakeGit::new("/tmp/repo");
        let worker = Worker::spawn_with_retry(
            "/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7, std::time::Duration::from_millis(50),
        );
        worker.send(Request::RefreshList { generation: 1 });

        let mut mergeables: Vec<Option<String>> = vec![];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match worker.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                Ok(Response::ListEnriched { generation: 1, result: Ok(es) }) => {
                    mergeables.push(es.first().and_then(|e| e.mergeable.clone()));
                    if mergeables.iter().any(|m| m.as_deref() == Some("CONFLICTING")) { break; }
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert_eq!(
            mergeables.first(), Some(&Some("UNKNOWN".into())),
            "first ListEnriched should carry UNKNOWN; got {mergeables:?}"
        );
        assert!(
            mergeables.iter().any(|m| m.as_deref() == Some("CONFLICTING")),
            "re-poll should eventually deliver CONFLICTING; got {mergeables:?}"
        );
    }
}
