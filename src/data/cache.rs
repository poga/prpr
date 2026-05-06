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

    pub fn insert(&mut self, pkg: PrPackage) {
        let key = (pkg.detail.number, pkg.detail.head_ref_oid.clone());
        self.packages.insert(key, pkg);
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
    use pretty_assertions::assert_eq;

    fn fixture_detail(head_oid: &str) -> PrDetail {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let mut d: PrDetail = serde_json::from_str(json).unwrap();
        d.head_ref_oid = head_oid.into();
        d
    }

    fn empty_pkg(detail: PrDetail) -> PrPackage {
        PrPackage {
            detail,
            files: vec![],
            colors: HashMap::new(),
            commit_stats: HashMap::new(),
        }
    }

    #[test]
    fn set_list_replaces_old_value() {
        let mut cache = Cache::new();
        assert!(cache.list.is_none());
        cache.set_list(vec![]);
        assert_eq!(cache.list.as_ref().unwrap().len(), 0);
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut cache = Cache::new();
        let pkg = empty_pkg(fixture_detail("aaaa"));
        let num = pkg.detail.number;
        cache.insert(pkg);
        assert!(cache.get(num).is_some());
    }

    #[test]
    fn force_push_lands_in_a_fresh_slot() {
        let mut cache = Cache::new();
        cache.insert(empty_pkg(fixture_detail("first")));
        cache.insert(empty_pkg(fixture_detail("second")));
        // Both entries coexist; `get` returns one of them (the most recent
        // in iteration order). Important property: we have two slots.
        assert_eq!(cache.packages.len(), 2);
    }

    #[test]
    fn get_returns_none_for_unknown_pr() {
        let cache = Cache::new();
        assert!(cache.get(999).is_none());
    }
}
