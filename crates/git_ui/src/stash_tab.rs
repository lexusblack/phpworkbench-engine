use git::stash::StashEntry;

/// Split a raw stash subject into an optional branch name and the remaining
/// description.
///
/// Git's default subjects look like:
///   "WIP on <branch>: <oid> <subject>"
///   "On <branch>: <message>"
///
/// Custom stash messages carry no branch prefix.
pub fn split_stash_message(raw: &str) -> (Option<String>, String) {
    if let Some(rest) = raw.strip_prefix("WIP on ")
        && let Some((branch, message)) = rest.split_once(": ")
        && !branch.is_empty()
    {
        return (Some(branch.to_string()), message.to_string());
    }

    if let Some(rest) = raw.strip_prefix("On ")
        && let Some((branch, message)) = rest.split_once(": ")
        && !branch.is_empty()
    {
        return (Some(branch.to_string()), message.to_string());
    }

    (None, raw.to_string())
}

/// Branch chip text for a stash row. Prefers the branch parsed by the git crate
/// and falls back to parsing the message for robustness.
pub fn stash_branch_chip(entry: &StashEntry) -> Option<String> {
    entry
        .branch
        .clone()
        .or_else(|| split_stash_message(&entry.message).0)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StashTabState {
    /// Stash index (`stash@{N}`), not a position in the rendered list.
    pub selected: Option<usize>,
    pub busy: bool,
    pub confirming_drop: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StashButtonState {
    pub apply: bool,
    pub pop: bool,
    pub drop: bool,
}

impl StashTabState {
    pub fn select(&mut self, index: usize) {
        if !self.busy {
            self.selected = Some(index);
            self.confirming_drop = false;
            self.error = None;
        }
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    /// Drop a selection that no longer exists after the list is reloaded.
    pub fn reconcile(&mut self, entries: &[StashEntry]) {
        if let Some(selected) = self.selected
            && !entries.iter().any(|entry| entry.index == selected)
        {
            self.selected = None;
            self.confirming_drop = false;
        }
    }

    pub fn begin_drop(&mut self) -> bool {
        if self.buttons().drop {
            self.confirming_drop = true;
            true
        } else {
            false
        }
    }

    pub fn cancel_drop(&mut self) {
        self.confirming_drop = false;
    }

    pub fn buttons(&self) -> StashButtonState {
        let enabled = self.selected.is_some() && !self.busy && !self.confirming_drop;
        StashButtonState {
            apply: enabled,
            pop: enabled,
            drop: enabled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::Oid;
    use std::str::FromStr;

    fn entry(index: usize, branch: Option<&str>, message: &str) -> StashEntry {
        StashEntry {
            index,
            oid: Oid::from_str("abc123").unwrap(),
            message: message.to_string(),
            branch: branch.map(ToString::to_string),
            timestamp: 0,
        }
    }

    #[test]
    fn parses_wip_on_branch() {
        let (branch, message) = split_stash_message("WIP on main: abc123 work in progress");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(message, "abc123 work in progress");
    }

    #[test]
    fn parses_on_branch() {
        let (branch, message) = split_stash_message("On feature/login: custom note");
        assert_eq!(branch.as_deref(), Some("feature/login"));
        assert_eq!(message, "custom note");
    }

    #[test]
    fn keeps_custom_message_without_branch() {
        let (branch, message) = split_stash_message("work in progress A");
        assert_eq!(branch, None);
        assert_eq!(message, "work in progress A");
    }

    #[test]
    fn parses_multiline_branch_message() {
        let (branch, message) = split_stash_message("WIP on main: abc123 first line\nsecond line");
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(message, "abc123 first line\nsecond line");
    }

    #[test]
    fn branch_chip_prefers_entry_branch_then_message() {
        let with_branch = entry(0, Some("main"), "subject");
        assert_eq!(stash_branch_chip(&with_branch).as_deref(), Some("main"));

        let only_message = entry(1, None, "WIP on dev: abc123 subject");
        assert_eq!(stash_branch_chip(&only_message).as_deref(), Some("dev"));

        let custom = entry(2, None, "my stash");
        assert_eq!(stash_branch_chip(&custom), None);
    }

    #[test]
    fn selection_state_machine() {
        let mut state = StashTabState::default();
        assert_eq!(state.buttons(), StashButtonState::default());

        state.select(3);
        assert_eq!(state.selected, Some(3));
        assert_eq!(
            state.buttons(),
            StashButtonState {
                apply: true,
                pop: true,
                drop: true,
            }
        );

        state.busy = true;
        assert_eq!(state.buttons(), StashButtonState::default());
        state.busy = false;

        assert!(state.begin_drop());
        assert!(state.confirming_drop);
        assert_eq!(state.buttons(), StashButtonState::default());
        state.cancel_drop();
        assert!(!state.confirming_drop);
        assert!(state.buttons().drop);
    }

    #[test]
    fn reconcile_clears_missing_selection() {
        let mut state = StashTabState::default();
        state.select(1);
        state.reconcile(&[entry(0, Some("main"), "a"), entry(1, Some("dev"), "b")]);
        assert_eq!(state.selected, Some(1));

        state.reconcile(&[entry(0, Some("main"), "a")]);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn select_is_ignored_while_busy() {
        let mut state = StashTabState::default();
        state.select(2);
        state.busy = true;
        state.select(5);
        assert_eq!(state.selected, Some(2));
    }
}
