use collections::{BTreeMap, HashSet};
use git::stash::{StashEntry, StashFile};
use std::path::{Path, PathBuf};

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

/// The canonical git stash reference, e.g. `stash@{0}`.
pub fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// Title used for a stash file diff tab, matching PhpStorm's
/// `Stash@{n}: <path>` format.
pub fn stash_diff_title(index: usize, path: &str) -> String {
    format!("Stash@{{{index}}}: {path}")
}

/// A node in the stash file tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StashTreeNode {
    Folder(StashFolder),
    File(StashFile),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashFolder {
    pub name: String,
    /// Full directory path relative to the repository root.
    pub path: String,
    /// Number of files anywhere below this folder.
    pub file_count: usize,
    pub children: Vec<StashTreeNode>,
}

#[derive(Default)]
struct FolderBuilder {
    children: BTreeMap<String, FolderBuilder>,
    files: Vec<StashFile>,
}

impl FolderBuilder {
    fn into_nodes(self, prefix: &str) -> Vec<StashTreeNode> {
        let mut nodes = Vec::with_capacity(self.children.len() + self.files.len());

        for (name, child) in self.children {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let children = child.into_nodes(&path);
            let file_count = children
                .iter()
                .map(|node| match node {
                    StashTreeNode::Folder(folder) => folder.file_count,
                    StashTreeNode::File(_) => 1,
                })
                .sum();
            nodes.push(StashTreeNode::Folder(StashFolder {
                name,
                path,
                file_count,
                children,
            }));
        }

        let mut files = self.files;
        files.sort_by(|a, b| {
            a.path
                .rsplit('/')
                .next()
                .cmp(&b.path.rsplit('/').next())
                .then_with(|| a.path.cmp(&b.path))
        });
        nodes.extend(files.into_iter().map(StashTreeNode::File));

        nodes
    }
}

/// Builds a nested, folders-first tree from flat stash file paths.
pub fn build_stash_file_tree(files: &[StashFile]) -> Vec<StashTreeNode> {
    let mut root = FolderBuilder::default();

    for file in files {
        let mut components = file.path.split('/').filter(|part| !part.is_empty());
        let Some(mut current) = components.next() else {
            continue;
        };
        let mut folder = &mut root;

        for next in components {
            folder = folder.children.entry(current.to_string()).or_default();
            current = next;
        }

        folder.files.push(file.clone());
    }

    root.into_nodes("")
}

/// Message shown in the file area when there is nothing useful to render.
pub fn stash_files_message(
    selected: Option<usize>,
    loading: bool,
    files: &[StashFile],
) -> Option<&'static str> {
    if selected.is_none() {
        Some("Select a stash")
    } else if loading {
        Some("Loading…")
    } else if files.is_empty() {
        Some("No files")
    } else {
        None
    }
}

fn sanitize_preview_component(component: &str) -> Option<String> {
    if component.is_empty() || component == "." || component == ".." {
        return None;
    }

    let mut sanitized = String::with_capacity(component.len());
    for ch in component.chars() {
        let valid = !ch.is_control() && !matches!(ch, '/' | '\\' | ':' | '\0');
        sanitized.push(if valid { ch } else { '_' });
    }

    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        None
    } else {
        Some(sanitized)
    }
}

/// Sanitizes a repository-relative path so it can safely live below a preview
/// directory. Parent-directory segments are discarded, while the file name and
/// extension are preserved.
pub fn sanitize_preview_relative_path(path: &str) -> PathBuf {
    let mut sanitized = PathBuf::new();
    for component in path.split(['/', '\\']) {
        if let Some(component) = sanitize_preview_component(component) {
            sanitized.push(component);
        }
    }

    if sanitized.as_os_str().is_empty() {
        sanitized.push("file");
    }

    sanitized
}

fn sanitize_preview_key(key: &str) -> String {
    let sanitized: String = key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        "stash".to_string()
    } else {
        sanitized
    }
}

/// Temp path used to materialize a stash blob for preview.
pub fn stash_preview_path(base: &Path, stash_key: &str, path: &str) -> PathBuf {
    base.join("phpworkbench-stash-preview")
        .join(sanitize_preview_key(stash_key))
        .join(sanitize_preview_relative_path(path))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StashTabState {
    /// Stash index (`stash@{N}`), not a position in the rendered list.
    pub selected: Option<usize>,
    pub busy: bool,
    pub confirming_drop: bool,
    pub error: Option<String>,
    /// Files belonging to `files_for`.
    pub files: Vec<StashFile>,
    pub files_for: Option<usize>,
    pub files_loading: bool,
    pub files_error: Option<String>,
    /// Folder paths explicitly collapsed by the user. Folders are expanded by
    /// default, so only collapsed folders are stored here.
    pub collapsed_dirs: HashSet<String>,
    pub selected_file: Option<String>,
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
            if self.files_for != Some(index) {
                self.files.clear();
                self.selected_file = None;
                self.files_error = None;
            }
        }
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }

    pub fn begin_files_load(&mut self, index: usize) {
        self.files_for = Some(index);
        self.files.clear();
        self.files_loading = true;
        self.files_error = None;
        self.selected_file = None;
    }

    pub fn finish_files_load(&mut self, index: usize, files: Vec<StashFile>) {
        if self.files_for == Some(index) {
            self.files = files;
            self.files_loading = false;
            self.files_error = None;
        }
    }

    pub fn fail_files_load(&mut self, index: usize, error: String) {
        if self.files_for == Some(index) {
            self.files.clear();
            self.files_loading = false;
            self.files_error = Some(error);
        }
    }

    pub fn toggle_dir(&mut self, path: &str) {
        if !self.collapsed_dirs.remove(path) {
            self.collapsed_dirs.insert(path.to_string());
        }
    }

    pub fn is_dir_collapsed(&self, path: &str) -> bool {
        self.collapsed_dirs.contains(path)
    }

    pub fn select_file(&mut self, path: &str) {
        self.selected_file = Some(path.to_string());
    }

    /// Drop a selection that no longer exists after the list is reloaded.
    pub fn reconcile(&mut self, entries: &[StashEntry]) {
        if let Some(selected) = self.selected
            && !entries.iter().any(|entry| entry.index == selected)
        {
            self.selected = None;
            self.confirming_drop = false;
            self.files.clear();
            self.files_for = None;
            self.files_loading = false;
            self.files_error = None;
            self.selected_file = None;
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
    use git::stash::StashFileStatus;
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

    fn file(path: &str, status: StashFileStatus) -> StashFile {
        StashFile {
            status,
            path: path.to_string(),
            old_path: None,
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

    #[test]
    fn stash_ref_matches_git_syntax() {
        assert_eq!(stash_ref(0), "stash@{0}");
        assert_eq!(stash_ref(7), "stash@{7}");
    }

    #[test]
    fn status_letters_map_for_all_supported_states() {
        assert_eq!(StashFileStatus::Added.letter(), "A");
        assert_eq!(StashFileStatus::Modified.letter(), "M");
        assert_eq!(StashFileStatus::Deleted.letter(), "D");
        assert_eq!(StashFileStatus::Renamed.letter(), "R");

        assert_eq!(
            StashFileStatus::from_status_code("A"),
            Some(StashFileStatus::Added)
        );
        assert_eq!(
            StashFileStatus::from_status_code("M"),
            Some(StashFileStatus::Modified)
        );
        assert_eq!(
            StashFileStatus::from_status_code("D"),
            Some(StashFileStatus::Deleted)
        );
        assert_eq!(
            StashFileStatus::from_status_code("R100"),
            Some(StashFileStatus::Renamed)
        );
        assert_eq!(StashFileStatus::from_status_code("X"), None);
    }

    #[test]
    fn stash_tree_groups_nested_directories_and_counts_files() {
        let files = vec![
            file("backend/controllers/A.php", StashFileStatus::Modified),
            file("backend/models/B.php", StashFileStatus::Modified),
            file("root.txt", StashFileStatus::Modified),
        ];

        let tree = build_stash_file_tree(&files);
        assert_eq!(tree.len(), 2);

        let StashTreeNode::Folder(backend) = &tree[0] else {
            panic!("expected backend folder first");
        };
        assert_eq!(backend.name, "backend");
        assert_eq!(backend.path, "backend");
        assert_eq!(backend.file_count, 2);
        assert_eq!(backend.children.len(), 2);

        let StashTreeNode::Folder(controllers) = &backend.children[0] else {
            panic!("expected controllers folder");
        };
        assert_eq!(controllers.name, "controllers");
        assert_eq!(controllers.path, "backend/controllers");
        assert_eq!(controllers.file_count, 1);

        let StashTreeNode::Folder(models) = &backend.children[1] else {
            panic!("expected models folder");
        };
        assert_eq!(models.name, "models");

        let StashTreeNode::File(root) = &tree[1] else {
            panic!("expected root file after folders");
        };
        assert_eq!(root.path, "root.txt");
    }

    #[test]
    fn stash_tree_orders_folders_before_files_at_every_level() {
        let files = vec![
            file("z.txt", StashFileStatus::Added),
            file("a/inner.txt", StashFileStatus::Added),
            file("m.txt", StashFileStatus::Added),
            file("a/deep/file.txt", StashFileStatus::Added),
        ];

        let tree = build_stash_file_tree(&files);
        assert_eq!(tree.len(), 3);

        let StashTreeNode::Folder(a) = &tree[0] else {
            panic!("expected a folder first");
        };
        assert_eq!(a.file_count, 2);
        let StashTreeNode::Folder(deep) = &a.children[0] else {
            panic!("expected deep folder");
        };
        assert_eq!(deep.path, "a/deep");
        let StashTreeNode::File(inner) = &a.children[1] else {
            panic!("expected inner file");
        };
        assert_eq!(inner.path, "a/inner.txt");

        assert!(matches!(&tree[1], StashTreeNode::File(f) if f.path == "m.txt"));
        assert!(matches!(&tree[2], StashTreeNode::File(f) if f.path == "z.txt"));
    }

    #[test]
    fn stash_tree_includes_files_added_directly_to_a_folder() {
        let files = vec![
            file("backend/root.php", StashFileStatus::Added),
            file("backend/controllers/A.php", StashFileStatus::Added),
        ];

        let tree = build_stash_file_tree(&files);
        let StashTreeNode::Folder(backend) = &tree[0] else {
            panic!("expected backend folder");
        };
        assert_eq!(backend.file_count, 2);
        assert_eq!(backend.children.len(), 2);
        assert!(
            matches!(&backend.children[0], StashTreeNode::Folder(f) if f.name == "controllers")
        );
        assert!(
            matches!(&backend.children[1], StashTreeNode::File(f) if f.path == "backend/root.php")
        );
    }

    #[test]
    fn preview_path_sanitizes_parent_segments_and_keeps_extension() {
        let path = sanitize_preview_relative_path("../../etc/passwd.php");
        assert_eq!(path, PathBuf::from("etc/passwd.php"));
        assert!(!path.components().any(|c| c.as_os_str() == ".."));
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("php"));

        let windows = sanitize_preview_relative_path("..\\..\\windows\\evil.rs");
        assert_eq!(windows, PathBuf::from("windows/evil.rs"));
        assert_eq!(windows.extension().and_then(|e| e.to_str()), Some("rs"));
    }

    #[test]
    fn preview_path_lives_under_stash_preview_directory() {
        let base = Path::new("/tmp");
        let path = stash_preview_path(base, "stash@{1}", "backend/controllers/A.php");
        assert_eq!(
            path,
            PathBuf::from("/tmp/phpworkbench-stash-preview/stash__1_/backend/controllers/A.php")
        );
        assert!(!path.components().any(|c| c.as_os_str() == ".."));
    }

    #[test]
    fn diff_title_uses_stash_reference() {
        assert_eq!(
            stash_diff_title(1, "backend/controllers/A.php"),
            "Stash@{1}: backend/controllers/A.php"
        );
    }

    #[test]
    fn empty_state_messages_are_explicit() {
        let files = vec![file("a.txt", StashFileStatus::Modified)];
        assert_eq!(
            stash_files_message(None, false, &[]),
            Some("Select a stash")
        );
        assert_eq!(stash_files_message(Some(0), true, &[]), Some("Loading…"));
        assert_eq!(stash_files_message(Some(0), false, &[]), Some("No files"));
        assert_eq!(stash_files_message(Some(0), false, &files), None);
    }

    #[test]
    fn file_load_state_tracks_the_selected_stash() {
        let mut state = StashTabState::default();
        state.select(2);
        state.begin_files_load(2);
        assert!(state.files_loading);
        state.finish_files_load(2, vec![file("a.txt", StashFileStatus::Modified)]);
        assert_eq!(state.files.len(), 1);
        assert!(!state.files_loading);

        state.begin_files_load(3);
        state.finish_files_load(2, vec![file("stale.txt", StashFileStatus::Deleted)]);
        assert!(state.files.is_empty());
        assert!(state.files_loading);
    }

    #[test]
    fn collapsed_dirs_are_expanded_by_default() {
        let mut state = StashTabState::default();
        assert!(!state.is_dir_collapsed("backend"));
        state.toggle_dir("backend");
        assert!(state.is_dir_collapsed("backend"));
        state.toggle_dir("backend");
        assert!(!state.is_dir_collapsed("backend"));
    }
}
