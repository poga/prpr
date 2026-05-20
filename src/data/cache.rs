//! Passive in-memory cache for the PR list. The PR list comes from
//! `gh pr list` (the only real network call); everything else is derived
//! from local git refs on demand in the worker.

use crate::data::pr::Pr;

#[derive(Default)]
pub struct Cache {
    pub list: Option<Vec<Pr>>,
}

impl Cache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_list(&mut self, prs: Vec<Pr>) {
        self.list = Some(prs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::pr::{Author, Pr, PrState};
    use pretty_assertions::assert_eq;

    fn pr(n: u32) -> Pr {
        Pr {
            number: n,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "u".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            base_ref_name: "main".into(),
            head_ref_name: "feat".into(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
            mergeable: None,
        }
    }

    #[test]
    fn set_list_replaces_old_value() {
        let mut c = Cache::new();
        c.set_list(vec![pr(1)]);
        c.set_list(vec![pr(2), pr(3)]);
        let list = c.list.as_ref().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].number, 2);
    }
}
