//! In-memory cache. The cache is the only consumer of `GhClient` / `GitClient`;
//! views consume already-parsed data from here.
//!
//! Concurrency: callers are expected to wrap this in `Arc<Mutex<Cache>>` if
//! shared between threads. The cache itself is `Send` but not `Sync`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::data::blame::{Blame, parse_blame};
use crate::data::diff::{FileDiff, parse_diff};
use crate::data::gh::GhClient;
use crate::data::git::GitClient;
use crate::data::pr::{Pr, PrDetail};
use crate::render::attribution::{LineColors, attribute_file};

#[derive(Debug, Clone)]
pub struct PrPackage {
    pub detail: PrDetail,
    pub files: Vec<FileDiff>,
    /// Indexed by file path.
    pub colors: HashMap<String, LineColors>,
}

pub struct Cache {
    repo_root: PathBuf,
    gh: Arc<dyn GhClient>,
    git: Arc<dyn GitClient>,
    window_size: usize,

    pub list: Option<Vec<Pr>>,
    /// Key = (pr_number, head_sha).
    packages: HashMap<(u32, String), PrPackage>,
}

impl Cache {
    pub fn new(
        repo_root: PathBuf,
        gh: Arc<dyn GhClient>,
        git: Arc<dyn GitClient>,
        window_size: usize,
    ) -> Self {
        Self {
            repo_root,
            gh,
            git,
            window_size,
            list: None,
            packages: HashMap::new(),
        }
    }

    /// Refresh the PR list (always re-fetches).
    pub fn refresh_list(&mut self) -> Result<&[Pr]> {
        let prs = self.gh.list_prs(&self.repo_root)?;
        self.list = Some(prs);
        Ok(self.list.as_deref().unwrap())
    }

    /// Load a PR. If we already have a cached package for the same `head_sha`,
    /// return it. Otherwise fetch & build.
    pub fn load_pr(&mut self, number: u32) -> Result<&PrPackage> {
        let detail = self.gh.view_pr(&self.repo_root, number)?;
        let key = (number, detail.head_ref_oid.clone());

        if !self.packages.contains_key(&key) {
            let pkg = self.build_package(detail)?;
            self.packages.insert(key.clone(), pkg);
        }
        Ok(self.packages.get(&key).unwrap())
    }

    /// Look up a cached package by number (does not fetch). Returns the
    /// most recently-cached entry for that number, regardless of head_sha.
    pub fn get(&self, number: u32) -> Option<&PrPackage> {
        self.packages
            .iter()
            .find(|((n, _), _)| *n == number)
            .map(|(_, v)| v)
    }

    fn build_package(&self, detail: PrDetail) -> Result<PrPackage> {
        // 1. Make sure the PR refs are local.
        self.git
            .fetch_pr(&self.repo_root, detail.number)
            .with_context(|| format!("fetching PR #{}", detail.number))?;

        // 2. Pull the unified diff and parse it.
        let raw = self.gh.diff_pr(&self.repo_root, detail.number)?;
        let files = parse_diff(&raw)?;

        // 3. For each text file, run blame on head and on base.
        let commits: Vec<String> = detail.commits.iter().map(|c| c.oid.clone()).collect();
        let mut colors: HashMap<String, LineColors> = HashMap::new();
        for f in &files {
            if f.binary {
                continue;
            }
            let head = self
                .git
                .blame(&self.repo_root, &detail.head_ref_oid, &f.path)
                .map(|s| parse_blame(&s))
                .unwrap_or_else(|_| Blame { line_shas: vec![] });
            let base = self
                .git
                .blame(&self.repo_root, &detail.base_ref_oid, &f.path)
                .map(|s| parse_blame(&s))
                .unwrap_or_else(|_| Blame { line_shas: vec![] });
            let lc = attribute_file(&commits, self.window_size, &head, &base);
            colors.insert(f.path.clone(), lc);
        }

        Ok(PrPackage {
            detail,
            files,
            colors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::gh::fakes::FakeGh;
    use crate::data::git::fakes::FakeGit;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    fn fixture_pr() -> Pr {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        prs.into_iter().next().unwrap()
    }

    fn fixture_detail() -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn refresh_list_populates_cache() {
        let mut gh = FakeGh::new();
        gh.prs = vec![fixture_pr()];
        let git = FakeGit::new("/tmp/repo");
        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        let prs = cache.refresh_list().unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 482);
    }

    #[test]
    fn load_pr_builds_a_package() {
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
            .insert((head_sha.clone(), "src/sched.rs".into()), porcelain.clone());
        git.blames.insert(
            (detail.base_ref_oid.clone(), "src/sched.rs".into()),
            porcelain,
        );
        // README.md has no blame fixture — cache should tolerate missing blame.

        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        let pkg = cache.load_pr(detail.number).unwrap();
        assert_eq!(pkg.files.len(), 2);
        assert!(pkg.colors.contains_key("src/sched.rs"));
    }

    #[test]
    fn force_push_changes_cache_key() {
        // Two separate caches simulate before/after force-push: each gets its
        // own gh fake whose view_pr returns a different head_ref_oid. The
        // entry key is `(number, head_sha)`, so force-push lands in a fresh slot.
        let mut detail = fixture_detail();

        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(detail.number, "".into());
        let git = FakeGit::new("/tmp/repo");
        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        cache.load_pr(detail.number).unwrap();
        assert_eq!(cache.packages.len(), 1);

        // Simulate a force-push by mutating head_ref_oid and rebuilding the cache.
        detail.head_ref_oid = "ffffffffffffffffffffffffffffffffffffffff".into();
        let mut gh2 = FakeGh::new();
        gh2.views.insert(detail.number, detail.clone());
        gh2.diffs.insert(detail.number, "".into());
        let git2 = FakeGit::new("/tmp/repo");
        let mut cache2 = Cache::new("/tmp/repo".into(), Arc::new(gh2), Arc::new(git2), 7);
        cache2.load_pr(detail.number).unwrap();
        assert_eq!(cache2.packages.len(), 1);
        let key = cache2.packages.keys().next().unwrap();
        assert_eq!(key.1, "ffffffffffffffffffffffffffffffffffffffff");
    }

    #[test]
    fn get_returns_cached_package() {
        let detail = fixture_detail();
        let head_sha = detail.head_ref_oid.clone();
        let mut gh = FakeGh::new();
        gh.views.insert(detail.number, detail.clone());
        gh.diffs.insert(detail.number, "".into());
        let mut git = FakeGit::new("/tmp/repo");
        git.blames
            .insert((head_sha, "src/sched.rs".into()), String::new());
        let mut cache = Cache::new("/tmp/repo".into(), Arc::new(gh), Arc::new(git), 7);
        assert!(cache.get(detail.number).is_none());
        cache.load_pr(detail.number).unwrap();
        assert!(cache.get(detail.number).is_some());
    }
}
