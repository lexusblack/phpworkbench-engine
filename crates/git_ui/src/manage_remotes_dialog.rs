use std::path::PathBuf;

use collections::HashMap;
use editor::Editor;
use gpui::{
    actions, div, px, rems, rgb, App, Context, DismissEvent, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Styled, Task, WeakEntity, Window,
};
use project::git_store::Repository;
use ui::{
    Button, ButtonStyle, Color, IconButton, IconName, IconSize,
    Label, LabelSize, Tooltip, prelude::*,
};
use workspace::{ModalView, Workspace};

actions!(manage_remotes, [ManageRemotes]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    None,
    Adding,
    Editing,
}

pub struct ManageRemotesDialog {
    _workspace: WeakEntity<Workspace>,
    repository: Option<Entity<Repository>>,
    work_dir: Option<PathBuf>,
    focus_handle: FocusHandle,
    remotes: Vec<RemoteEntry>,
    selected_index: Option<usize>,
    edit_mode: EditMode,
    name_editor: Entity<Editor>,
    url_editor: Entity<Editor>,
    loading: bool,
    error_message: Option<SharedString>,
    _load_task: Option<Task<()>>,
}

impl EventEmitter<DismissEvent> for ManageRemotesDialog {}
impl ModalView for ManageRemotesDialog {}

impl Focusable for ManageRemotesDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ManageRemotesDialog {
    pub fn open(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        work_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let window_handle = window.window_handle();
        cx.defer(move |cx| {
            window_handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let weak_workspace = workspace.weak_handle();
                            workspace.toggle_modal(window, cx, move |window, cx| {
                                ManageRemotesDialog::new(weak_workspace, repository, work_dir, window, cx)
                            });
                        })
                        .ok();
                })
                .ok();
        });
    }

    pub fn new(
        workspace: WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        work_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();

        let resolved_dir = repository
            .as_ref()
            .map(|repo| repo.read(cx).work_directory_abs_path.to_path_buf())
            .or(work_dir);

        let name_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Remote name (e.g. origin)", window, cx);
            editor
        });

        let url_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Git URL (e.g. git@github.com:user/repo.git)", window, cx);
            editor
        });

        let mut this = Self {
            _workspace: workspace,
            repository,
            work_dir: resolved_dir,
            focus_handle,
            remotes: Vec::new(),
            selected_index: None,
            edit_mode: EditMode::None,
            name_editor,
            url_editor,
            loading: true,
            error_message: None,
            _load_task: None,
        };

        this.load_remotes(window, cx);
        this
    }

    fn load_remotes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        self.error_message = None;

        let work_dir = self.work_dir.clone();
        let repo_handle = self.repository.clone();

        let task = cx.spawn_in(window, async move |this, cx| {
            let mut remotes_map: HashMap<String, String> = HashMap::default();

            // 1. Primary: Run `git remote -v` directly on background thread in work_dir
            if let Some(dir) = &work_dir {
                let dir_clone = dir.clone();
                let git_output = cx.background_executor().spawn(async move {
                    std::process::Command::new("git")
                        .current_dir(&dir_clone)
                        .args(["remote", "-v"])
                        .output()
                }).await;

                if let Ok(output) = git_output {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        for line in stdout.lines() {
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            if parts.len() >= 2 {
                                let name = parts[0].trim().to_string();
                                let url = parts[1].trim().to_string();
                                remotes_map.entry(name).or_insert(url);
                            }
                        }
                    }
                }
            }

            // 2. Fallback: Check repository snapshot for remote URLs
            if remotes_map.is_empty() {
                if let Some(repo) = repo_handle {
                    let snapshot_urls = this.read_with(cx, |_, cx| {
                        let repo_read = repo.read(cx);
                        (
                            repo_read.remote_origin_url.clone(),
                            repo_read.remote_upstream_url.clone(),
                        )
                    }).ok();

                    if let Some((origin_url, upstream_url)) = snapshot_urls {
                        if let Some(url) = origin_url {
                            remotes_map.insert("origin".to_string(), url);
                        }
                        if let Some(url) = upstream_url {
                            remotes_map.insert("upstream".to_string(), url);
                        }
                    }
                }
            }

            let mut entries: Vec<RemoteEntry> = remotes_map
                .into_iter()
                .map(|(name, url)| RemoteEntry { name, url })
                .collect();

            // Sort with 'origin' first, then alphabetical
            entries.sort_by(|a, b| {
                if a.name == "origin" {
                    std::cmp::Ordering::Less
                } else if b.name == "origin" {
                    std::cmp::Ordering::Greater
                } else {
                    a.name.cmp(&b.name)
                }
            });

            this.update_in(cx, |this, _window, cx| {
                this.remotes = entries;
                this.loading = false;
                if !this.remotes.is_empty() && this.selected_index.is_none() {
                    this.selected_index = Some(0);
                }
                cx.notify();
            }).ok();
        });

        self._load_task = Some(task);
    }

    fn start_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_mode = EditMode::Adding;
        self.error_message = None;

        let default_name = if self.remotes.iter().any(|r| r.name == "origin") {
            String::new()
        } else {
            "origin".to_string()
        };

        self.name_editor.update(cx, |editor, cx| {
            editor.set_text(default_name, window, cx);
        });
        self.url_editor.update(cx, |editor, cx| {
            editor.set_text("", window, cx);
        });

        self.name_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_index else { return; };
        let Some(entry) = self.remotes.get(ix).cloned() else { return; };

        self.edit_mode = EditMode::Editing;
        self.error_message = None;

        self.name_editor.update(cx, |editor, cx| {
            editor.set_text(entry.name, window, cx);
        });
        self.url_editor.update(cx, |editor, cx| {
            editor.set_text(entry.url, window, cx);
        });

        self.url_editor.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_index else { return; };
        let Some(entry) = self.remotes.get(ix).cloned() else { return; };

        let work_dir = self.work_dir.clone();
        let remote_name = entry.name.clone();

        cx.spawn_in(window, async move |this, cx| {
            if let Some(dir) = work_dir {
                let _ = cx.background_executor().spawn(async move {
                    std::process::Command::new("git")
                        .current_dir(&dir)
                        .args(["remote", "remove", &remote_name])
                        .output()
                }).await;
            }

            this.update_in(cx, |this, window, cx| {
                this.load_remotes(window, cx);
            }).ok();
        }).detach();
    }

    fn save_edit_or_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name_editor.read(cx).text(cx).trim().to_string();
        let url = self.url_editor.read(cx).text(cx).trim().to_string();

        if name.is_empty() {
            self.error_message = Some("Remote name cannot be empty".into());
            cx.notify();
            return;
        }

        if url.is_empty() {
            self.error_message = Some("Remote URL cannot be empty".into());
            cx.notify();
            return;
        }

        let mode = self.edit_mode;
        let work_dir = self.work_dir.clone();
        let prev_name = self.selected_index
            .and_then(|ix| self.remotes.get(ix))
            .map(|r| r.name.clone());

        self.edit_mode = EditMode::None;
        self.error_message = None;

        cx.spawn_in(window, async move |this, cx| {
            if let Some(dir) = work_dir {
                cx.background_executor().spawn(async move {
                    match mode {
                        EditMode::Adding => {
                            let _ = std::process::Command::new("git")
                                .current_dir(&dir)
                                .args(["remote", "add", &name, &url])
                                .output();
                        }
                        EditMode::Editing => {
                            let old_name = prev_name.unwrap_or_else(|| name.clone());
                            if old_name != name {
                                let _ = std::process::Command::new("git")
                                    .current_dir(&dir)
                                    .args(["remote", "rename", &old_name, &name])
                                    .output();
                            }
                            let _ = std::process::Command::new("git")
                                .current_dir(&dir)
                                .args(["remote", "set-url", &name, &url])
                                .output();
                        }
                        EditMode::None => {}
                    }
                }).await;
            }

            this.update_in(cx, |this, window, cx| {
                this.load_remotes(window, cx);
            }).ok();
        }).detach();
    }

    fn cancel_edit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit_mode = EditMode::None;
        self.error_message = None;
        cx.notify();
    }

    fn close(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl Render for ManageRemotesDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_selection = self.selected_index.is_some();
        let remotes_count = self.remotes.len();

        v_flex()
            .id("git-remotes-dialog")
            .key_context("ManageRemotesDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                if this.edit_mode != EditMode::None {
                    this.cancel_edit(window, cx);
                } else {
                    this.close(window, cx);
                }
            }))
            .w(rems(36.))
            .elevation_3(cx)
            .rounded_xl()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .overflow_hidden()
            // Header with macOS-style window controls & Title
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2p5()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .bg(cx.theme().colors().surface_background)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2p5()
                            // Traffic lights
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(div().w_2p5().h_2p5().rounded_full().bg(rgb(0xff5f56)))
                                    .child(div().w_2p5().h_2p5().rounded_full().bg(rgb(0xffbd2e)))
                                    .child(div().w_2p5().h_2p5().rounded_full().bg(rgb(0x27c93f))),
                            )
                            .child(
                                Label::new("Git Remotes")
                                    .size(LabelSize::Default)
                                    .weight(FontWeight::SEMIBOLD)
                                    .color(Color::Default),
                            ),
                    )
                    .child(
                        IconButton::new("close-remotes", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
            // Toolbar: + - edit
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .bg(cx.theme().colors().editor_background)
                    .child(
                        IconButton::new("add-remote-btn", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Add Remote (Alt+Insert)"))
                            .on_click(cx.listener(|this, _, window, cx| this.start_add(window, cx))),
                    )
                    .child(
                        IconButton::new("remove-remote-btn", IconName::Dash)
                            .icon_size(IconSize::Small)
                            .disabled(!has_selection)
                            .tooltip(Tooltip::text("Remove Remote (Delete)"))
                            .on_click(cx.listener(|this, _, window, cx| this.remove_selected(window, cx))),
                    )
                    .child(
                        IconButton::new("edit-remote-btn", IconName::Pencil)
                            .icon_size(IconSize::Small)
                            .disabled(!has_selection)
                            .tooltip(Tooltip::text("Edit Remote"))
                            .on_click(cx.listener(|this, _, window, cx| this.start_edit(window, cx))),
                    ),
            )
            // Main content table
            .child(
                v_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .child(
                        v_flex()
                            .w_full()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .overflow_hidden()
                            // Table Header: Name | URL
                            .child(
                                h_flex()
                                    .w_full()
                                    .bg(cx.theme().colors().tab_bar_background)
                                    .border_b_1()
                                    .border_color(cx.theme().colors().border)
                                    .child(
                                        div()
                                            .w(px(120.))
                                            .px_2()
                                            .py_1()
                                            .border_r_1()
                                            .border_color(cx.theme().colors().border)
                                            .child(
                                                Label::new("Name")
                                                    .size(LabelSize::Small)
                                                    .weight(FontWeight::MEDIUM)
                                                    .color(Color::Muted),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .px_2()
                                            .py_1()
                                            .child(
                                                Label::new("URL")
                                                    .size(LabelSize::Small)
                                                    .weight(FontWeight::MEDIUM)
                                                    .color(Color::Muted),
                                            ),
                                    ),
                            )
                            // Table Body Rows
                            .child(
                                v_flex()
                                    .id("git-remotes-table-body")
                                    .w_full()
                                    .min_h(rems(10.))
                                    .max_h(rems(16.))
                                    .overflow_y_scroll()
                                    .bg(cx.theme().colors().editor_background)
                                    .when(self.loading, |this| {
                                        this.child(
                                            div()
                                                .p_4()
                                                .items_center()
                                                .justify_center()
                                                .child(
                                                    Label::new("Reading Git remotes…")
                                                        .size(LabelSize::Small)
                                                        .color(Color::Muted),
                                                ),
                                        )
                                    })
                                    .when(!self.loading && remotes_count == 0, |this| {
                                        this.child(
                                            div()
                                                .p_4()
                                                .items_center()
                                                .justify_center()
                                                .child(
                                                    Label::new("No remotes configured. Click '+' to add one.")
                                                        .size(LabelSize::Small)
                                                        .color(Color::Muted),
                                                ),
                                        )
                                    })
                                    .when(!self.loading && remotes_count > 0, |this| {
                                        let selected_ix = self.selected_index;
                                        this.children(self.remotes.iter().enumerate().map(|(ix, entry)| {
                                            let is_selected = selected_ix == Some(ix);
                                            h_flex()
                                                .id(format!("remote-row-{}", ix))
                                                .w_full()
                                                .cursor_pointer()
                                                .when(is_selected, |this| {
                                                    this.bg(cx.theme().colors().element_selected)
                                                })
                                                .when(!is_selected, |this| {
                                                    this.hover(|h| h.bg(cx.theme().colors().element_hover))
                                                })
                                                .on_click(cx.listener(move |this, _, _window, cx| {
                                                    this.selected_index = Some(ix);
                                                    cx.notify();
                                                }))
                                                .child(
                                                    div()
                                                        .w(px(120.))
                                                        .px_2()
                                                        .py_1p5()
                                                        .border_r_1()
                                                        .border_color(cx.theme().colors().border_variant)
                                                        .child(
                                                            Label::new(entry.name.clone())
                                                                .size(LabelSize::Small)
                                                                .weight(FontWeight::MEDIUM)
                                                                .color(if is_selected { Color::Default } else { Color::Muted }),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .px_2()
                                                        .py_1p5()
                                                        .child(
                                                            Label::new(entry.url.clone())
                                                                .size(LabelSize::Small)
                                                                .color(if is_selected { Color::Default } else { Color::Muted }),
                                                        ),
                                                )
                                        }))
                                    }),
                            ),
                    )
                    // Inline Add / Edit Panel
                    .when(self.edit_mode != EditMode::None, |this| {
                        let is_add = self.edit_mode == EditMode::Adding;
                        this.child(
                            v_flex()
                                .w_full()
                                .p_2p5()
                                .gap_2()
                                .rounded_md()
                                .bg(cx.theme().colors().surface_background)
                                .border_1()
                                .border_color(cx.theme().colors().border_focused)
                                .child(
                                    Label::new(if is_add { "Add Remote" } else { "Edit Remote" })
                                        .size(LabelSize::Small)
                                        .weight(FontWeight::SEMIBOLD),
                                )
                                .when_some(self.error_message.clone(), |this, err| {
                                    this.child(
                                        Label::new(err)
                                            .size(LabelSize::Small)
                                            .color(Color::Error),
                                    )
                                })
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div().w(px(70.)).child(
                                                Label::new("Name:")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().colors().border)
                                                .bg(cx.theme().colors().editor_background)
                                                .px_2()
                                                .py_1()
                                                .child(self.name_editor.clone()),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            div().w(px(70.)).child(
                                                Label::new("URL:")
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(cx.theme().colors().border)
                                                .bg(cx.theme().colors().editor_background)
                                                .px_2()
                                                .py_1()
                                                .child(self.url_editor.clone()),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .pt_1()
                                        .child(
                                            Button::new("cancel-edit-btn", "Cancel")
                                                .style(ButtonStyle::Subtle)
                                                .on_click(cx.listener(|this, _, window, cx| this.cancel_edit(window, cx))),
                                        )
                                        .child(
                                            Button::new("save-edit-btn", if is_add { "Add" } else { "Save" })
                                                .style(ButtonStyle::Filled)
                                                .on_click(cx.listener(|this, _, window, cx| this.save_edit_or_add(window, cx))),
                                        ),
                                ),
                        )
                    }),
            )
            // Footer with OK and Cancel buttons
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .bg(cx.theme().colors().surface_background)
                    .child(
                        Button::new("remotes-cancel-btn", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        Button::new("remotes-ok-btn", "OK")
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
    }
}
