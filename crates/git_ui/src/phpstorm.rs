//! PhpStorm-style git workflow helpers.
//!
//! Everything in this module is deliberately free of GPUI/stateful types so
//! that the formatting and state-machine rules can be unit-tested in
//! isolation. The UI code in `branch_picker` and `push_dialog` is a thin
//! wrapper around these helpers.

use std::cmp::Ordering;

use git::repository::UpstreamTracking;

/// Right-aligned ahead/behind badges for a branch row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AheadBehindBadges {
    /// `↑n`, omitted when there is nothing to push.
    pub ahead: Option<String>,
    /// `↓n`, omitted when there is nothing to pull.
    pub behind: Option<String>,
    /// Muted note rendered when no numeric badge applies.
    pub note: Option<String>,
}

impl AheadBehindBadges {
    pub fn is_empty(&self) -> bool {
        self.ahead.is_none() && self.behind.is_none() && self.note.is_none()
    }
}

/// Build the PhpStorm-style `↑n` / `↓n` badges for a branch's upstream.
///
/// Zero counters are hidden. Branches without an upstream get a muted
/// `no upstream` note instead of numbers.
pub fn ahead_behind_badges(tracking: Option<UpstreamTracking>) -> AheadBehindBadges {
    match tracking {
        None => AheadBehindBadges {
            note: Some("no upstream".to_string()),
            ..Default::default()
        },
        Some(UpstreamTracking::Gone) => AheadBehindBadges {
            note: Some("upstream gone".to_string()),
            ..Default::default()
        },
        Some(UpstreamTracking::Tracked(status)) => AheadBehindBadges {
            ahead: (status.ahead > 0).then(|| format!("↑{}", status.ahead)),
            behind: (status.behind > 0).then(|| format!("↓{}", status.behind)),
            note: None,
        },
    }
}

/// One outgoing commit as rendered in the Push dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingCommitRow {
    pub short_sha: String,
    pub summary: String,
    pub author: String,
    pub relative_date: String,
}

impl OutgoingCommitRow {
    /// Single-line rendering used by the Push dialog and by tests.
    pub fn format(&self) -> String {
        let mut row = format!(
            "{} · {} · {} · {}",
            self.short_sha, self.summary, self.author, self.relative_date
        );
        if row.contains('\n') {
            row = row.replace('\n', " ");
        }
        row
    }
}

/// Display order used by the branch popup: current branch first, then
/// alphabetical by lowercase name, local branches before remote ones.
pub fn compare_branch_display(
    left: (&str, bool, bool),
    right: (&str, bool, bool),
) -> Ordering {
    let (left_name, left_is_head, left_is_remote) = left;
    let (right_name, right_is_head, right_is_remote) = right;
    (!left_is_head, left_is_remote, left_name.to_lowercase()).cmp(&(
        !right_is_head,
        right_is_remote,
        right_name.to_lowercase(),
    ))
}

/// Subtitle shown under the Push dialog title.
pub fn push_target_label(
    repo_name: &str,
    remote_name: &str,
    branch_name: &str,
    has_upstream: bool,
) -> String {
    if has_upstream {
        format!("{repo_name} → {remote_name}/{branch_name}")
    } else {
        format!("{repo_name} → {remote_name}/{branch_name} (will set upstream)")
    }
}

/// Push dialog lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PushDialogState {
    #[default]
    Idle,
    Pushing,
    Succeeded,
    Failed(String),
}

impl PushDialogState {
    pub fn begin_push(&mut self) -> bool {
        match self {
            Self::Idle | Self::Failed(_) => {
                *self = Self::Pushing;
                true
            }
            Self::Pushing | Self::Succeeded => false,
        }
    }

    pub fn succeed(&mut self) -> bool {
        match self {
            Self::Pushing => {
                *self = Self::Succeeded;
                true
            }
            _ => false,
        }
    }

    pub fn fail(&mut self, error: impl Into<String>) -> bool {
        match self {
            Self::Pushing => {
                *self = Self::Failed(error.into());
                true
            }
            _ => false,
        }
    }

    pub fn is_pushing(&self) -> bool {
        matches!(self, Self::Pushing)
    }

    pub fn can_push(&self) -> bool {
        matches!(self, Self::Idle | Self::Failed(_))
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use git::repository::UpstreamTrackingStatus;

    use super::*;

    fn tracked(ahead: u32, behind: u32) -> Option<UpstreamTracking> {
        Some(UpstreamTracking::Tracked(UpstreamTrackingStatus {
            ahead,
            behind,
        }))
    }

    #[test]
    fn ahead_behind_hides_zero_counts() {
        assert_eq!(ahead_behind_badges(tracked(0, 0)).ahead, None);
        assert_eq!(ahead_behind_badges(tracked(0, 0)).behind, None);
        assert!(ahead_behind_badges(tracked(0, 0)).is_empty());

        let ahead = ahead_behind_badges(tracked(7, 0));
        assert_eq!(ahead.ahead.as_deref(), Some("↑7"));
        assert_eq!(ahead.behind, None);

        let behind = ahead_behind_badges(tracked(0, 2));
        assert_eq!(behind.ahead, None);
        assert_eq!(behind.behind.as_deref(), Some("↓2"));

        let both = ahead_behind_badges(tracked(3, 1));
        assert_eq!(both.ahead.as_deref(), Some("↑3"));
        assert_eq!(both.behind.as_deref(), Some("↓1"));
    }

    #[test]
    fn ahead_behind_reports_missing_or_gone_upstream() {
        let none = ahead_behind_badges(None);
        assert_eq!(none.note.as_deref(), Some("no upstream"));
        assert_eq!(none.ahead, None);

        let gone = ahead_behind_badges(Some(UpstreamTracking::Gone));
        assert_eq!(gone.note.as_deref(), Some("upstream gone"));
    }

    #[test]
    fn outgoing_commit_rows_are_single_line() {
        let row = OutgoingCommitRow {
            short_sha: "abc1234".into(),
            summary: "Fix\nthing".into(),
            author: "Ada".into(),
            relative_date: "2 minutes ago".into(),
        };
        assert_eq!(row.format(), "abc1234 · Fix thing · Ada · 2 minutes ago");
    }

    #[test]
    fn branches_sort_current_first_then_alphabetically() {
        let mut branches = vec![
            ("zeta", false, false),
            ("beta", false, false),
            ("main", true, false),
            ("Alpha", false, false),
        ];
        branches.sort_by(|a, b| compare_branch_display(*a, *b));
        let names: Vec<_> = branches.into_iter().map(|(name, _, _)| name).collect();
        assert_eq!(names, vec!["main", "Alpha", "beta", "zeta"]);
    }

    #[test]
    fn push_state_machine_happy_path() {
        let mut state = PushDialogState::Idle;
        assert!(state.can_push());
        assert!(state.begin_push());
        assert_eq!(state, PushDialogState::Pushing);
        assert!(!state.begin_push());
        assert!(state.succeed());
        assert_eq!(state, PushDialogState::Succeeded);
    }

    #[test]
    fn push_state_machine_reports_errors_and_allows_retry() {
        let mut state = PushDialogState::Idle;
        assert!(state.begin_push());
        assert!(state.fail("network down"));
        assert_eq!(state.error(), Some("network down"));
        assert!(state.can_push());
        assert!(state.begin_push());
        assert!(state.succeed());
        assert_eq!(state.error(), None);
    }

    #[test]
    fn push_target_label_mentions_upstream_when_missing() {
        assert_eq!(
            push_target_label("zed", "origin", "main", true),
            "zed → origin/main"
        );
        assert_eq!(
            push_target_label("zed", "origin", "main", false),
            "zed → origin/main (will set upstream)"
        );
    }
}

/// Stable identifiers for the entries of the PhpStorm-style file context menu.
///
/// The identifiers are decoupled from the concrete GPUI action types so the
/// menu model can be unit-tested without a window or an application context.
pub mod entry_menu_action {
    pub const COMMIT_FILE: &str = "commit_file";
    pub const ROLLBACK: &str = "rollback";
    pub const SHOW_DIFF: &str = "show_diff";
    pub const SHOW_DIFF_NEW_TAB: &str = "show_diff_new_tab";
    pub const JUMP_TO_SOURCE: &str = "jump_to_source";
    pub const COPY_PATH: &str = "copy_path";
    pub const DELETE: &str = "delete";
    pub const ADD_TO_VCS: &str = "add_to_vcs";
    pub const ADD_TO_GITIGNORE: &str = "add_to_gitignore";
    pub const SHELVE: &str = "shelve";
    pub const REFRESH: &str = "refresh";
    pub const BRANCHES: &str = "git.branches";
    pub const FETCH: &str = "git.fetch";
    pub const PUSH: &str = "git.push";
    pub const PULL: &str = "git.pull";
    pub const STASH_CHANGES: &str = "git.stash_changes";
    pub const UNSTASH_CHANGES: &str = "git.unstash_changes";

    /// Action identifiers in this prefix are rendered inside the `Git`
    /// submenu rather than directly in the top-level menu.
    pub const GIT_PREFIX: &str = "git.";
}

/// Marker `action_id` used for separator rows.
pub const MENU_SEPARATOR: &str = "separator";

/// One row of the PhpStorm-style file context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemSpec {
    pub label: &'static str,
    pub enabled: bool,
    pub action_id: &'static str,
}

impl MenuItemSpec {
    pub fn separator() -> Self {
        Self {
            label: "",
            enabled: false,
            action_id: MENU_SEPARATOR,
        }
    }

    pub fn is_separator(&self) -> bool {
        self.action_id == MENU_SEPARATOR
    }
}

/// State of the changed file the context menu was opened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMenuState {
    /// Every affected file is fully staged.
    pub staged: bool,
    /// Every affected file is new/untracked.
    pub untracked: bool,
    /// Every affected file is deleted from the worktree.
    pub deleted: bool,
    /// Whether multiple files or a section header is selected.
    pub multiple: bool,
    /// Whether any files exist in this selection/target.
    pub has_files: bool,
}

impl Default for EntryMenuState {
    fn default() -> Self {
        Self {
            staged: false,
            untracked: false,
            deleted: false,
            multiple: false,
            has_files: true,
        }
    }
}

impl EntryMenuState {
    pub fn for_entry(staged: bool, untracked: bool, deleted: bool) -> Self {
        Self {
            staged,
            untracked,
            deleted,
            multiple: false,
            has_files: true,
        }
    }

    pub fn for_multiple(staged: bool, untracked: bool, deleted: bool, has_files: bool) -> Self {
        Self {
            staged,
            untracked,
            deleted,
            multiple: true,
            has_files,
        }
    }
}

/// Builds the PhpStorm file context menu, in PhpStorm's order.
///
/// Entries that this fork cannot wire to a real action (Local History,
/// Deployment, ...) are intentionally absent instead of being shown dead.
pub fn menu_items_for(state: EntryMenuState) -> Vec<MenuItemSpec> {
    use entry_menu_action as ids;

    let tracked = !state.untracked && state.has_files;
    let commit_label = if state.multiple {
        "Commit Files..."
    } else {
        "Commit File..."
    };

    let item = |label, enabled, action_id| MenuItemSpec {
        label,
        enabled,
        action_id,
    };

    vec![
        item(commit_label, state.has_files, ids::COMMIT_FILE),
        item("Rollback...", tracked, ids::ROLLBACK),
        MenuItemSpec::separator(),
        item("Show Diff", tracked, ids::SHOW_DIFF),
        item("Show Diff in a New Tab", tracked, ids::SHOW_DIFF_NEW_TAB),
        item("Jump to Source", state.has_files && !state.deleted, ids::JUMP_TO_SOURCE),
        item("Copy Path/Reference...", state.has_files, ids::COPY_PATH),
        MenuItemSpec::separator(),
        item("Delete...", state.has_files, ids::DELETE),
        item("Add to VCS", state.has_files && !state.staged, ids::ADD_TO_VCS),
        item("Add to .gitignore", state.has_files && state.untracked, ids::ADD_TO_GITIGNORE),
        MenuItemSpec::separator(),
        item("Shelve Changes...", tracked, ids::SHELVE),
        MenuItemSpec::separator(),
        item("Refresh", true, ids::REFRESH),
        item("Branches...", true, ids::BRANCHES),
        item("Fetch", true, ids::FETCH),
        item("Push...", true, ids::PUSH),
        item("Pull", true, ids::PULL),
        item("Stash Changes...", tracked, ids::STASH_CHANGES),
        item("Unstash Changes...", true, ids::UNSTASH_CHANGES),
    ]
}

#[cfg(test)]
mod entry_menu_tests {
    use super::entry_menu_action as ids;
    use super::*;

    fn labels(state: EntryMenuState) -> Vec<&'static str> {
        menu_items_for(state)
            .into_iter()
            .filter(|item| !item.is_separator())
            .map(|item| item.label)
            .collect()
    }

    fn enabled(state: EntryMenuState, action_id: &str) -> bool {
        menu_items_for(state)
            .into_iter()
            .find(|item| item.action_id == action_id)
            .unwrap_or_else(|| panic!("missing menu item {action_id}"))
            .enabled
    }

    #[test]
    fn menu_matches_phpstorm_order() {
        assert_eq!(
            labels(EntryMenuState::for_entry(false, false, false)),
            vec![
                "Commit File...",
                "Rollback...",
                "Show Diff",
                "Show Diff in a New Tab",
                "Jump to Source",
                "Copy Path/Reference...",
                "Delete...",
                "Add to VCS",
                "Add to .gitignore",
                "Shelve Changes...",
                "Refresh",
                "Branches...",
                "Fetch",
                "Push...",
                "Pull",
                "Stash Changes...",
                "Unstash Changes...",
            ]
        );
    }

    #[test]
    fn menu_multiple_uses_plural_commit() {
        assert_eq!(
            labels(EntryMenuState::for_multiple(false, false, false, true))[0],
            "Commit Files..."
        );
    }

    #[test]
    fn separators_split_the_menu_into_phpstorm_groups() {
        let items = menu_items_for(EntryMenuState::for_entry(false, false, false));
        let separator_indices: Vec<_> = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.is_separator().then_some(index))
            .collect();
        assert_eq!(separator_indices, vec![2, 7, 11, 13]);
    }

    #[test]
    fn staged_and_unstaged_tracked_files_only_differ_on_add_to_vcs() {
        let staged = EntryMenuState::for_entry(true, false, false);
        let unstaged = EntryMenuState::for_entry(false, false, false);

        assert!(!enabled(staged, ids::ADD_TO_VCS));
        assert!(enabled(unstaged, ids::ADD_TO_VCS));

        for action_id in [
            ids::COMMIT_FILE,
            ids::ROLLBACK,
            ids::SHOW_DIFF,
            ids::SHOW_DIFF_NEW_TAB,
            ids::JUMP_TO_SOURCE,
            ids::COPY_PATH,
            ids::DELETE,
            ids::REFRESH,
        ] {
            assert_eq!(
                enabled(staged, action_id),
                enabled(unstaged, action_id),
                "unexpected enablement difference for {action_id}"
            );
        }

        assert!(enabled(staged, ids::ROLLBACK));
        assert!(enabled(unstaged, ids::ROLLBACK));
        assert!(!enabled(staged, ids::ADD_TO_GITIGNORE));
        assert!(!enabled(unstaged, ids::ADD_TO_GITIGNORE));
    }

    #[test]
    fn untracked_files_disable_diff_and_rollback_but_enable_add_to_vcs() {
        let untracked = EntryMenuState::for_entry(false, true, false);

        assert!(!enabled(untracked, ids::ROLLBACK));
        assert!(!enabled(untracked, ids::SHOW_DIFF));
        assert!(!enabled(untracked, ids::SHOW_DIFF_NEW_TAB));
        assert!(enabled(untracked, ids::ADD_TO_VCS));
        assert!(enabled(untracked, ids::ADD_TO_GITIGNORE));
        assert!(enabled(untracked, ids::JUMP_TO_SOURCE));
    }

    #[test]
    fn deleted_files_cannot_jump_to_source() {
        let deleted = EntryMenuState::for_entry(false, false, true);
        assert!(!enabled(deleted, ids::JUMP_TO_SOURCE));
        assert!(enabled(deleted, ids::ROLLBACK));
    }

    #[test]
    fn omitted_entries_are_absent() {
        let labels = labels(EntryMenuState::for_entry(false, false, false));
        for omitted in [
            "Local History",
            "Deployment",
            "Unstaged Changes",
            "Staged Changes",
            "Copy Relative Path",
            "Add to .git/info/exclude",
            "View File History",
            "Stage File",
            "Discard Changes",
        ] {
            assert!(
                !labels.contains(&omitted),
                "menu should not contain {omitted}"
            );
        }
    }

    #[test]
    fn git_submenu_entries_use_the_git_prefix() {
        let items = menu_items_for(EntryMenuState::for_entry(false, false, false));
        let git_actions: Vec<_> = items
            .iter()
            .filter(|item| item.action_id.starts_with(ids::GIT_PREFIX))
            .map(|item| item.action_id)
            .collect();
        assert_eq!(
            git_actions,
            vec![ids::BRANCHES, ids::FETCH, ids::PUSH, ids::PULL]
        );
    }
}
