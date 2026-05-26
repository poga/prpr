# Fancy file paths in PR list

## Goal

In the inline file list under the selected PR, dim the directory prefix and keep the filename bright. The eye should land on the filename when scanning.

## Change

In `file_line` (`src/view/pr_list.rs`), after the (possibly left-truncated) path string is computed, split it at the last `/`:

- If a `/` exists: render the prefix (everything up to and including the last `/`) in `OVERLAY1`; render the filename (everything after) in `TEXT`.
- If no `/` exists (top-level file like `Cargo.toml`): render the whole path in `TEXT`.

The split happens on the truncated string, not the original — so a path that was truncated to `…ar/baz.rs` renders as `OVERLAY1("…ar/") + TEXT("baz.rs")`.

If left-truncation removed the entire directory portion (the result starts with `…` but contains no `/`), render the whole truncated string in `TEXT` (no dim portion).

## Testing

One new test in the existing `mod tests` block of `src/view/pr_list.rs`:

`file_line_dims_directory_and_brightens_filename`:
- Render a `file_line(FileMeta { path: "src/foo/bar.rs", additions: 1, deletions: 0 }, false, 80)`.
- Inspect the returned `Line`'s spans and assert that:
  - The dir prefix span exists with text `"src/foo/"` and foreground `OVERLAY1`.
  - The filename span exists with text `"bar.rs"` and foreground `TEXT`.

(A second top-level-file assertion can live in the same test or as a sibling — implementer's choice; both are cheap.)

## Out of scope

- File-type icons (Nerd Font dependency).
- Extension coloring.
- Group-by-directory layouts.
