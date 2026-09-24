# Projects mode: every clone under one folder

## Goal

Review the open PRs of many repos in one list. The user keeps their clones
side by side in one folder (`~/projects/a`, `~/projects/b`, …) and runs
`prpr --projects` from that folder. Every clone is a project; every open
PR of every project is a row. Review, merge, draft toggle, and browser
open work exactly as in single-repo mode, because each PR still has a
local clone to run `git` and `gh` in.

This lifts the v1 non-goal "cross-repo PR queue" from the original design
spec, in its narrowest useful form: no GitHub search, no repo mapping, no
on-demand clones.

## Entry

- New flag `--projects`. Direct children of cwd are scanned. A child
  counts when it is a directory (symlinks followed), not hidden, has a
  `.git` entry, and `git remote -v` mentions `github.com`. Everything
  else is skipped silently.
- Zero matches → `no GitHub clones found under <cwd>`.
- `--projects` inside a clone → error; the flag is for the parent folder.
- Plain `prpr` outside a clone keeps its error and adds the hint to try
  `--projects`.

## Identity

A PR is `PrId { repo, number }`, where `repo` is the clone's folder name.
Two clones can share a PR number, so every cache, reselect, enrichment
merge, worker request, and worker response keys on `PrId`. Single-repo
mode fills the same field from its one clone, so there is one list code
path. Status text shows `#N` in single-repo mode and `repo#N` across
projects.

The git refs prpr writes (`refs/prpr/pr-N`) live inside each clone, so
they need no extra namespace.

## Fetching

One worker thread; each request carries its `PrId` and the worker looks
up the clone root. One `git fetch` lock per clone.

Per refresh:

1. `gh pr list` (fast fields) per clone, at most 4 at a time. Progress
   events carry `done/total` so the placeholder reads
   `fetching PR list (gh) 3/7 repos…`.
2. `ListFast` carries the rows of every clone that answered plus the
   names of clones that failed (`stale`). The UI keeps a stale clone's
   previous rows instead of dropping them. Only when every clone fails is
   the refresh an error.
3. `git fetch` runs only for clones whose open-PR set changed since their
   last successful fetch, compared by `(number, updated_at)`. The first
   refresh fetches all. A base branch moving on its own does not trigger
   a fetch; GitHub's own `mergeable` verdict still arrives via enrichment
   and overrides the local one, and opening a PR uses the three-dot diff,
   so a stale base does not change what is shown.
4. Local conflict verdicts are computed for every clone that answered.
5. Enrichment (`statusCheckRollup`, `reviewDecision`, `mergeable`) per
   clone, same cap. Clones still answering `UNKNOWN` are re-polled; each
   round re-sends everything gathered so far.

Manual refresh and startup wait for every clone's fast list before rows
show. Silent auto-refresh keeps the old rows on screen, as before.

## List view

- Rows are sorted newest `updatedAt` first, in both modes. Equal
  timestamps keep `gh` order.
- Projects mode adds a repo column before `#N`, padded to the widest
  visible name and capped at 16 columns.
- Header: `prpr · <folder> · 7 repos · 23 open` (single mode keeps
  `prpr · <repo> · <branch> · N open`).
- `/` also matches the repo name, in both modes.

## Components

| File | Change |
|---|---|
| `src/data/projects.rs` | `Repo { name, root }`, `discover(dir, git)`, `par_map` (bounded parallel map), `PARALLEL_REPOS = 4`. |
| `src/data/pr.rs` | `PrId`; `Pr.repo` and `PrEnrichment.repo` (`serde(skip)`, filled by the worker). |
| `src/data/worker.rs` | `Worker::spawn(Vec<Repo>, …)`. Requests/responses keyed by `PrId`. `ListProgress` gains `done/total`; `ListFast` gains `stale`. Per-clone fetch locks and fetch signatures. |
| `src/app.rs` | `App.repos`; `AppState::new_projects`; `PrId` everywhere a number was the key; stale-row retention; sort; `pr_label`. |
| `src/view/pr_list.rs` | `projects`, `repo_count`, `loading_progress`; repo column; header; search on repo. |
| `src/view/merge_modal.rs` | Modal and progress state carry `PrId` plus a display label. |
| `src/main.rs` | `--projects`; `single_startup` / `projects_startup`. |

## Not in this version

- Reading repos from a config file or a GitHub search query.
- Nested discovery deeper than one level.
- Per-clone error reporting in the UI; a failing clone is silent.
