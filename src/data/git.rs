//! `git` CLI subprocess wrappers. Same trait pattern as `gh.rs`.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, anyhow};

pub trait GitClient: Send + Sync {
    /// Resolve the repo root containing `cwd`. Errors if `cwd` is not in a git repo.
    fn repo_root(&self, cwd: &Path) -> Result<std::path::PathBuf>;
    /// Returns `true` if the `origin` (or any) remote points at github.com.
    fn has_github_remote(&self, repo_root: &Path) -> Result<bool>;
    /// Fetch `refs/pull/<num>/head` so `head_oid` is locally available.
    fn fetch_pr(&self, repo_root: &Path, number: u32) -> Result<()>;
    /// Run `git blame --porcelain <commit> -- <file>`. Returns raw stdout.
    fn blame(&self, repo_root: &Path, commit: &str, file: &str) -> Result<String>;
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

    fn fetch_pr(&self, repo_root: &Path, number: u32) -> Result<()> {
        let refspec = format!("+refs/pull/{number}/head:refs/pprr/pr-{number}");
        run(Command::new("git")
            .current_dir(repo_root)
            .args(["fetch", "--quiet", "origin", &refspec]))?;
        Ok(())
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
}

#[cfg(test)]
pub(crate) mod fakes {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    pub struct FakeGit {
        pub root: PathBuf,
        pub has_gh: bool,
        pub blames: HashMap<(String, String), String>,
    }

    impl FakeGit {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self {
                root: root.into(),
                has_gh: true,
                blames: HashMap::new(),
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
        fn fetch_pr(&self, _root: &Path, _n: u32) -> Result<()> {
            Ok(())
        }
        fn blame(&self, _root: &Path, c: &str, f: &str) -> Result<String> {
            self.blames
                .get(&(c.into(), f.into()))
                .cloned()
                .ok_or_else(|| anyhow!("no fake blame for {c} {f}"))
        }
    }
}
