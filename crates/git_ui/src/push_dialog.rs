use askpass::AskPassDelegate;
use git::repository::{CommitSummary, PushOptions, UpstreamTracking};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Task,
    Window, div, rems,
};
use git_ui_core::askpass_modal::AskPassModal;
use project::git_store::Repository;
use time::OffsetDateTime;
use ui::{
    Banner, Button, ButtonStyle, Color, Headline, HeadlineSize, Icon, IconName, IconSize, Label,
    LabelSize, Severity, Tooltip, prelude::*,
};
use workspace::{ModalView, Workspace};

use crate::phpstorm::{OutgoingCommitRow, PushDialogState, push_target_label};

gpui::actions!(push_dialog, [OpenPushDialog]);

pub struct PushDialog {
    workspace: gpui::WeakEntity<Workspace>,
    repository: Entity<Repository>,
    focus_handle: FocusHandle,
    state: PushDialogState,
    commits: Vec<CommitSummary>,
    loading: bool,
    load_error: Option<SharedString>,
    repo_name: SharedString,
    branch_name: SharedString,
    remote_name: SharedString,
    remote_branch_name: SharedString,
    has_upstream: bool,
    has_branch: bool,
    _load_task: Task<()>,
}

impl EventEmitter<DismissEvent> for PushDialog {}
impl ModalView for PushDialog {}

impl Focusable for PushDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PushDialog {
    /// Opens the Push dialog.
    ///
    /// The open is deferred because callers run inside a `GitPanel` (and thus
    /// workspace) update; updating the workspace entity synchronously here
    /// would double-lease it.
    pub fn open(
        workspace: gpui::WeakEntity<Workspace>,
        repository: Option<Entity<Repository>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(repository) = repository else {
            return;
        };
        let window_handle = window.window_handle();
        cx.defer(move |cx| {
            window_handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            let weak_workspace = workspace.weak_handle();
                            workspace.toggle_modal(window, cx, move |window, cx| {
                                PushDialog::new(weak_workspace, repository, window, cx)
                            });
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn new(
        workspace: gpui::WeakEntity<Workspace>,
        repository: Entity<Repository>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let (repo_name, branch_name, remote_name, remote_branch_name, has_upstream, has_branch) = {
            let repository = repository.read(cx);
            let repo_name = repository
                .work_directory_abs_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| repository.work_directory_abs_path.display().to_string());

            if let Some(branch) = repository.branch.as_ref() {
                let has_upstream = matches!(
                    branch.upstream.as_ref().map(|upstream| &upstream.tracking),
                    Some(UpstreamTracking::Tracked(_))
                );
                let remote_name = branch
                    .upstream
                    .as_ref()
                    .and_then(|upstream| upstream.remote_name())
                    .unwrap_or("origin")
                    .to_string();
                let remote_branch_name = branch
                    .upstream
                    .as_ref()
                    .and_then(|upstream| upstream.branch_name())
                    .unwrap_or_else(|| branch.name())
                    .to_string();
                (
                    SharedString::from(repo_name),
                    SharedString::from(branch.name().to_owned()),
                    SharedString::from(remote_name),
                    SharedString::from(remote_branch_name),
                    has_upstream,
                    true,
                )
            } else {
                (
                    SharedString::from(repo_name),
                    SharedString::from("(detached HEAD)"),
                    SharedString::from("origin"),
                    SharedString::from("(detached HEAD)"),
                    false,
                    false,
                )
            }
        };

        let receiver = repository.update(cx, |repository, _| repository.upstream_commits(100));
        let load_task = cx.spawn_in(window, async move |this, cx| {
            let result = receiver.await;
            this.update_in(cx, |this, _window, cx| {
                this.loading = false;
                match result {
                    Ok(Ok(commits)) => this.commits = commits,
                    Ok(Err(error)) => this.load_error = Some(format!("{error:#}").into()),
                    Err(error) => this.load_error = Some(format!("{error}").into()),
                }
                cx.notify();
            })
            .ok();
        });

        Self {
            workspace,
            repository,
            focus_handle,
            state: PushDialogState::Idle,
            commits: Vec::new(),
            loading: true,
            load_error: None,
            repo_name,
            branch_name,
            remote_name,
            remote_branch_name,
            has_upstream,
            has_branch,
            _load_task: load_task,
        }
    }

    fn askpass_delegate(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AskPassDelegate {
        let workspace = self.workspace.clone();
        let operation: SharedString = format!("git push {}", self.remote_name).into();
        let window = window.window_handle();
        AskPassDelegate::new_with_cancellation(
            &mut cx.to_async(),
            move |prompt, tx, cancellation, cx| {
                window
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.toggle_modal(window, cx, |window, cx| {
                                    AskPassModal::new(
                                        operation.clone(),
                                        prompt.into(),
                                        tx,
                                        cancellation,
                                        window,
                                        cx,
                                    )
                                });
                            })
                            .ok();
                    })
                    .ok();
            },
        )
    }

    fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_branch || !self.state.begin_push() {
            return;
        }
        cx.notify();

        let askpass = self.askpass_delegate(window, cx);
        let options = (!self.has_upstream).then_some(PushOptions::SetUpstream);
        let branch_name = self.branch_name.clone();
        let remote_branch_name = self.remote_branch_name.clone();
        let remote_name = self.remote_name.clone();
        let repository = self.repository.clone();

        let receiver = repository.update(cx, move |repository, cx| {
            repository.push(
                branch_name,
                remote_branch_name,
                remote_name,
                options,
                askpass,
                cx,
            )
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = receiver.await;
            this.update_in(cx, |this, _window, cx| {
                match result {
                    Ok(Ok(_)) => {
                        this.state.succeed();
                        cx.emit(DismissEvent);
                    }
                    Ok(Err(error)) => {
                        this.state.fail(format!("{error:#}"));
                    }
                    Err(error) => {
                        this.state.fail(format!("{error}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.push(window, cx);
    }

    fn render_commit_row(&self, commit: &CommitSummary, cx: &App) -> impl IntoElement {
        let short_sha: String = commit.sha.chars().take(7).collect();
        let row = OutgoingCommitRow {
            short_sha,
            summary: commit.subject.to_string(),
            author: commit.author_name.to_string(),
            relative_date: relative_time(commit.commit_timestamp),
        };

        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .hover(|this| this.bg(cx.theme().colors().element_hover))
            .child(
                Label::new(row.short_sha)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(row.summary)
                    .size(LabelSize::Small)
                    .truncate()
                    .flex_1(),
            )
            .child(
                Label::new(row.author)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(row.relative_date)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }
}

fn relative_time(timestamp: i64) -> String {
    let commit_time =
        OffsetDateTime::from_unix_timestamp(timestamp).unwrap_or_else(|_| OffsetDateTime::now_utc());
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    time_format::format_localized_timestamp(
        commit_time,
        OffsetDateTime::now_utc(),
        local_offset,
        time_format::TimestampFormat::Relative,
    )
}

impl Render for PushDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pushing = self.state.is_pushing();
        let target = push_target_label(
            &self.repo_name,
            &self.remote_name,
            &self.remote_branch_name,
            self.has_upstream,
        );

        v_flex()
            .id("push-dialog")
            .key_context("PushDialog")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .w(rems(38.))
            .elevation_3(cx)
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .overflow_hidden()
            .child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Icon::new(IconName::ExpandUp)
                                    .size(IconSize::Small)
                                    .color(Color::Accent),
                            )
                            .child(Headline::new("Push").size(HeadlineSize::Small)),
                    )
                    .child(
                        Label::new(target)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .when(!self.has_upstream && self.has_branch, |this| {
                this.child(
                    div().px_3().pt_2().child(
                        Banner::new().severity(Severity::Info).child(Label::new(format!(
                            "no upstream configured — push will set {}/{}",
                            self.remote_name, self.remote_branch_name
                        ))),
                    ),
                )
            })
            .child(
                v_flex()
                    .id("push-dialog-commits")
                    .max_h(rems(18.))
                    .overflow_y_scroll()
                    .p_2()
                    .gap_0p5()
                    .when(self.loading, |this| {
                        this.child(div().p_2().child(
                            Label::new("Loading outgoing commits…")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ))
                    })
                    .when_some(self.load_error.clone(), |this, error| {
                        this.child(div().p_2().child(
                            Label::new(error)
                                .size(LabelSize::Small)
                                .color(Color::Error),
                        ))
                    })
                    .when(!self.loading && self.load_error.is_none(), |this| {
                        if self.commits.is_empty() {
                            this.child(div().p_2().child(
                                Label::new("No commits to push.")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ))
                        } else {
                            this.children(
                                self.commits
                                    .iter()
                                    .map(|commit| self.render_commit_row(commit, cx)),
                            )
                        }
                    }),
            )
            .when_some(self.state.error().map(ToOwned::to_owned), |this, error| {
                this.child(div().px_3().pb_2().child(
                    Label::new(error)
                        .size(LabelSize::Small)
                        .color(Color::Error),
                ))
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Button::new("push-dialog-cancel", "Cancel")
                            .style(ButtonStyle::Outlined)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                let _ = this;
                                cx.emit(DismissEvent);
                            })),
                    )
                    .child(
                        Button::new("push-dialog-push", "Push")
                            .style(ButtonStyle::Filled)
                            .loading(pushing)
                            .disabled(pushing || !self.has_branch)
                            .tooltip(Tooltip::text("git push"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.push(window, cx);
                            })),
                    ),
            )
    }
}
