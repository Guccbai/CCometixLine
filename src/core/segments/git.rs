use super::{Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug)]
pub struct GitInfo {
    pub branch: String,
    pub status: GitStatus,
    pub ahead: u32,
    pub behind: u32,
    pub sha: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum GitStatus {
    Clean,
    Dirty,
    Conflicts,
}

pub struct GitSegment {
    show_sha: bool,
}

impl Default for GitSegment {
    fn default() -> Self {
        Self::new()
    }
}

impl GitSegment {
    pub fn new() -> Self {
        Self { show_sha: false }
    }

    pub fn with_sha(mut self, show_sha: bool) -> Self {
        self.show_sha = show_sha;
        self
    }

    /// One `git status --porcelain=v2 --branch` call yields branch, upstream
    /// ahead/behind, HEAD sha and working-tree state; None outside a repo.
    fn get_git_info(&self, working_dir: &str) -> Option<GitInfo> {
        let output = Command::new("git")
            .args([
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "--branch",
            ])
            .current_dir(working_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let mut info = parse_porcelain_v2(&String::from_utf8_lossy(&output.stdout));
        if !self.show_sha {
            info.sha = None;
        }
        Some(info)
    }
}

/// Parse `git status --porcelain=v2 --branch` output. Headers start with
/// `# `; `u` entries are unmerged (conflicts); any other entry means dirty.
fn parse_porcelain_v2(text: &str) -> GitInfo {
    let mut info = GitInfo {
        branch: "detached".to_string(),
        status: GitStatus::Clean,
        ahead: 0,
        behind: 0,
        sha: None,
    };
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => {
                    info.sha = Some(value.chars().take(7).collect());
                }
                "branch.head" if value != "(detached)" => info.branch = value.to_string(),
                "branch.ab" => {
                    for part in value.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            info.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            info.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
        } else if line.starts_with("u ") {
            info.status = GitStatus::Conflicts;
        } else if !line.is_empty() && info.status == GitStatus::Clean {
            info.status = GitStatus::Dirty;
        }
    }
    info
}

impl Segment for GitSegment {
    fn collect(&self, input: &InputData) -> Option<SegmentData> {
        let git_info = self.get_git_info(&input.workspace.current_dir)?;

        let mut metadata = HashMap::new();
        metadata.insert("branch".to_string(), git_info.branch.clone());
        metadata.insert("status".to_string(), format!("{:?}", git_info.status));
        metadata.insert("ahead".to_string(), git_info.ahead.to_string());
        metadata.insert("behind".to_string(), git_info.behind.to_string());

        if let Some(ref sha) = git_info.sha {
            metadata.insert("sha".to_string(), sha.clone());
        }

        let primary = git_info.branch;
        let mut status_parts = Vec::new();

        match git_info.status {
            GitStatus::Clean => status_parts.push("✓".to_string()),
            GitStatus::Dirty => status_parts.push("●".to_string()),
            GitStatus::Conflicts => status_parts.push("⚠".to_string()),
        }

        if git_info.ahead > 0 {
            status_parts.push(format!("↑{}", git_info.ahead));
        }
        if git_info.behind > 0 {
            status_parts.push(format!("↓{}", git_info.behind));
        }

        if let Some(ref sha) = git_info.sha {
            status_parts.push(sha.clone());
        }

        Some(SegmentData {
            primary,
            secondary: status_parts.join(" "),
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Git
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_branch_with_upstream() {
        let info = parse_porcelain_v2(
            "# branch.oid 3c43efd0123456789\n# branch.head master\n# branch.upstream origin/master\n# branch.ab +2 -1\n",
        );
        assert_eq!(info.branch, "master");
        assert_eq!(info.status, GitStatus::Clean);
        assert_eq!((info.ahead, info.behind), (2, 1));
        assert_eq!(info.sha.as_deref(), Some("3c43efd"));
    }

    #[test]
    fn file_names_do_not_fake_conflicts() {
        // Regression: a path containing "DD"/"UU"/"AA" used to read as a conflict.
        let info = parse_porcelain_v2("# branch.head main\n? DDL.sql\n? UUID.txt\n");
        assert_eq!(info.status, GitStatus::Dirty);
    }

    #[test]
    fn unmerged_entry_is_conflict() {
        let info = parse_porcelain_v2(
            "# branch.head main\n1 .M N... 100644 100644 100644 a b x.rs\nu AU N... 100644 100644 100644 100644 a b c y.rs\n",
        );
        assert_eq!(info.status, GitStatus::Conflicts);
    }

    #[test]
    fn detached_and_initial() {
        let info = parse_porcelain_v2("# branch.oid (initial)\n# branch.head (detached)\n");
        assert_eq!(info.branch, "detached");
        assert_eq!(info.sha, None);
    }
}
