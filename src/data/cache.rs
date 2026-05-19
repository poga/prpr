//! Passive in-memory cache. The worker thread fetches data; the UI thread
//! drains worker responses and inserts results here. The Cache itself does
//! no I/O.

use std::collections::HashMap;

use crate::data::diff::FileDiff;
use crate::data::pr::{Pr, PrDetail};
use crate::render::attribution::{CommitStats, LineColors};

#[derive(Debug, Clone)]
pub struct PrPackage {
    pub detail: PrDetail,
    pub files: Vec<FileDiff>,
    /// Indexed by file path.
    pub colors: HashMap<String, LineColors>,
    /// Per-commit-OID add/delete counts, summed across all files. One
    /// entry per PR commit, even commits that touched no files.
    pub commit_stats: HashMap<String, CommitStats>,
}

#[derive(Default)]
pub struct Cache {
    pub list: Option<Vec<Pr>>,
    /// Key = (pr_number, head_sha). A force-push lands in a fresh slot.
    packages: HashMap<(u32, String), PrPackage>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_list(&mut self, prs: Vec<Pr>) {
        self.list = Some(prs);
    }

    /// Create a skeleton package: detail known, files & colors empty,
    /// `commit_stats` zero-filled for every PR commit.
    pub fn insert_partial(&mut self, detail: PrDetail) {
        let key = (detail.number, detail.head_ref_oid.clone());
        let commit_stats: HashMap<String, CommitStats> = detail
            .commits
            .iter()
            .map(|c| (c.oid.clone(), CommitStats::default()))
            .collect();
        let pkg = PrPackage {
            detail,
            files: vec![],
            colors: HashMap::new(),
            commit_stats,
        };
        self.packages.insert(key, pkg);
    }

    /// Replace the parsed `files` list on an existing partial. No-op if
    /// there is no entry for `(number, head_oid)` — a force-push moved the
    /// PR to a different slot since the diff was requested.
    pub fn update_diff(
        &mut self,
        number: u32,
        head_oid: &str,
        files: Vec<crate::data::diff::FileDiff>,
    ) {
        if let Some(pkg) = self.packages.get_mut(&(number, head_oid.to_string())) {
            pkg.files = files;
        }
    }

    /// Merge one file's colors and accumulate its per-commit stats into
    /// the existing entry. No-op if there is no entry for `(number, head_oid)`.
    pub fn add_file_colors(
        &mut self,
        number: u32,
        head_oid: &str,
        path: String,
        colors: LineColors,
        per_commit: HashMap<String, CommitStats>,
    ) {
        let Some(pkg) = self.packages.get_mut(&(number, head_oid.to_string())) else {
            return;
        };
        pkg.colors.insert(path, colors);
        for (oid, s) in per_commit {
            let entry = pkg.commit_stats.entry(oid).or_default();
            entry.adds += s.adds;
            entry.dels += s.dels;
        }
    }

    /// Look up the most recently-cached entry for `number`, regardless of
    /// `head_sha`. Returns `None` if nothing is cached yet.
    pub fn get(&self, number: u32) -> Option<&PrPackage> {
        self.packages
            .iter()
            .find(|((n, _), _)| *n == number)
            .map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::diff::FileDiff;
    use crate::render::attribution::{CommitStats, LineColors};
    use pretty_assertions::assert_eq;

    fn fixture_detail(head_oid: &str) -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let mut d: PrDetail = serde_json::from_str(json).unwrap();
        d.head_ref_oid = head_oid.into();
        d
    }

    #[test]
    fn set_list_replaces_old_value() {
        let mut cache = Cache::new();
        assert!(cache.list.is_none());
        cache.set_list(vec![]);
        assert_eq!(cache.list.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn insert_partial_zero_fills_commit_stats_for_every_pr_commit() {
        let mut cache = Cache::new();
        let detail = fixture_detail("aaaa");
        let commit_count = detail.commits.len();
        let number = detail.number;
        cache.insert_partial(detail);
        let pkg = cache.get(number).unwrap();
        assert!(pkg.files.is_empty());
        assert!(pkg.colors.is_empty());
        assert_eq!(pkg.commit_stats.len(), commit_count);
        for s in pkg.commit_stats.values() {
            assert_eq!(s.adds, 0);
            assert_eq!(s.dels, 0);
        }
    }

    #[test]
    fn update_diff_swaps_files_when_head_oid_matches() {
        let mut cache = Cache::new();
        let detail = fixture_detail("head1");
        let number = detail.number;
        cache.insert_partial(detail);
        let files = vec![FileDiff {
            path: "a.rs".into(),
            lines: vec![],
            binary: false,
        }];
        cache.update_diff(number, "head1", files.clone());
        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.files.len(), 1);
        assert_eq!(pkg.files[0].path, "a.rs");
    }

    #[test]
    fn update_diff_is_noop_when_head_oid_does_not_match() {
        let mut cache = Cache::new();
        let detail = fixture_detail("head1");
        let number = detail.number;
        cache.insert_partial(detail);
        cache.update_diff(number, "different", vec![FileDiff {
            path: "a.rs".into(),
            lines: vec![],
            binary: false,
        }]);
        let pkg = cache.get(number).unwrap();
        assert!(pkg.files.is_empty());
    }

    #[test]
    fn add_file_colors_accumulates_stats_across_calls() {
        let mut cache = Cache::new();
        let detail = fixture_detail("h");
        let number = detail.number;
        let first_sha = detail.commits[0].oid.clone();
        cache.insert_partial(detail);

        let lc1 = LineColors {
            head: vec![],
            delete: HashMap::new(),
        };
        let mut s1 = HashMap::new();
        s1.insert(first_sha.clone(), CommitStats { adds: 3, dels: 1 });
        cache.add_file_colors(number, "h", "a.rs".into(), lc1, s1);

        let lc2 = LineColors {
            head: vec![],
            delete: HashMap::new(),
        };
        let mut s2 = HashMap::new();
        s2.insert(first_sha.clone(), CommitStats { adds: 5, dels: 2 });
        cache.add_file_colors(number, "h", "b.rs".into(), lc2, s2);

        let pkg = cache.get(number).unwrap();
        assert_eq!(pkg.colors.len(), 2);
        let stats = pkg.commit_stats.get(&first_sha).copied().unwrap();
        assert_eq!(stats.adds, 8);
        assert_eq!(stats.dels, 3);
    }

    #[test]
    fn add_file_colors_is_noop_when_entry_missing() {
        let mut cache = Cache::new();
        let mut stats = HashMap::new();
        stats.insert("x".to_string(), CommitStats { adds: 1, dels: 0 });
        cache.add_file_colors(
            999,
            "h",
            "a.rs".into(),
            LineColors {
                head: vec![],
                delete: HashMap::new(),
            },
            stats,
        );
        assert!(cache.get(999).is_none());
    }

    #[test]
    fn get_returns_none_for_unknown_pr() {
        let cache = Cache::new();
        assert!(cache.get(999).is_none());
    }
}
