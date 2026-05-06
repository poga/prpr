//! Parser for `git blame --porcelain <commit> -- <file>` output.
//!
//! The format alternates between header chunks (starting with a 40-char SHA
//! followed by source-line-number, result-line-number, [num-lines]) and a
//! TAB-prefixed source-line. Subsequent lines from the same commit show only
//! the `<sha> <orig> <result>` header and the TAB line; metadata (author etc.)
//! is omitted after the first appearance of a SHA.

use std::collections::HashMap;

/// Result: a vector indexed by `result_lineno - 1`. Holds the SHA that owns
/// each line. If the file is empty, the vector is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blame {
    pub line_shas: Vec<String>,
}

pub fn parse_blame(input: &str) -> Blame {
    let mut by_lineno: HashMap<u32, String> = HashMap::new();
    let mut max_line: u32 = 0;

    let mut lines = input.split('\n').peekable();
    while let Some(header) = lines.next() {
        if header.is_empty() {
            continue;
        }
        // Header form: "<sha> <orig> <result> [num]".
        let mut parts = header.split_whitespace();
        let Some(sha) = parts.next() else { continue };
        if sha.len() != 40 {
            continue;
        }
        let _orig = parts.next();
        let Some(result_str) = parts.next() else { continue };
        let Ok(result_lineno) = result_str.parse::<u32>() else { continue };

        // Skip metadata lines until we hit the TAB-prefixed source line.
        // Metadata appears only on first-mention of a SHA; for subsequent
        // mentions we go straight to the TAB line.
        loop {
            let Some(next) = lines.next() else { break };
            if next.starts_with('\t') {
                break;
            }
        }

        if result_lineno > max_line {
            max_line = result_lineno;
        }
        by_lineno.insert(result_lineno, sha.to_string());
    }

    let mut line_shas = vec![String::new(); max_line as usize];
    for (lineno, sha) in by_lineno {
        if lineno >= 1 && (lineno as usize) <= line_shas.len() {
            line_shas[lineno as usize - 1] = sha;
        }
    }
    Blame { line_shas }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_porcelain_fixture() {
        let input = include_str!("../../tests/fixtures/blame_porcelain.txt");
        let blame = parse_blame(input);
        // The fixture mentions lines 42, 43, 45, 46, 47.
        // Line indices 0..41 stay empty; index 41 (line 42) = a1...
        assert!(blame.line_shas.len() >= 47);
        assert_eq!(
            blame.line_shas[41],
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0",
        );
        assert_eq!(
            blame.line_shas[42],
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0",
        );
        assert_eq!(
            blame.line_shas[44],
            "d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d2e3",
        );
        assert_eq!(
            blame.line_shas[45],
            "789abcdef0123456789abcdef0123456789abcde",
        );
        assert_eq!(
            blame.line_shas[46],
            "789abcdef0123456789abcdef0123456789abcde",
        );
    }

    #[test]
    fn empty_input_returns_empty() {
        let blame = parse_blame("");
        assert!(blame.line_shas.is_empty());
    }
}
