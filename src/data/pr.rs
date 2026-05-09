use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};

/// `gh` returns `"reviewDecision": ""` for PRs that haven't been reviewed,
/// not `null` and not a missing key. Treat empty strings as `None`.
fn deser_review_decision<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<ReviewDecision>, D::Error> {
    use serde::de::IntoDeserializer;
    let s: Option<String> = Option::deserialize(d)?;
    match s.as_deref() {
        None | Some("") => Ok(None),
        Some(other) => ReviewDecision::deserialize(other.into_deserializer()).map(Some),
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pr {
    pub number: u32,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    pub state: PrState,
    pub author: Author,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<StatusCheck>,
    #[serde(
        rename = "reviewDecision",
        default,
        deserialize_with = "deser_review_decision"
    )]
    pub review_decision: Option<ReviewDecision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Author {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StatusCheck {
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl Pr {
    /// Aggregate CI conclusion across status_check_rollup.
    /// Returns "fail" if any check failed, "pending" if any are pending,
    /// "pass" if all completed successfully, "none" if empty.
    pub fn ci_state(&self) -> CiState {
        if self.status_check_rollup.is_empty() {
            return CiState::None;
        }
        let mut any_pending = false;
        for c in &self.status_check_rollup {
            match c.status.as_deref() {
                Some("COMPLETED") => match c.conclusion.as_deref() {
                    Some("SUCCESS") => {}
                    Some("FAILURE") | Some("TIMED_OUT") | Some("CANCELLED") => {
                        return CiState::Fail;
                    }
                    _ => {}
                },
                _ => any_pending = true,
            }
        }
        if any_pending {
            CiState::Pending
        } else {
            CiState::Pass
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Pass,
    Fail,
    Pending,
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PrDetail {
    pub number: u32,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    pub state: PrState,
    pub author: Author,
    #[serde(rename = "baseRefName")]
    pub base_ref_name: String,
    #[serde(rename = "baseRefOid")]
    pub base_ref_oid: String,
    #[serde(rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(rename = "headRefOid")]
    pub head_ref_oid: String,
    #[serde(default)]
    pub mergeable: Option<String>,
    #[serde(rename = "statusCheckRollup", default)]
    pub status_check_rollup: Vec<StatusCheck>,
    #[serde(
        rename = "reviewDecision",
        default,
        deserialize_with = "deser_review_decision"
    )]
    pub review_decision: Option<ReviewDecision>,
    pub commits: Vec<Commit>,
    pub files: Vec<FileMeta>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Commit {
    pub oid: String,
    #[serde(rename = "messageHeadline")]
    pub message_headline: String,
    pub authors: Vec<Author>,
    #[serde(rename = "committedDate", default)]
    pub committed_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FileMeta {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_list_fixture() {
        let json = include_str!("../../tests/fixtures/pr_list.json");
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 482);
        assert_eq!(prs[0].author.login, "alice");
        assert_eq!(prs[0].labels[0].name, "bug");
        assert_eq!(prs[0].ci_state(), CiState::Pass);
        assert_eq!(prs[0].review_decision, Some(ReviewDecision::Approved));
        assert_eq!(prs[1].ci_state(), CiState::Fail);
    }

    #[test]
    fn ci_state_none_when_empty() {
        let pr = Pr {
            number: 1,
            title: "t".into(),
            is_draft: false,
            state: PrState::Open,
            author: Author { login: "a".into() },
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            labels: vec![],
            status_check_rollup: vec![],
            review_decision: None,
        };
        assert_eq!(pr.ci_state(), CiState::None);
    }

    #[test]
    fn empty_review_decision_string_parses_as_none() {
        // `gh pr list --json reviewDecision` returns "" (not null, not missing)
        // for PRs that haven't been reviewed yet. Make sure we tolerate that.
        let json = r#"[{
            "number": 1,
            "title": "t",
            "isDraft": false,
            "state": "OPEN",
            "author": { "login": "a" },
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "labels": [],
            "statusCheckRollup": [],
            "reviewDecision": ""
        }]"#;
        let prs: Vec<Pr> = serde_json::from_str(json).unwrap();
        assert_eq!(prs[0].review_decision, None);
    }

    #[test]
    fn parses_pr_view_fixture() {
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let pr: PrDetail = serde_json::from_str(json).unwrap();
        assert_eq!(pr.number, 482);
        assert_eq!(pr.head_ref_oid.len(), 40);
        assert_eq!(pr.commits.len(), 3);
        assert_eq!(
            pr.commits[0].oid,
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0"
        );
        assert_eq!(pr.files.len(), 4);
        assert_eq!(pr.files[0].path, "src/sched.rs");
    }

    #[test]
    fn parses_committed_date_when_present() {
        use chrono::TimeZone;
        let json = include_str!("../../tests/fixtures/pr_view.json");
        let detail: PrDetail = serde_json::from_str(json).unwrap();
        let first = &detail.commits[0];
        assert_eq!(
            first.committed_date,
            Some(Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap()),
        );
    }

    #[test]
    fn missing_committed_date_is_none() {
        // Older API responses or edge fixtures may omit the field.
        let json = r#"{"oid":"a","messageHeadline":"x","authors":[]}"#;
        let c: Commit = serde_json::from_str(json).unwrap();
        assert_eq!(c.committed_date, None);
    }
}
