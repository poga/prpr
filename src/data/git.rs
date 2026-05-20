//! `git` CLI subprocess wrappers. Same trait pattern as `gh.rs`.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};

use crate::data::pr::{Author, Commit};

pub trait GitClient: Send + Sync {
    /// Resolve the repo root containing `cwd`. Errors if `cwd` is not in a git repo.
    fn repo_root(&self, cwd: &Path) -> Result<std::path::PathBuf>;
    /// Returns `true` if the `origin` (or any) remote points at github.com.
    fn has_github_remote(&self, repo_root: &Path) -> Result<bool>;
    /// Resolve a ref name (`refs/prpr/pr-123`, `origin/main`, …) to its oid.
    /// Used to bypass `gh pr view` once the bulk fetch has primed refs.
    fn rev_parse(&self, repo_root: &Path, refname: &str) -> Result<String>;
    /// List commits in `base..head` (PR-only commits), oldest-first.
    fn log_commits(&self, repo_root: &Path, base: &str, head: &str) -> Result<Vec<Commit>>;
    /// Fetch the given PR numbers' head refs (into `refs/prpr/pr-<n>`)
    /// and refresh `origin/*` heads — all in one git invocation.
    /// RefreshList waits for this before emitting `ListFast`, so any
    /// subsequent `OpenPr` is zero-network.
    fn fetch_pr_refs(&self, repo_root: &Path, numbers: &[u32]) -> Result<()>;
    /// Three-dot diff between `base` and `head` against local refs.
    /// Mirrors `gh pr diff` but is offline once both oids are fetched.
    fn diff(&self, repo_root: &Path, base: &str, head: &str) -> Result<String>;
    /// Run `git blame --porcelain <commit> -- <file>`. Returns raw stdout.
    fn blame(&self, repo_root: &Path, commit: &str, file: &str) -> Result<String>;
    /// Run `git log --reverse -p <base>..<head> -- <file>` with a SHA marker
    /// per commit. Used to attribute deleted lines to the PR commit that
    /// removed them. Returns raw stdout.
    fn log_patches(&self, repo_root: &Path, base: &str, head: &str, file: &str) -> Result<String>;
}

pub struct GitCli;

fn run(cmd: &mut Command) -> Result<Output> {
    let out = cmd
        .output()
        .with_context(|| format!("failed to spawn: {cmd:?}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        return Err(anyhow!("git exited with {}: {}", out.status, stderr.trim()));
    }
    Ok(out)
}

impl GitClient for GitCli {
    fn repo_root(&self, cwd: &Path) -> Result<std::path::PathBuf> {
        let out = run(Command::new("git")
            .current_dir(cwd)
            .args(["rev-parse", "--show-toplevel"]))?;
        let s = String::from_utf8(out.stdout)?.trim().to_string();
        if s.is_empty() {
            Err(anyhow!("git rev-parse returned empty"))
        } else {
            Ok(std::path::PathBuf::from(s))
        }
    }

    fn has_github_remote(&self, repo_root: &Path) -> Result<bool> {
        let out = run(Command::new("git")
            .current_dir(repo_root)
            .args(["remote", "-v"]))?;
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(s.contains("github.com"))
    }

    fn rev_parse(&self, repo_root: &Path, refname: &str) -> Result<String> {
        let out = run(Command::new("git")
            .current_dir(repo_root)
            .args(["rev-parse", refname]))?;
        let s = String::from_utf8(out.stdout)
            .with_context(|| "git rev-parse returned non-UTF-8")?
            .trim()
            .to_string();
        if s.is_empty() {
            Err(anyhow!("git rev-parse returned empty for {refname}"))
        } else {
            Ok(s)
        }
    }

    fn log_commits(&self, repo_root: &Path, base: &str, head: &str) -> Result<Vec<Commit>> {
        // Use \x1f (ASCII unit separator) between fields and \x1e (record
        // separator) between commits — git's pretty format tolerates them
        // and they cannot appear in oid/date/login fields.
        let range = format!("{base}..{head}");
        let out = run(Command::new("git").current_dir(repo_root).args([
            "log",
            "--reverse",
            "--no-color",
            "--pretty=format:%H\x1f%an\x1f%cI\x1f%s\x1e",
            &range,
        ]))?;
        let raw = String::from_utf8(out.stdout)
            .with_context(|| "git log returned non-UTF-8")?;
        let mut commits = Vec::new();
        for record in raw.split('\x1e') {
            let record = record.trim_start_matches('\n');
            if record.is_empty() {
                continue;
            }
            let mut fields = record.splitn(4, '\x1f');
            let oid = fields.next().unwrap_or("").to_string();
            let author = fields.next().unwrap_or("").to_string();
            let date_str = fields.next().unwrap_or("");
            let subject = fields.next().unwrap_or("").to_string();
            if oid.is_empty() {
                continue;
            }
            let committed_date: Option<DateTime<Utc>> = date_str
                .parse::<DateTime<Utc>>()
                .ok();
            commits.push(Commit {
                oid,
                message_headline: subject,
                authors: vec![Author { login: author }],
                committed_date,
            });
        }
        Ok(commits)
    }

    fn fetch_pr_refs(&self, repo_root: &Path, numbers: &[u32]) -> Result<()> {
        // Build one fetch with explicit refspecs for the given PRs plus
        // `origin/*` so the base ref is current. Skipping all-PR refs
        // means closed/merged PRs aren't fetched on every refresh —
        // a big saving on repos with hundreds of historical PRs.
        let mut args: Vec<String> =
            vec!["fetch".into(), "--quiet".into(), "origin".into()];
        for n in numbers {
            args.push(format!("+refs/pull/{n}/head:refs/prpr/pr-{n}"));
        }
        args.push("+refs/heads/*:refs/remotes/origin/*".into());
        run(Command::new("git").current_dir(repo_root).args(&args))?;
        Ok(())
    }

    fn diff(&self, repo_root: &Path, base: &str, head: &str) -> Result<String> {
        let range = format!("{base}...{head}");
        let out = run(Command::new("git").current_dir(repo_root).args([
            "diff", "--no-color", &range,
        ]))?;
        let s = String::from_utf8(out.stdout)
            .with_context(|| "`git diff` produced non-UTF-8 output")?;
        Ok(s)
    }

    fn blame(&self, repo_root: &Path, commit: &str, file: &str) -> Result<String> {
        let out = run(Command::new("git").current_dir(repo_root).args([
            "blame",
            "--porcelain",
            commit,
            "--",
            file,
        ]))?;
        let s = String::from_utf8(out.stdout)?;
        Ok(s)
    }

    fn log_patches(&self, repo_root: &Path, base: &str, head: &str, file: &str) -> Result<String> {
        let range = format!("{base}..{head}");
        let out = run(Command::new("git").current_dir(repo_root).args([
            "log",
            "--reverse",
            "--no-color",
            "--pretty=format:prpr-commit %H",
            "-p",
            &range,
            "--",
            file,
        ]))?;
        let s = String::from_utf8(out.stdout)?;
        Ok(s)
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub struct FakeGit {
        pub root: PathBuf,
        pub has_gh: bool,
        /// Keyed by ref name (e.g. `refs/prpr/pr-7`, `origin/main`) → oid.
        pub refs: HashMap<String, String>,
        /// Keyed by (base, head) → commits list returned by `log_commits`.
        pub commits: HashMap<(String, String), Vec<Commit>>,
        pub blames: HashMap<(String, String), String>,
        /// Keyed by (base, head) → diff output.
        pub diffs: HashMap<(String, String), String>,
        /// Keyed by (base, head, file) → log_patches output. Missing keys
        /// resolve to empty (no PR-commit deletions for that file).
        pub log_patches: HashMap<(String, String, String), String>,
    }

    impl FakeGit {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self {
                root: root.into(),
                has_gh: true,
                refs: HashMap::new(),
                commits: HashMap::new(),
                blames: HashMap::new(),
                diffs: HashMap::new(),
                log_patches: HashMap::new(),
            }
        }
    }

    impl GitClient for FakeGit {
        fn repo_root(&self, _cwd: &Path) -> Result<PathBuf> {
            Ok(self.root.clone())
        }
        fn has_github_remote(&self, _root: &Path) -> Result<bool> {
            Ok(self.has_gh)
        }
        fn rev_parse(&self, _root: &Path, refname: &str) -> Result<String> {
            self.refs
                .get(refname)
                .cloned()
                .ok_or_else(|| anyhow!("no fake ref for {refname}"))
        }
        fn log_commits(&self, _root: &Path, base: &str, head: &str) -> Result<Vec<Commit>> {
            Ok(self
                .commits
                .get(&(base.into(), head.into()))
                .cloned()
                .unwrap_or_default())
        }
        fn fetch_pr_refs(&self, _root: &Path, _numbers: &[u32]) -> Result<()> {
            Ok(())
        }
        fn diff(&self, _root: &Path, base: &str, head: &str) -> Result<String> {
            self.diffs
                .get(&(base.into(), head.into()))
                .cloned()
                .ok_or_else(|| anyhow!("no fake diff for {base}...{head}"))
        }
        fn blame(&self, _root: &Path, c: &str, f: &str) -> Result<String> {
            self.blames
                .get(&(c.into(), f.into()))
                .cloned()
                .ok_or_else(|| anyhow!("no fake blame for {c} {f}"))
        }
        fn log_patches(&self, _root: &Path, base: &str, head: &str, file: &str) -> Result<String> {
            Ok(self
                .log_patches
                .get(&(base.into(), head.into(), file.into()))
                .cloned()
                .unwrap_or_default())
        }
    }
}
