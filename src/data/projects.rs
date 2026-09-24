//! Which clones prpr is watching. Single-repo mode holds one `Repo` (the
//! clone containing cwd); `--projects` mode holds every GitHub clone that
//! is a direct child of cwd.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use anyhow::{Result, anyhow};

use crate::data::git::GitClient;

/// One clone. `name` is the folder name and doubles as the PR identity
/// namespace; `root` is where every `git`/`gh` subprocess for it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub name: String,
    pub root: PathBuf,
}

impl Repo {
    pub fn from_root(root: PathBuf) -> Self {
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self { name, root }
    }
}

/// Upper bound on concurrent `gh`/`git` subprocesses across clones.
pub const PARALLEL_REPOS: usize = 4;

/// Every direct child of `dir` that is a git repo with a github.com remote,
/// sorted by name. Symlinked directories are followed; hidden ones and
/// plain folders are skipped without comment.
pub fn discover(dir: &Path, git: &dyn GitClient) -> Result<Vec<Repo>> {
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| anyhow!("reading {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let hidden = p
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(true);
            !hidden && p.is_dir() && p.join(".git").exists()
        })
        .collect();
    candidates.sort();
    let checked = par_map(candidates, PARALLEL_REPOS, |p| {
        let ok = git.has_github_remote(&p).unwrap_or(false);
        (p, ok)
    });
    Ok(checked
        .into_iter()
        .filter(|(_, ok)| *ok)
        .map(|(p, _)| Repo::from_root(p))
        .collect())
}

/// Apply `f` to every item on at most `cap` threads. Results keep the
/// input order.
pub fn par_map<T, R, F>(items: Vec<T>, cap: usize, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let n = items.len();
    if n == 0 {
        return vec![];
    }
    let slots: Vec<Mutex<Option<T>>> = items.into_iter().map(|t| Mutex::new(Some(t))).collect();
    let out: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = cap.clamp(1, n);
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= n {
                        break;
                    }
                    let item = slots[i].lock().unwrap().take().unwrap();
                    *out[i].lock().unwrap() = Some(f(item));
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::git::GitCli;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let st = Command::new("git").current_dir(dir).args(args).status().unwrap();
        assert!(st.success(), "git {args:?} failed in {}", dir.display());
    }

    fn make_clone(parent: &Path, name: &str, remote: Option<&str>) -> PathBuf {
        let p = parent.join(name);
        std::fs::create_dir_all(&p).unwrap();
        git(&p, &["init", "-q"]);
        if let Some(url) = remote {
            git(&p, &["remote", "add", "origin", url]);
        }
        p
    }

    #[test]
    fn discover_keeps_only_github_clones_that_are_direct_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_clone(root, "beta", Some("git@github.com:o/beta.git"));
        make_clone(root, "alpha", Some("https://github.com/o/alpha"));
        make_clone(root, "gitlab", Some("https://gitlab.com/o/x"));
        make_clone(root, "noremote", None);
        make_clone(root, ".hidden", Some("https://github.com/o/h"));
        std::fs::create_dir_all(root.join("plain")).unwrap();
        // A clone one level deeper must not count.
        make_clone(&root.join("plain"), "nested", Some("https://github.com/o/n"));
        std::fs::write(root.join("file.txt"), "x").unwrap();

        let repos = discover(root, &GitCli).unwrap();
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        assert_eq!(repos[0].root, root.join("alpha"));
    }

    #[test]
    fn discover_follows_symlinked_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let real = make_clone(elsewhere.path(), "real", Some("https://github.com/o/real"));
        std::os::unix::fs::symlink(&real, tmp.path().join("linked")).unwrap();

        let repos = discover(tmp.path(), &GitCli).unwrap();
        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["linked"]);
    }

    #[test]
    fn par_map_preserves_order_and_bounds_concurrency() {
        use std::sync::atomic::AtomicUsize;
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let out = par_map((0..20).collect(), 3, |i: usize| {
            let a = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(a, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(5));
            active.fetch_sub(1, Ordering::SeqCst);
            i * 2
        });
        assert_eq!(out, (0..20).map(|i| i * 2).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3, "peak {}", peak.load(Ordering::SeqCst));
    }
}
