use crate::Oid;
use anyhow::{Context, Result, anyhow};
use std::{str::FromStr, sync::Arc};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StashEntry {
    pub index: usize,
    pub oid: Oid,
    pub message: String,
    pub branch: Option<String>,
    pub timestamp: i64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StashFileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl StashFileStatus {
    pub fn from_status_code(code: &str) -> Option<Self> {
        match code.chars().next()? {
            'A' | 'C' => Some(Self::Added),
            'M' | 'T' | 'U' => Some(Self::Modified),
            'D' => Some(Self::Deleted),
            'R' => Some(Self::Renamed),
            _ => None,
        }
    }

    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StashFile {
    pub status: StashFileStatus,
    pub path: String,
    pub old_path: Option<String>,
}

/// Parse `git stash show --name-status` output into changed files.
pub fn parse_stash_files(output: &str) -> Vec<StashFile> {
    output.lines().filter_map(parse_stash_file_line).collect()
}

fn parse_stash_file_line(line: &str) -> Option<StashFile> {
    let mut parts = line.split('\t');
    let code = parts.next()?.trim();
    let status = StashFileStatus::from_status_code(code)?;
    let first_path = unquote_git_path(parts.next()?);

    if matches!(status, StashFileStatus::Renamed) {
        let new_path = parts.next().map(unquote_git_path)?;
        Some(StashFile {
            status,
            path: new_path,
            old_path: Some(first_path),
        })
    } else {
        Some(StashFile {
            status,
            path: first_path,
            old_path: None,
        })
    }
}

fn unquote_git_path(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
    };

    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            result.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => result.push('\n'),
            Some('t') => result.push('\t'),
            Some('r') => result.push('\r'),
            Some('\\') => result.push('\\'),
            Some('"') => result.push('"'),
            Some(other) => result.push(other),
            None => break,
        }
    }
    result
}

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct GitStash {
    pub entries: Arc<[StashEntry]>,
}

impl GitStash {
    pub fn apply(&mut self, other: GitStash) {
        self.entries = other.entries;
    }
}

impl FromStr for GitStash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        if s.trim().is_empty() {
            return Ok(Self::default());
        }

        let mut entries = Vec::new();
        let mut errors = Vec::new();

        for (line_num, line) in s.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match parse_stash_line(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    errors.push(format!("Line {}: {}", line_num + 1, e));
                }
            }
        }

        // If we have some valid entries but also some errors, log the errors but continue
        if !errors.is_empty() && !entries.is_empty() {
            log::warn!("Failed to parse some stash entries: {}", errors.join(", "));
        } else if !errors.is_empty() {
            return Err(anyhow!(
                "Failed to parse stash entries: {}",
                errors.join(", ")
            ));
        }

        Ok(Self {
            entries: entries.into(),
        })
    }
}

/// Parse a single stash line in the format: "stash@{N}\0<oid>\0<timestamp>\0<message>"
fn parse_stash_line(line: &str) -> Result<StashEntry> {
    let parts: Vec<&str> = line.splitn(4, '\0').collect();

    if parts.len() != 4 {
        return Err(anyhow!(
            "Expected 4 null-separated parts, got {}",
            parts.len()
        ));
    }

    let index = parse_stash_index(parts[0])
        .with_context(|| format!("Failed to parse stash index from '{}'", parts[0]))?;

    let oid = Oid::from_str(parts[1])
        .with_context(|| format!("Failed to parse OID from '{}'", parts[1]))?;

    let timestamp = parts[2]
        .parse::<i64>()
        .with_context(|| format!("Failed to parse timestamp from '{}'", parts[2]))?;

    let (branch, message) = parse_stash_message(parts[3]);

    Ok(StashEntry {
        index,
        oid,
        message: message.to_string(),
        branch: branch.map(Into::into),
        timestamp,
    })
}

/// Parse stash index from format "stash@{N}" where N is the index
fn parse_stash_index(input: &str) -> Result<usize> {
    let trimmed = input.trim();

    if !trimmed.starts_with("stash@{") || !trimmed.ends_with('}') {
        return Err(anyhow!(
            "Invalid stash index format: expected 'stash@{{N}}'"
        ));
    }

    let index_str = trimmed
        .strip_prefix("stash@{")
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| anyhow!("Failed to extract index from stash reference"))?;

    index_str
        .parse::<usize>()
        .with_context(|| format!("Invalid stash index number: '{}'", index_str))
}

/// Parse stash message and extract branch information if present
///
/// Handles the following formats:
/// - "WIP on <branch>: <message>" -> (Some(branch), message)
/// - "On <branch>: <message>" -> (Some(branch), message)
/// - "<message>" -> (None, message)
fn parse_stash_message(input: &str) -> (Option<&str>, &str) {
    // Handle "WIP on <branch>: <message>" pattern
    if let Some(stripped) = input.strip_prefix("WIP on ")
        && let Some(colon_pos) = stripped.find(": ")
    {
        let branch = &stripped[..colon_pos];
        let message = &stripped[colon_pos + 2..];
        if !branch.is_empty() && !message.is_empty() {
            return (Some(branch), message);
        }
    }

    // Handle "On <branch>: <message>" pattern
    if let Some(stripped) = input.strip_prefix("On ")
        && let Some(colon_pos) = stripped.find(": ")
    {
        let branch = &stripped[..colon_pos];
        let message = &stripped[colon_pos + 2..];
        if !branch.is_empty() && !message.is_empty() {
            return (Some(branch), message);
        }
    }

    // Fallback: treat entire input as message with no branch
    (None, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stash_index() {
        assert_eq!(parse_stash_index("stash@{0}").unwrap(), 0);
        assert_eq!(parse_stash_index("stash@{42}").unwrap(), 42);
        assert_eq!(parse_stash_index("  stash@{5}  ").unwrap(), 5);

        assert!(parse_stash_index("invalid").is_err());
        assert!(parse_stash_index("stash@{not_a_number}").is_err());
        assert!(parse_stash_index("stash@{0").is_err());
    }

    #[test]
    fn test_parse_stash_message() {
        // WIP format
        let (branch, message) = parse_stash_message("WIP on main: working on feature");
        assert_eq!(branch, Some("main"));
        assert_eq!(message, "working on feature");

        // On format
        let (branch, message) = parse_stash_message("On feature-branch: some changes");
        assert_eq!(branch, Some("feature-branch"));
        assert_eq!(message, "some changes");

        // No branch format
        let (branch, message) = parse_stash_message("just a regular message");
        assert_eq!(branch, None);
        assert_eq!(message, "just a regular message");

        // Edge cases
        let (branch, message) = parse_stash_message("WIP on : empty message");
        assert_eq!(branch, None);
        assert_eq!(message, "WIP on : empty message");

        let (branch, message) = parse_stash_message("On branch-name:");
        assert_eq!(branch, None);
        assert_eq!(message, "On branch-name:");
    }

    #[test]
    fn test_parse_stash_line() {
        let line = "stash@{0}\u{0000}abc123\u{0000}1234567890\u{0000}WIP on main: test commit";
        let entry = parse_stash_line(line).unwrap();

        assert_eq!(entry.index, 0);
        assert_eq!(entry.message, "test commit");
        assert_eq!(entry.branch, Some("main".to_string()));
        assert_eq!(entry.timestamp, 1234567890);
    }

    #[test]
    fn test_git_stash_from_str() {
        let input = "stash@{0}\u{0000}abc123\u{0000}1234567890\u{0000}WIP on main: first stash\nstash@{1}\u{0000}def456\u{0000}1234567891\u{0000}On feature: second stash";
        let stash = GitStash::from_str(input).unwrap();

        assert_eq!(stash.entries.len(), 2);
        assert_eq!(stash.entries[0].index, 0);
        assert_eq!(stash.entries[0].branch, Some("main".to_string()));
        assert_eq!(stash.entries[1].index, 1);
        assert_eq!(stash.entries[1].branch, Some("feature".to_string()));
    }

    #[test]
    fn test_parse_stash_files_name_status() {
        let parsed = parse_stash_files(
            "M\tbackend/controllers/A.php\nA\tnewfile.txt\nD\told.txt\nR100\told/name.rs\tnew/name.rs\n",
        );

        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0].status, StashFileStatus::Modified);
        assert_eq!(parsed[0].path, "backend/controllers/A.php");
        assert_eq!(parsed[1].status, StashFileStatus::Added);
        assert_eq!(parsed[2].status, StashFileStatus::Deleted);
        assert_eq!(parsed[3].status, StashFileStatus::Renamed);
        assert_eq!(parsed[3].path, "new/name.rs");
        assert_eq!(parsed[3].old_path.as_deref(), Some("old/name.rs"));
    }

    #[test]
    fn test_git_stash_empty_input() {
        let stash = GitStash::from_str("").unwrap();
        assert_eq!(stash.entries.len(), 0);

        let stash = GitStash::from_str("   \n  \n  ").unwrap();
        assert_eq!(stash.entries.len(), 0);
    }
}
