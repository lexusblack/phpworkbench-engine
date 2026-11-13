use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use db::kvp::KeyValueStore;
use editor::{Editor, EditorEvent};
use git::repository::{Branch, CreateWorktreeTarget};
use git_ui_core::notifications::show_error_toast;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Global,
    InteractiveElement, IntoElement, KeyContext, MouseButton, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, px, rems,
};
use notifications::status_toast::StatusToast;
use project::git_store::{Repository, RepositoryEvent};
use ui::{
    Button, ButtonStyle, Color, Divider, Icon, IconButton, IconName, IconSize, Label, LabelSize,
    Tooltip, prelude::*,
};
use workspace::{ModalView, Workspace};

use crate::push_dialog::PushDialog;

const FAVORITES_KVP_KEY: &str = "phpworkbench.branch_favorites.v1";
const MAX_RECENT_BRANCHES: usize = 6;

const fn action_shortcut(mac: &'static str, other: &'static str) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        let _ = other;
        mac
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = mac;
        other
    }
}

#[derive(Default)]
struct BranchFavoritesStore {
    loaded: bool,
    by_repo: HashMap<String, HashSet<String>>,
}

impl Global for BranchFavoritesStore {}

fn load_favorites(repo_key: &str, cx: &mut App) -> HashSet<String> {
    if !cx.has_global::<BranchFavoritesStore>() {
        cx.set_global(BranchFavoritesStore::default());
    }
    let needs_load = !cx.global::<BranchFavoritesStore>().loaded;
    if needs_load {
        let mut parsed = HashMap::new();
        if let Ok(Some(raw)) = KeyValueStore::global(cx).read_kvp(FAVORITES_KVP_KEY) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, Vec<String>>>(&raw) {
                for (k, list) in map {
                    parsed.insert(k, list.into_iter().collect::<HashSet<_>>());
                }
            }
        }
        let store = cx.global_mut::<BranchFavoritesStore>();
        store.by_repo = parsed;
        store.loaded = true;
    }
    cx.global::<BranchFavoritesStore>()
        .by_repo
        .get(repo_key)
        .cloned()
        .unwrap_or_default()
}

fn toggle_favorite_in_store(repo_key: &str, branch_name: &str, cx: &mut App) -> HashSet<String> {
    let _ = load_favorites(repo_key, cx);
    let snapshot_for_db = {
        let store = cx.global_mut::<BranchFavoritesStore>();
        let set = store.by_repo.entry(repo_key.to_string()).or_default();
        if !set.remove(branch_name) {
            set.insert(branch_name.to_string());
        }
        store
            .by_repo
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect::<Vec<_>>()))
            .collect::<HashMap<String, Vec<String>>>()
    };

    if let Ok(serialized) = serde_json::to_string(&snapshot_for_db) {
        let db = KeyValueStore::global(cx);
        db::write_and_log(cx, move || async move {
            db.write_kvp(FAVORITES_KVP_KEY.to_string(), serialized)
                .await
        });
    }

    cx.global::<BranchFavoritesStore>()
        .by_repo
        .get(repo_key)
        .cloned()
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BranchSection {
    Recent,
    Local,
    Remote,
    Tags,
}

impl BranchSection {
    fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Local => "Local",
            Self::Remote => "Remote",
            Self::Tags => "Tags",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchItem {
    pub full_name: String,
    pub tracking: Option<String>,
    pub ahead: Option<usize>,
    pub behind: Option<usize>,
    pub is_head: bool,
    pub is_remote: bool,
    pub timestamp: i64,
}

impl BranchItem {
    pub fn from_git_branch(branch: &Branch) -> Self {
        let full_name = branch.name().to_string();
        let tracking = branch
            .upstream
            .as_ref()
            .map(|upstream| upstream.ref_name.to_string());
        let (ahead, behind) = match branch.tracking_status() {
            Some(status) => (
                (status.ahead > 0).then_some(status.ahead as usize),
                (status.behind > 0).then_some(status.behind as usize),
            ),
            None => (None, None),
        };
        let timestamp = branch
            .most_recent_commit
            .as_ref()
            .map(|commit| commit.commit_timestamp)
            .unwrap_or(0);
        Self {
            full_name,
            tracking,
            ahead,
            behind,
            is_head: branch.is_head,
            is_remote: branch.is_remote(),
            timestamp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchTreeNode {
    Folder {
        name: String,
        path: String,
        children: Vec<BranchTreeNode>,
    },
    Leaf {
        label: String,
        item: BranchItem,
    },
}

/// Group branches sharing `/` prefixes into collapsible folder nodes, while keeping
/// top-level branches flat.
pub(crate) fn build_branch_tree(items: &[BranchItem]) -> Vec<BranchTreeNode> {
    #[derive(Default)]
    struct Builder {
        folders: BTreeMap<String, Builder>,
        leaves: Vec<(String, BranchItem)>,
    }

    impl Builder {
        fn insert(&mut self, parts: &[&str], item: BranchItem) {
            match parts {
                [] => {}
                [leaf] => self.leaves.push(((*leaf).to_string(), item)),
                [folder, rest @ ..] => {
                    self.folders
                        .entry((*folder).to_string())
                        .or_default()
                        .insert(rest, item);
                }
            }
        }

        fn build(self, prefix: &str) -> Vec<BranchTreeNode> {
            let mut nodes = Vec::new();
            for (folder_name, child_builder) in self.folders {
                let folder_path = if prefix.is_empty() {
                    folder_name.clone()
                } else {
                    format!("{prefix}/{folder_name}")
                };
                let children = child_builder.build(&folder_path);
                nodes.push(BranchTreeNode::Folder {
                    name: folder_name,
                    path: folder_path,
                    children,
                });
            }

            let mut leaves = self.leaves;
            leaves.sort_by(|(a_label, a_item), (b_label, b_item)| {
                (!a_item.is_head, a_label.to_lowercase())
                    .cmp(&(!b_item.is_head, b_label.to_lowercase()))
            });
            for (label, item) in leaves {
                nodes.push(BranchTreeNode::Leaf { label, item });
            }
            nodes
        }
    }

    let mut root = Builder::default();
    for item in items {
        let parts: Vec<&str> = item.full_name.split('/').collect();
        root.insert(&parts, item.clone());
    }
    root.build("")
}

pub(crate) fn select_recent_branches(
    branches: &[BranchItem],
    favorites: &HashSet<String>,
    limit: usize,
) -> Vec<BranchItem> {
    let mut local: Vec<&BranchItem> = branches.iter().filter(|b| !b.is_remote).collect();
    local.sort_by(|a, b| {
        let a_fav = favorites.contains(&a.full_name);
        let b_fav = favorites.contains(&b.full_name);
        match (a.is_head, b.is_head) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => match (a_fav, b_fav) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => b
                    .timestamp
                    .cmp(&a.timestamp)
                    .then_with(|| a.full_name.to_lowercase().cmp(&b.full_name.to_lowercase())),
            },
        }
    });

    let mut out = Vec::new();
    for item in local {
        let must_keep = item.is_head || favorites.contains(&item.full_name);
        if out.len() < limit || must_keep {
            out.push(item.clone());
        }
    }
    out
}

pub(crate) fn matches_tokens(haystack: &str, query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return true;
    }
    let hay = haystack.to_lowercase();
    trimmed
        .split_whitespace()
        .all(|token| hay.contains(&token.to_lowercase()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuickActionKind {
    UpdateProject,
    Commit,
    Push,
    NewBranch,
    NewWorktree,
    CheckoutRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QuickActionEntry {
    kind: QuickActionKind,
    icon: IconName,
    label: &'static str,
    shortcut: &'static str,
}

fn quick_actions() -> [QuickActionEntry; 6] {
    [
        QuickActionEntry {
            kind: QuickActionKind::UpdateProject,
            icon: IconName::ArrowDown,
            label: "Update Project...",
            shortcut: action_shortcut("⌘T", "Ctrl+T"),
        },
        QuickActionEntry {
            kind: QuickActionKind::Commit,
            icon: IconName::GitCommit,
            label: "Commit...",
            shortcut: action_shortcut("⌘K", "Ctrl+K"),
        },
        QuickActionEntry {
            kind: QuickActionKind::Push,
            icon: IconName::ExpandUp,
            label: "Push...",
            shortcut: action_shortcut("⇧⌘K", "Ctrl+Shift+K"),
        },
        QuickActionEntry {
            kind: QuickActionKind::NewBranch,
            icon: IconName::Plus,
            label: "New Branch...",
            shortcut: action_shortcut("⌥⌘N", "Ctrl+Alt+N"),
        },
        QuickActionEntry {
            kind: QuickActionKind::NewWorktree,
            icon: IconName::GitWorktree,
            label: "New Worktree...",
            shortcut: "",
        },
        QuickActionEntry {
            kind: QuickActionKind::CheckoutRevision,
            icon: IconName::Hash,
            label: "Checkout Tag or Revision...",
            shortcut: "",
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PopupRow {
    Action(QuickActionEntry),
    Divider,
    SectionHeader {
        section: BranchSection,
        label: &'static str,
        count: usize,
        expanded: bool,
    },
    Folder {
        section: BranchSection,
        key: String,
        name: String,
        depth: usize,
        expanded: bool,
    },
    Branch {
        section: BranchSection,
        depth: usize,
        label: String,
        item: BranchItem,
        is_favorite: bool,
    },
    Tag {
        depth: usize,
        name: String,
    },
    EmptyHint(&'static str),
}

impl PopupRow {
    fn is_selectable(&self) -> bool {
        !matches!(self, Self::Divider | Self::EmptyHint(_))
    }

    fn submenu_target(&self) -> Option<SubmenuTarget> {
        match self {
            Self::Branch { section, item, .. } => Some(SubmenuTarget::Branch {
                section: *section,
                item: item.clone(),
            }),
            Self::Tag { name, .. } => Some(SubmenuTarget::Tag(name.clone())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SubmenuTarget {
    Branch {
        section: BranchSection,
        item: BranchItem,
    },
    Tag(String),
}

impl SubmenuTarget {
    fn key(&self) -> String {
        match self {
            Self::Branch { section, item } => format!("{:?}:{}", section, item.full_name),
            Self::Tag(tag) => format!("tag:{tag}"),
        }
    }
}

#[derive(Clone)]
struct SubmenuAction {
    id: &'static str,
    label: SharedString,
    icon: IconName,
    color: Color,
    handler: Rc<dyn Fn(&mut PhpStormBranchPopup, &mut Window, &mut Context<PhpStormBranchPopup>)>,
}

enum SubmenuItem {
    Action(SubmenuAction),
    Divider,
}

pub struct PhpStormBranchPopup {
    workspace: WeakEntity<Workspace>,
    repository: Option<Entity<Repository>>,
    repo_key: String,
    search_editor: Entity<Editor>,
    query: String,
    all_branches: Vec<BranchItem>,
    tags: Vec<String>,
    tags_loaded: bool,
    favorites: HashSet<String>,
    expanded_sections: HashMap<BranchSection, bool>,
    expanded_folders: HashSet<String>,
    rows: Vec<PopupRow>,
    selected_index: Option<usize>,
    active_submenu: Option<(SubmenuTarget, usize)>,
    submenu_selected: usize,
    modal_mode: bool,
    _subscriptions: Vec<Subscription>,
    _tags_task: Option<Task<()>>,
}

impl EventEmitter<DismissEvent> for PhpStormBranchPopup {}
impl ModalView for PhpStormBranchPopup {}

impl Focusable for PhpStormBranchPopup {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_editor.focus_handle(cx)
    }
}

pub fn popover(
    workspace: WeakEntity<Workspace>,
    repository: Option<Entity<Repository>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PhpStormBranchPopup> {
    cx.new(|cx| PhpStormBranchPopup::new(workspace, repository, false, window, cx))
}

pub fn open_modal(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = workspace.weak_handle();
    let repository = workspace.project().read(cx).active_repository(cx);
    workspace.toggle_modal(window, cx, |window, cx| {
        PhpStormBranchPopup::new(workspace_handle, repository, true, window, cx)
    });
}

impl PhpStormBranchPopup {
    pub fn new(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        modal_mode: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search for branches and actions", window, cx);
            editor
        });

        let repo_key = repository
            .as_ref()
            .map(|repo| {
                repo.read(cx)
                    .work_directory_abs_path
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default();

        let all_branches = repository
            .as_ref()
            .map(|repo| {
                repo.read(cx)
                    .branch_list
                    .iter()
                    .map(BranchItem::from_git_branch)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let favorites = load_favorites(&repo_key, cx);

        let mut expanded_sections = HashMap::new();
        expanded_sections.insert(BranchSection::Recent, true);
        expanded_sections.insert(BranchSection::Local, true);
        expanded_sections.insert(BranchSection::Remote, false);
        expanded_sections.insert(BranchSection::Tags, false);

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &search_editor,
            window,
            |this, _, event: &EditorEvent, _window, cx| {
                if matches!(event, EditorEvent::BufferEdited) {
                    this.query = this.search_editor.read(cx).text(cx);
                    this.active_submenu = None;
                    this.rebuild_rows();
                    cx.notify();
                }
            },
        ));

        if let Some(repo) = &repository {
            subscriptions.push(cx.subscribe_in(
                repo,
                window,
                |this, repo, event, window, cx| {
                    if matches!(event, RepositoryEvent::BranchListChanged) {
                        this.all_branches = repo
                            .read(cx)
                            .branch_list
                            .iter()
                            .map(BranchItem::from_git_branch)
                            .collect();
                        this.load_tags(window, cx);
                        this.rebuild_rows();
                        cx.notify();
                    }
                },
            ));
        }

        let mut this = Self {
            workspace,
            repository,
            repo_key,
            search_editor,
            query: String::new(),
            all_branches,
            tags: Vec::new(),
            tags_loaded: false,
            favorites,
            expanded_sections,
            expanded_folders: HashSet::new(),
            rows: Vec::new(),
            selected_index: None,
            active_submenu: None,
            submenu_selected: 0,
            modal_mode,
            _subscriptions: subscriptions,
            _tags_task: None,
        };

        this.rebuild_rows();
        this.load_tags(window, cx);
        this.search_editor.focus_handle(cx).focus(window, cx);
        this
    }

    fn load_tags(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = &self.repository else {
            return;
        };
        let rx = repo.update(cx, |repo, _| repo.list_tags());
        self._tags_task = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(tags)) = rx.await {
                this.update(cx, |this, cx| {
                    this.tags = tags;
                    this.tags_loaded = true;
                    this.rebuild_rows();
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    fn current_branch_name(&self) -> Option<String> {
        self.all_branches
            .iter()
            .find(|b| b.is_head)
            .map(|b| b.full_name.clone())
    }

    fn folder_key(section: BranchSection, path: &str) -> String {
        format!("{section:?}:{path}")
    }

    fn is_section_expanded(&self, section: BranchSection) -> bool {
        if !self.query.trim().is_empty() {
            return true;
        }
        self.expanded_sections.get(&section).copied().unwrap_or(false)
    }

    fn is_folder_expanded(&self, section: BranchSection, path: &str) -> bool {
        if !self.query.trim().is_empty() {
            return true;
        }
        self.expanded_folders
            .contains(&Self::folder_key(section, path))
    }

    fn append_tree_rows(
        &self,
        rows: &mut Vec<PopupRow>,
        section: BranchSection,
        nodes: &[BranchTreeNode],
        depth: usize,
    ) {
        for node in nodes {
            match node {
                BranchTreeNode::Folder {
                    name,
                    path,
                    children,
                } => {
                    let expanded = self.is_folder_expanded(section, path);
                    rows.push(PopupRow::Folder {
                        section,
                        key: Self::folder_key(section, path),
                        name: name.clone(),
                        depth,
                        expanded,
                    });
                    if expanded {
                        self.append_tree_rows(rows, section, children, depth + 1);
                    }
                }
                BranchTreeNode::Leaf { label, item } => {
                    rows.push(PopupRow::Branch {
                        section,
                        depth,
                        label: label.clone(),
                        item: item.clone(),
                        is_favorite: self.favorites.contains(&item.full_name),
                    });
                }
            }
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        let query = self.query.trim();
        let is_searching = !query.is_empty();

        let actions = quick_actions();
        let mut top_actions = Vec::new();
        let mut mid_actions = Vec::new();
        for action in actions {
            if matches_tokens(action.label, query) {
                match action.kind {
                    QuickActionKind::UpdateProject
                    | QuickActionKind::Commit
                    | QuickActionKind::Push => top_actions.push(action),
                    QuickActionKind::NewBranch
                    | QuickActionKind::NewWorktree
                    | QuickActionKind::CheckoutRevision => mid_actions.push(action),
                }
            }
        }

        let has_actions = !top_actions.is_empty() || !mid_actions.is_empty();
        for action in &top_actions {
            rows.push(PopupRow::Action(action.clone()));
        }
        if !top_actions.is_empty() && !mid_actions.is_empty() {
            rows.push(PopupRow::Divider);
        }
        for action in &mid_actions {
            rows.push(PopupRow::Action(action.clone()));
        }
        if has_actions {
            rows.push(PopupRow::Divider);
        }

        let local_all: Vec<BranchItem> = self
            .all_branches
            .iter()
            .filter(|b| !b.is_remote)
            .cloned()
            .collect();
        let remote_all: Vec<BranchItem> = self
            .all_branches
            .iter()
            .filter(|b| b.is_remote)
            .cloned()
            .collect();

        let local_filtered: Vec<BranchItem> = local_all
            .iter()
            .filter(|b| {
                matches_tokens(&b.full_name, query)
                    || b.tracking
                        .as_deref()
                        .is_some_and(|t| matches_tokens(t, query))
            })
            .cloned()
            .collect();
        let remote_filtered: Vec<BranchItem> = remote_all
            .iter()
            .filter(|b| matches_tokens(&b.full_name, query))
            .cloned()
            .collect();
        let recent_filtered = if is_searching {
            Vec::new()
        } else {
            select_recent_branches(&local_all, &self.favorites, MAX_RECENT_BRANCHES)
        };
        let tags_filtered: Vec<String> = self
            .tags
            .iter()
            .filter(|t| matches_tokens(t, query))
            .cloned()
            .collect();

        if !recent_filtered.is_empty() {
            let expanded = self.is_section_expanded(BranchSection::Recent);
            rows.push(PopupRow::SectionHeader {
                section: BranchSection::Recent,
                label: BranchSection::Recent.label(),
                count: recent_filtered.len(),
                expanded,
            });
            if expanded {
                let tree = build_branch_tree(&recent_filtered);
                self.append_tree_rows(&mut rows, BranchSection::Recent, &tree, 1);
            }
        }

        if !local_filtered.is_empty() || !is_searching {
            let expanded = self.is_section_expanded(BranchSection::Local);
            rows.push(PopupRow::SectionHeader {
                section: BranchSection::Local,
                label: BranchSection::Local.label(),
                count: local_filtered.len(),
                expanded,
            });
            if expanded {
                if local_filtered.is_empty() {
                    rows.push(PopupRow::EmptyHint("No local branches"));
                } else {
                    let tree = build_branch_tree(&local_filtered);
                    self.append_tree_rows(&mut rows, BranchSection::Local, &tree, 1);
                }
            }
        }

        if !remote_filtered.is_empty() || !is_searching {
            let expanded = self.is_section_expanded(BranchSection::Remote);
            rows.push(PopupRow::SectionHeader {
                section: BranchSection::Remote,
                label: BranchSection::Remote.label(),
                count: remote_filtered.len(),
                expanded,
            });
            if expanded {
                if remote_filtered.is_empty() {
                    rows.push(PopupRow::EmptyHint("No remote branches"));
                } else {
                    let tree = build_branch_tree(&remote_filtered);
                    self.append_tree_rows(&mut rows, BranchSection::Remote, &tree, 1);
                }
            }
        }

        if !tags_filtered.is_empty() || !is_searching {
            let expanded = self.is_section_expanded(BranchSection::Tags);
            rows.push(PopupRow::SectionHeader {
                section: BranchSection::Tags,
                label: BranchSection::Tags.label(),
                count: tags_filtered.len(),
                expanded,
            });
            if expanded {
                if tags_filtered.is_empty() {
                    rows.push(PopupRow::EmptyHint(if self.tags_loaded {
                        "No tags"
                    } else {
                        "Loading tags..."
                    }));
                } else {
                    for tag in tags_filtered {
                        rows.push(PopupRow::Tag {
                            depth: 1,
                            name: tag,
                        });
                    }
                }
            }
        }

        self.rows = rows;
        if let Some(ix) = self.selected_index {
            if ix >= self.rows.len() || !self.rows[ix].is_selectable() {
                self.selected_index = self.rows.iter().position(PopupRow::is_selectable);
            }
        } else {
            self.selected_index = self.rows.iter().position(PopupRow::is_selectable);
        }
    }

    fn run_repo_op(
        &self,
        op_name: &'static str,
        success_toast: Option<String>,
        rx: futures::channel::oneshot::Receiver<anyhow::Result<()>>,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            match rx.await {
                Ok(Ok(())) => {
                    if let Some(msg) = success_toast {
                        workspace
                            .update(cx, |workspace, cx| {
                                let toast = StatusToast::new(msg, cx, |this, _| {
                                    this.icon(
                                        Icon::new(IconName::Check)
                                            .size(IconSize::Small)
                                            .color(Color::Success),
                                    )
                                });
                                workspace.toggle_status_toast(toast, cx);
                            })
                            .ok();
                    }
                }
                Ok(Err(err)) => {
                    if let Some(workspace) = workspace.upgrade() {
                        cx.update(|cx| {
                            show_error_toast(workspace, op_name, err, cx);
                        });
                    }
                }
                Err(_) => {}
            }
        })
        .detach();
    }

    fn execute_quick_action(
        &mut self,
        kind: QuickActionKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            QuickActionKind::UpdateProject => {
                window.dispatch_action(Box::new(git::Pull), cx);
                cx.emit(DismissEvent);
            }
            QuickActionKind::Commit => {
                window.dispatch_action(Box::new(git::Commit), cx);
                cx.emit(DismissEvent);
            }
            QuickActionKind::Push => {
                PushDialog::open(self.workspace.clone(), self.repository.clone(), window, cx);
                cx.emit(DismissEvent);
            }
            QuickActionKind::NewBranch => {
                self.open_new_branch_modal(None, window, cx);
                cx.emit(DismissEvent);
            }
            QuickActionKind::NewWorktree => {
                window.dispatch_action(Box::new(zed_actions::git::Worktree), cx);
                cx.emit(DismissEvent);
            }
            QuickActionKind::CheckoutRevision => {
                self.open_checkout_revision_modal(window, cx);
                cx.emit(DismissEvent);
            }
        }
    }

    fn open_new_branch_modal(
        &self,
        base_ref: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        let title: SharedString = match &base_ref {
            Some(base) => format!("New Branch from '{base}'").into(),
            None => "Create New Branch".into(),
        };
        BranchTextPromptModal::open(
            workspace.clone(),
            title,
            "",
            "New branch name",
            "Create",
            window,
            cx,
            move |name, _window, cx| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let rx = repo.update(cx, |repo, _| {
                    repo.create_branch(name.clone(), base_ref.clone())
                });
                let workspace = workspace.clone();
                cx.spawn(async move |cx| match rx.await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        if let Some(workspace) = workspace.upgrade() {
                            cx.update(|cx| show_error_toast(workspace, "switch -c", err, cx));
                        }
                    }
                    Err(_) => {}
                })
                .detach();
            },
        );
    }

    fn open_rename_branch_modal(
        &self,
        branch_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        let title: SharedString = format!("Rename Branch '{branch_name}'").into();
        let old_name = branch_name.clone();
        BranchTextPromptModal::open(
            workspace.clone(),
            title,
            &branch_name,
            "New branch name",
            "Rename",
            window,
            cx,
            move |new_name, _window, cx| {
                let new_name = new_name.trim().to_string();
                if new_name.is_empty() || new_name == old_name {
                    return;
                }
                let rx = repo.update(cx, |repo, _| repo.rename_branch(old_name.clone(), new_name));
                let workspace = workspace.clone();
                cx.spawn(async move |cx| {
                    if let Ok(Err(err)) = rx.await {
                        if let Some(workspace) = workspace.upgrade() {
                            cx.update(|cx| show_error_toast(workspace, "branch -m", err, cx));
                        }
                    }
                })
                .detach();
            },
        );
    }

    fn open_checkout_revision_modal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        BranchTextPromptModal::open(
            workspace.clone(),
            "Checkout Tag or Revision",
            "",
            "Tag, branch, or commit hash",
            "Checkout",
            window,
            cx,
            move |rev, _window, cx| {
                let rev = rev.trim().to_string();
                if rev.is_empty() {
                    return;
                }
                let rx = repo.update(cx, |repo, _| repo.checkout_revision(rev.clone()));
                let workspace = workspace.clone();
                cx.spawn(async move |cx| match rx.await {
                    Ok(Ok(())) => {
                        workspace
                            .update(cx, |workspace, cx| {
                                let toast = StatusToast::new(
                                    format!("Checked out {rev}"),
                                    cx,
                                    |this, _| this,
                                );
                                workspace.toggle_status_toast(toast, cx);
                            })
                            .ok();
                    }
                    Ok(Err(err)) => {
                        if let Some(workspace) = workspace.upgrade() {
                            cx.update(|cx| show_error_toast(workspace, "checkout", err, cx));
                        }
                    }
                    Err(_) => {}
                })
                .detach();
            },
        );
    }

    fn open_new_worktree_for_branch_modal(
        &self,
        branch_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo) = self.repository.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        let slug = branch_name.replace('/', "-");
        let default_path = {
            let repo_dir = &repo.read(cx).work_directory_abs_path;
            let base = repo_dir
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "repo".to_string());
            repo_dir
                .parent()
                .map(|p| p.join(format!("{base}-{slug}")))
                .unwrap_or_else(|| PathBuf::from(format!("{base}-{slug}")))
                .to_string_lossy()
                .to_string()
        };
        let title: SharedString = format!("Checkout '{branch_name}' in New Worktree").into();
        BranchTextPromptModal::open(
            workspace.clone(),
            title,
            &default_path,
            "Worktree directory path",
            "Create Worktree",
            window,
            cx,
            move |path_str, _window, cx| {
                let trimmed = path_str.trim();
                if trimmed.is_empty() {
                    return;
                }
                let target_path = PathBuf::from(trimmed);
                let rx = repo.update(cx, |repo, _| {
                    repo.create_worktree(
                        CreateWorktreeTarget::ExistingBranch {
                            branch_name: branch_name.clone(),
                        },
                        target_path.clone(),
                    )
                });
                let workspace = workspace.clone();
                cx.spawn(async move |cx| match rx.await {
                    Ok(Ok(())) => {
                        workspace
                            .update(cx, |workspace, cx| {
                                let toast = StatusToast::new(
                                    format!("Created worktree {}", target_path.display()),
                                    cx,
                                    |this, _| this,
                                );
                                workspace.toggle_status_toast(toast, cx);
                            })
                            .ok();
                    }
                    Ok(Err(err)) => {
                        if let Some(workspace) = workspace.upgrade() {
                            cx.update(|cx| show_error_toast(workspace, "worktree add", err, cx));
                        }
                    }
                    Err(_) => {}
                })
                .detach();
            },
        );
    }

    fn build_submenu_items(&self, target: &SubmenuTarget) -> Vec<SubmenuItem> {
        let current = self.current_branch_name();
        match target {
            SubmenuTarget::Branch { item, .. } => {
                let branch_name = item.full_name.clone();
                let short_label = branch_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&branch_name)
                    .to_string();
                let mut items = Vec::new();

                if !item.is_head {
                    let b = branch_name.clone();
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-checkout",
                        label: "Checkout".into(),
                        icon: IconName::Check,
                        color: Color::Accent,
                        handler: Rc::new(move |this, _window, cx| {
                            if let Some(repo) = &this.repository {
                                let rx = repo.update(cx, |repo, _| repo.change_branch(b.clone()));
                                this.run_repo_op("switch", None, rx, cx);
                            }
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                {
                    let b = branch_name.clone();
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-new-from",
                        label: format!("New Branch from '{short_label}'...").into(),
                        icon: IconName::GitBranchPlus,
                        color: Color::Default,
                        handler: Rc::new(move |this, window, cx| {
                            this.open_new_branch_modal(Some(b.clone()), window, cx);
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                if !item.is_remote {
                    let b = branch_name.clone();
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-worktree",
                        label: "Checkout in New Worktree...".into(),
                        icon: IconName::GitWorktree,
                        color: Color::Default,
                        handler: Rc::new(move |this, window, cx| {
                            this.open_new_worktree_for_branch_modal(b.clone(), window, cx);
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                if !item.is_head {
                    if let Some(curr) = &current {
                        items.push(SubmenuItem::Divider);
                        let merge_ref = branch_name.clone();
                        let curr_for_msg = curr.clone();
                        items.push(SubmenuItem::Action(SubmenuAction {
                            id: "sub-merge",
                            label: format!("Merge '{short_label}' into '{curr}'").into(),
                            icon: IconName::GitGraph,
                            color: Color::Default,
                            handler: Rc::new(move |this, _window, cx| {
                                if let Some(repo) = &this.repository {
                                    let rx = repo.update(cx, |repo, _| {
                                        repo.merge_branch(merge_ref.clone())
                                    });
                                    this.run_repo_op(
                                        "merge",
                                        Some(format!("Merged {merge_ref} into {curr_for_msg}")),
                                        rx,
                                        cx,
                                    );
                                }
                                cx.emit(DismissEvent);
                            }),
                        }));

                        let rebase_ref = branch_name.clone();
                        let curr_for_msg = curr.clone();
                        items.push(SubmenuItem::Action(SubmenuAction {
                            id: "sub-rebase",
                            label: format!("Rebase '{curr}' onto '{short_label}'").into(),
                            icon: IconName::ArrowUpRight,
                            color: Color::Default,
                            handler: Rc::new(move |this, _window, cx| {
                                if let Some(repo) = &this.repository {
                                    let rx = repo.update(cx, |repo, _| {
                                        repo.rebase_branch(rebase_ref.clone())
                                    });
                                    this.run_repo_op(
                                        "rebase",
                                        Some(format!("Rebased {curr_for_msg} onto {rebase_ref}")),
                                        rx,
                                        cx,
                                    );
                                }
                                cx.emit(DismissEvent);
                            }),
                        }));
                    }
                }

                if item.is_head {
                    items.push(SubmenuItem::Divider);
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-update",
                        label: "Update".into(),
                        icon: IconName::ArrowDown,
                        color: Color::Default,
                        handler: Rc::new(|_this, window, cx| {
                            window.dispatch_action(Box::new(git::Pull), cx);
                            cx.emit(DismissEvent);
                        }),
                    }));
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-push",
                        label: "Push...".into(),
                        icon: IconName::ExpandUp,
                        color: Color::Default,
                        handler: Rc::new(|this, window, cx| {
                            PushDialog::open(
                                this.workspace.clone(),
                                this.repository.clone(),
                                window,
                                cx,
                            );
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                if !item.is_remote {
                    items.push(SubmenuItem::Divider);
                    let b = branch_name.clone();
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-rename",
                        label: "Rename...".into(),
                        icon: IconName::Pencil,
                        color: Color::Default,
                        handler: Rc::new(move |this, window, cx| {
                            this.open_rename_branch_modal(b.clone(), window, cx);
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                if !item.is_head {
                    if item.is_remote {
                        items.push(SubmenuItem::Divider);
                    }
                    let b = branch_name;
                    let is_remote = item.is_remote;
                    items.push(SubmenuItem::Action(SubmenuAction {
                        id: "sub-delete",
                        label: "Delete".into(),
                        icon: IconName::Trash,
                        color: Color::Error,
                        handler: Rc::new(move |this, _window, cx| {
                            if let Some(repo) = &this.repository {
                                let rx = repo.update(cx, |repo, _| {
                                    repo.delete_branch(is_remote, b.clone(), false)
                                });
                                this.run_repo_op(
                                    "branch -d",
                                    Some(format!("Deleted branch {b}")),
                                    rx,
                                    cx,
                                );
                            }
                            cx.emit(DismissEvent);
                        }),
                    }));
                }

                items
            }
            SubmenuTarget::Tag(tag_name) => {
                let t1 = tag_name.clone();
                let t2 = tag_name.clone();
                vec![
                    SubmenuItem::Action(SubmenuAction {
                        id: "sub-checkout-tag",
                        label: format!("Checkout Tag '{tag_name}'").into(),
                        icon: IconName::Check,
                        color: Color::Accent,
                        handler: Rc::new(move |this, _window, cx| {
                            if let Some(repo) = &this.repository {
                                let rx = repo.update(cx, |repo, _| {
                                    repo.checkout_revision(format!("refs/tags/{t1}"))
                                });
                                this.run_repo_op(
                                    "checkout",
                                    Some(format!("Checked out tag {t1}")),
                                    rx,
                                    cx,
                                );
                            }
                            cx.emit(DismissEvent);
                        }),
                    }),
                    SubmenuItem::Action(SubmenuAction {
                        id: "sub-branch-from-tag",
                        label: format!("New Branch from '{tag_name}'...").into(),
                        icon: IconName::GitBranchPlus,
                        color: Color::Default,
                        handler: Rc::new(move |this, window, cx| {
                            this.open_new_branch_modal(Some(t2.clone()), window, cx);
                            cx.emit(DismissEvent);
                        }),
                    }),
                ]
            }
        }
    }

    fn toggle_all_folders(&mut self, cx: &mut Context<Self>) {
        if self.expanded_folders.is_empty() {
            for b in &self.all_branches {
                let section = if b.is_remote {
                    BranchSection::Remote
                } else {
                    BranchSection::Local
                };
                let parts: Vec<&str> = b.full_name.split('/').collect();
                for len in 1..parts.len() {
                    let prefix = parts[..len].join("/");
                    self.expanded_folders
                        .insert(Self::folder_key(section, &prefix));
                    if !b.is_remote {
                        self.expanded_folders
                            .insert(Self::folder_key(BranchSection::Recent, &prefix));
                    }
                }
            }
        } else {
            self.expanded_folders.clear();
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some((target, _)) = &self.active_submenu {
            let items = self.build_submenu_items(target);
            let selectable: Vec<usize> = items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| matches!(item, SubmenuItem::Action(_)).then_some(i))
                .collect();
            if let Some(pos) = selectable.iter().position(|&i| i == self.submenu_selected) {
                let next =
                    (pos as isize + delta).rem_euclid(selectable.len() as isize) as usize;
                self.submenu_selected = selectable[next];
            } else if let Some(&first) = selectable.first() {
                self.submenu_selected = first;
            }
            cx.notify();
            return;
        }

        let selectable: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.is_selectable().then_some(i))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let next_ix = match self
            .selected_index
            .and_then(|sel| selectable.iter().position(|&i| i == sel))
        {
            Some(pos) => {
                let next = (pos as isize + delta).rem_euclid(selectable.len() as isize) as usize;
                selectable[next]
            }
            None => selectable[0],
        };
        self.selected_index = Some(next_ix);
        cx.notify();
    }

    fn handle_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((target, _)) = self.active_submenu.clone() {
            let items = self.build_submenu_items(&target);
            if let Some(SubmenuItem::Action(action)) = items.get(self.submenu_selected) {
                (action.handler)(self, window, cx);
            }
            return;
        }

        let Some(ix) = self.selected_index else {
            return;
        };
        self.activate_row(ix, false, window, cx);
    }

    fn activate_row(
        &mut self,
        ix: usize,
        from_submenu_arrow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        self.selected_index = Some(ix);
        match row {
            PopupRow::Action(action) => {
                self.active_submenu = None;
                self.execute_quick_action(action.kind, window, cx);
            }
            PopupRow::SectionHeader { section, .. } => {
                self.active_submenu = None;
                let cur = self.expanded_sections.get(&section).copied().unwrap_or(false);
                self.expanded_sections.insert(section, !cur);
                self.rebuild_rows();
                cx.notify();
            }
            PopupRow::Folder { key, .. } => {
                self.active_submenu = None;
                if !self.expanded_folders.remove(&key) {
                    self.expanded_folders.insert(key);
                }
                self.rebuild_rows();
                cx.notify();
            }
            PopupRow::Branch { .. } | PopupRow::Tag { .. } => {
                if let Some(target) = row.submenu_target() {
                    let already_open = self
                        .active_submenu
                        .as_ref()
                        .is_some_and(|(cur, _)| cur.key() == target.key());
                    if already_open && from_submenu_arrow {
                        self.active_submenu = None;
                    } else {
                        self.active_submenu = Some((target, ix));
                        self.submenu_selected = 0;
                    }
                    cx.notify();
                }
            }
            PopupRow::Divider | PopupRow::EmptyHint(_) => {}
        }
    }

    fn render_row(&self, ix: usize, row: &PopupRow, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let is_selected = self.selected_index == Some(ix);
        let row_bg = if is_selected {
            colors.element_selected
        } else {
            gpui::transparent_black()
        };

        match row {
            PopupRow::Divider => div()
                .px_2()
                .py_1()
                .child(Divider::horizontal())
                .into_any_element(),
            PopupRow::EmptyHint(text) => div()
                .px_4()
                .py_1()
                .child(Label::new(*text).size(LabelSize::XSmall).color(Color::Muted))
                .into_any_element(),
            PopupRow::Action(action) => {
                let kind = action.kind;
                h_flex()
                    .id(("pwb-action-row", ix))
                    .w_full()
                    .h(px(26.))
                    .px_2()
                    .gap_2()
                    .justify_between()
                    .rounded_sm()
                    .bg(row_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.element_hover))
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(Icon::new(action.icon).size(IconSize::Small).color(Color::Muted))
                            .child(Label::new(action.label).size(LabelSize::Small)),
                    )
                    .when(!action.shortcut.is_empty(), |el| {
                        el.child(
                            Label::new(action.shortcut)
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.execute_quick_action(kind, window, cx);
                    }))
                    .into_any_element()
            }
            PopupRow::SectionHeader {
                section,
                label,
                count,
                expanded,
            } => {
                let section = *section;
                let chevron = if *expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                h_flex()
                    .id(("pwb-section-row", ix))
                    .w_full()
                    .h(px(24.))
                    .px_2()
                    .gap_1p5()
                    .rounded_sm()
                    .bg(row_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.element_hover))
                    .child(Icon::new(chevron).size(IconSize::XSmall).color(Color::Muted))
                    .child(
                        Label::new(*label)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("{count}"))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_row(ix, false, window, cx);
                        let _ = section;
                    }))
                    .into_any_element()
            }
            PopupRow::Folder {
                name,
                depth,
                expanded,
                ..
            } => {
                let chevron = if *expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                };
                let folder_icon = if *expanded {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                };
                h_flex()
                    .id(("pwb-folder-row", ix))
                    .w_full()
                    .h(px(25.))
                    .pl(px(8. + (*depth as f32) * 14.))
                    .pr_2()
                    .gap_1p5()
                    .rounded_sm()
                    .bg(row_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.element_hover))
                    .child(Icon::new(chevron).size(IconSize::XSmall).color(Color::Muted))
                    .child(Icon::new(folder_icon).size(IconSize::Small).color(Color::Muted))
                    .child(Label::new(name.clone()).size(LabelSize::Small))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_row(ix, false, window, cx);
                    }))
                    .into_any_element()
            }
            PopupRow::Branch {
                section,
                depth,
                label,
                item,
                is_favorite,
            } => {
                let submenu_open = self.active_submenu.as_ref().is_some_and(|(cur, _)| {
                    matches!(
                        cur,
                        SubmenuTarget::Branch {
                            section: s,
                            item: b
                        } if *s == *section && b.full_name == item.full_name
                    )
                });
                let effective_bg = if submenu_open {
                    colors.element_selected
                } else {
                    row_bg
                };

                let tracking_text = item.tracking.as_ref().map(|tracking| {
                    let mut badges = Vec::new();
                    if let Some(a) = item.ahead {
                        badges.push(format!("↑{a}"));
                    }
                    if let Some(b) = item.behind {
                        badges.push(format!("↓{b}"));
                    }
                    if badges.is_empty() {
                        tracking.clone()
                    } else {
                        format!("{} {tracking}", badges.join(" "))
                    }
                });

                let star_branch = (!item.is_remote).then(|| item.full_name.clone());
                let is_head = item.is_head;
                let is_fav = *is_favorite;

                h_flex()
                    .id(("pwb-branch-row", ix))
                    .w_full()
                    .h(px(26.))
                    .pl(px(10. + (*depth as f32) * 14.))
                    .pr_2()
                    .gap_2()
                    .justify_between()
                    .rounded_sm()
                    .bg(effective_bg)
                    .cursor_pointer()
                    .hover(|s| s.bg(colors.element_hover))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .child(if is_head {
                                Icon::new(IconName::Check)
                                    .size(IconSize::Small)
                                    .color(Color::Accent)
                                    .into_any_element()
                            } else if let Some(branch_for_star) = star_branch {
                                div()
                                    .id(("pwb-star", ix))
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        cx.stop_propagation();
                                        this.favorites = toggle_favorite_in_store(
                                            &this.repo_key,
                                            &branch_for_star,
                                            cx,
                                        );
                                        this.rebuild_rows();
                                        cx.notify();
                                    }))
                                    .child(
                                        Icon::new(if is_fav {
                                            IconName::StarFilled
                                        } else {
                                            IconName::GitBranch
                                        })
                                        .size(IconSize::Small)
                                        .color(if is_fav {
                                            Color::Warning
                                        } else {
                                            Color::Muted
                                        }),
                                    )
                                    .into_any_element()
                            } else {
                                Icon::new(IconName::GitBranch)
                                    .size(IconSize::Small)
                                    .color(Color::Muted)
                                    .into_any_element()
                            })
                            .child(
                                Label::new(label.clone())
                                    .size(LabelSize::Small)
                                    .color(if is_head {
                                        Color::Accent
                                    } else {
                                        Color::Default
                                    })
                                    .truncate(),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1p5()
                            .flex_none()
                            .when_some(tracking_text, |el, tracking| {
                                el.child(
                                    Label::new(tracking)
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                            })
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_row(ix, true, window, cx);
                    }))
                    .into_any_element()
            }
            PopupRow::Tag { depth, name } => h_flex()
                .id(("pwb-tag-row", ix))
                .w_full()
                .h(px(26.))
                .pl(px(10. + (*depth as f32) * 14.))
                .pr_2()
                .gap_2()
                .justify_between()
                .rounded_sm()
                .bg(row_bg)
                .cursor_pointer()
                .hover(|s| s.bg(colors.element_hover))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .child(Icon::new(IconName::Hash).size(IconSize::Small).color(Color::Muted))
                        .child(Label::new(name.clone()).size(LabelSize::Small).truncate()),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.activate_row(ix, true, window, cx);
                }))
                .into_any_element(),
        }
    }

    fn render_submenu(&self, target: &SubmenuTarget, row_ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let items = self.build_submenu_items(target);
        let top_offset = ((row_ix.min(12) as f32) * 24.0 + 36.0).min(260.0);

        v_flex()
            .id("phpstorm-branch-submenu")
            .occlude()
            .w(rems(16.5))
            .mt(px(top_offset))
            .p_1()
            .gap_0p5()
            .elevation_3(cx)
            .bg(colors.elevated_surface_background)
            .border_1()
            .border_color(colors.border)
            .rounded_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(items.into_iter().enumerate().map(|(ix, item)| match item {
                SubmenuItem::Divider => div()
                    .px_1()
                    .py_1()
                    .child(Divider::horizontal())
                    .into_any_element(),
                SubmenuItem::Action(action) => {
                    let selected = self.submenu_selected == ix;
                    let handler = action.handler.clone();
                    h_flex()
                        .id((action.id, ix))
                        .w_full()
                        .h(px(26.))
                        .px_2()
                        .gap_2()
                        .rounded_sm()
                        .bg(if selected {
                            colors.element_selected
                        } else {
                            gpui::transparent_black()
                        })
                        .cursor_pointer()
                        .hover(|s| s.bg(colors.element_hover))
                        .child(Icon::new(action.icon).size(IconSize::Small).color(action.color))
                        .child(
                            Label::new(action.label)
                                .size(LabelSize::Small)
                                .color(action.color)
                                .truncate(),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            (handler)(this, window, cx);
                        }))
                        .into_any_element()
                }
            }))
            .into_any_element()
    }
}

impl Render for PhpStormBranchPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let submenu = self
            .active_submenu
            .as_ref()
            .map(|(target, row_ix)| self.render_submenu(target, *row_ix, cx));

        let main_card = v_flex()
            .id("phpstorm-branch-popup-card")
            .occlude()
            .w(rems(25.))
            .elevation_3(cx)
            .bg(colors.elevated_surface_background)
            .border_1()
            .border_color(colors.border)
            .rounded_lg()
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_2()
                    .gap_1p5()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h(px(28.))
                            .px_2()
                            .gap_1p5()
                            .rounded_md()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .child(
                                Icon::new(IconName::MagnifyingGlass)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.search_editor.clone()),
                            ),
                    )
                    .child(
                        IconButton::new("pwb-collapse-folders", IconName::ChevronUpDown)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Expand / Collapse Branch Folders"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.toggle_all_folders(cx);
                            })),
                    )
                    .child(
                        IconButton::new("pwb-fetch-remotes", IconName::RefreshTitle)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Fetch Remotes"))
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(Box::new(git::Fetch), cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("phpstorm-branch-popup-scroll")
                    .w_full()
                    .max_h(rems(32.))
                    .overflow_y_scroll()
                    .p_1()
                    .gap_0p5()
                    .children(
                        self.rows
                            .iter()
                            .enumerate()
                            .map(|(ix, row)| self.render_row(ix, row, cx)),
                    ),
            );

        h_flex()
            .items_start()
            .gap_1()
            .key_context({
                let mut key_context = KeyContext::new_with_defaults();
                key_context.add("Picker");
                key_context.add("GitBranchSelector");
                key_context
            })
            .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| {
                if this.active_submenu.take().is_some() {
                    cx.notify();
                } else {
                    cx.emit(DismissEvent);
                }
            }))
            .on_action(cx.listener(|this, _: &menu::SelectNext, _, cx| {
                this.move_selection(1, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectPrevious, _, cx| {
                this.move_selection(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::SelectChild, window, cx| {
                if this.active_submenu.is_some() {
                    return;
                }
                if let Some(ix) = this.selected_index {
                    if matches!(
                        this.rows.get(ix),
                        Some(PopupRow::Branch { .. } | PopupRow::Tag { .. })
                    ) {
                        this.activate_row(ix, false, window, cx);
                    } else if matches!(
                        this.rows.get(ix),
                        Some(
                            PopupRow::Folder { expanded: false, .. }
                                | PopupRow::SectionHeader { expanded: false, .. }
                        )
                    ) {
                        this.activate_row(ix, false, window, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &menu::SelectParent, window, cx| {
                if this.active_submenu.take().is_some() {
                    cx.notify();
                    return;
                }
                if let Some(ix) = this.selected_index {
                    if matches!(
                        this.rows.get(ix),
                        Some(
                            PopupRow::Folder { expanded: true, .. }
                                | PopupRow::SectionHeader { expanded: true, .. }
                        )
                    ) {
                        this.activate_row(ix, false, window, cx);
                    }
                }
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.handle_confirm(window, cx);
            }))
            .when(!self.modal_mode, |el| {
                el.on_mouse_down_out(cx.listener(|_, _, _, cx| {
                    cx.emit(DismissEvent);
                }))
            })
            .child(main_card)
            .when_some(submenu, |el, submenu| el.child(submenu))
    }
}

struct BranchTextPromptModal {
    title: SharedString,
    confirm_label: SharedString,
    editor: Entity<Editor>,
    on_confirm: Option<Box<dyn FnOnce(String, &mut Window, &mut App)>>,
}

impl EventEmitter<DismissEvent> for BranchTextPromptModal {}
impl ModalView for BranchTextPromptModal {}

impl Focusable for BranchTextPromptModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }
}

impl BranchTextPromptModal {
    #[allow(clippy::too_many_arguments)]
    fn open(
        workspace: WeakEntity<Workspace>,
        title: impl Into<SharedString>,
        initial_text: &str,
        placeholder: impl Into<SharedString>,
        confirm_label: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
        on_confirm: impl FnOnce(String, &mut Window, &mut App) + 'static,
    ) {
        let title = title.into();
        let placeholder = placeholder.into();
        let confirm_label = confirm_label.into();
        let initial_text = initial_text.to_string();
        let window_handle = window.window_handle();
        let callback: Box<dyn FnOnce(String, &mut Window, &mut App)> = Box::new(on_confirm);

        cx.defer(move |cx| {
            window_handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.toggle_modal(window, cx, |window, cx| {
                                let editor = cx.new(|cx| {
                                    let mut editor = Editor::single_line(window, cx);
                                    editor.set_placeholder_text(&placeholder, window, cx);
                                    if !initial_text.is_empty() {
                                        editor.set_text(initial_text, window, cx);
                                    }
                                    editor
                                });
                                Self {
                                    title,
                                    confirm_label,
                                    editor,
                                    on_confirm: Some(callback),
                                }
                            });
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.editor.read(cx).text(cx);
        if let Some(cb) = self.on_confirm.take() {
            cb(value, window, cx);
        }
        cx.emit(DismissEvent);
    }
}

impl Render for BranchTextPromptModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .key_context("RenameBranchModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_3(cx)
            .w(rems(30.))
            .p_3()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::GitBranch).size(IconSize::Small).color(Color::Accent))
                    .child(Label::new(self.title.clone()).size(LabelSize::Default)),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(colors.editor_background)
                    .border_1()
                    .border_color(colors.border)
                    .child(self.editor.clone()),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pwb-prompt-cancel", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|_, _, _window, cx| {
                                cx.emit(DismissEvent);
                            })),
                    )
                    .child(
                        Button::new("pwb-prompt-confirm", self.confirm_label.clone())
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&menu::Confirm, window, cx);
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_branch(name: &str, is_head: bool, is_remote: bool, ts: i64) -> BranchItem {
        BranchItem {
            full_name: name.to_string(),
            tracking: None,
            ahead: None,
            behind: None,
            is_head,
            is_remote,
            timestamp: ts,
        }
    }

    #[test]
    fn test_build_branch_tree_groups_slash_prefixes() {
        let items = vec![
            sample_branch("main", true, false, 100),
            sample_branch("djak/report-speedup", false, false, 90),
            sample_branch("djak/fix-login", false, false, 80),
        ];

        let tree = build_branch_tree(&items);
        assert_eq!(tree.len(), 2);

        match &tree[0] {
            BranchTreeNode::Folder {
                name,
                path,
                children,
            } => {
                assert_eq!(name, "djak");
                assert_eq!(path, "djak");
                assert_eq!(children.len(), 2);
            }
            other => panic!("expected folder first, got {other:?}"),
        }
        match &tree[1] {
            BranchTreeNode::Leaf { label, item } => {
                assert_eq!(label, "main");
                assert!(item.is_head);
            }
            other => panic!("expected main leaf second, got {other:?}"),
        }
    }

    #[test]
    fn test_select_recent_branches_prioritizes_head_favorites_and_timestamps() {
        let items = vec![
            sample_branch("old", false, false, 10),
            sample_branch("php83", false, false, 20),
            sample_branch("newest", false, false, 200),
            sample_branch("main", true, false, 50),
            sample_branch("origin/main", false, true, 500),
        ];
        let mut favs = HashSet::new();
        favs.insert("php83".to_string());

        let recent = select_recent_branches(&items, &favs, 3);
        let names: Vec<&str> = recent.iter().map(|b| b.full_name.as_str()).collect();
        assert_eq!(names, vec!["main", "php83", "newest"]);
    }

    #[test]
    fn test_matches_tokens_multi_word_case_insensitive() {
        assert!(matches_tokens("djak/report-speedup", "djak speed"));
        assert!(!matches_tokens("djak/report-speedup", "djak missing"));
        assert!(matches_tokens("main", ""));
    }
}
