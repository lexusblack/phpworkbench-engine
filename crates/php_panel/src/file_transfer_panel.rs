use gpui::*;
use ui::{prelude::*, Tooltip};
use workspace::dock::{DockPosition, Panel, PanelEvent};
use workspace::Workspace;
use crate::{
    DeployShared, DeployTransferGlobal, TransferLogEntry,
    OpenFileTransferLog,
};

pub struct FileTransferPanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    active_server_filter: Option<String>,
}

impl EventEmitter<PanelEvent> for FileTransferPanel {}

impl Focusable for FileTransferPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for FileTransferPanel {
    fn persistent_name() -> &'static str {
        "File Transfer"
    }

    fn panel_key() -> &'static str {
        "FileTransferPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Bottom
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Bottom | DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(220.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ArrowUpDown)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("File Transfer")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(OpenFileTransferLog)
    }

    fn activation_priority(&self) -> u32 {
        85
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        false
    }
}

impl FileTransferPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            cx.new(|cx| {
                let focus_handle = cx.focus_handle();
                let weak_self = cx.entity().downgrade();
                if cx.has_global::<DeployTransferGlobal>() {
                    cx.global_mut::<DeployTransferGlobal>().file_transfer_panel = Some(weak_self);
                } else {
                    cx.set_global(DeployTransferGlobal {
                        state: crate::DeployTransferState::Idle,
                        indicator: None,
                        file_transfer_panel: Some(weak_self),
                        history: Vec::new(),
                    });
                }
                Self {
                    workspace: workspace.weak_handle(),
                    focus_handle,
                    active_server_filter: None,
                }
            })
        })
    }
}

impl Render for FileTransferPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (history, active_server, is_auto_upload) = if cx.has_global::<DeployTransferGlobal>() {
            let global = cx.global::<DeployTransferGlobal>();
            let shared = if cx.has_global::<DeployShared>() {
                Some(cx.global::<DeployShared>())
            } else {
                None
            };
            let active = shared.and_then(|s| s.active.clone()).unwrap_or_else(|| "No Server".to_string());
            let auto = shared.map(|s| s.auto_upload).unwrap_or(false);
            (global.history.clone(), active, auto)
        } else {
            (Vec::new(), "No Server".to_string(), false)
        };

        // Collect unique servers from history
        let mut servers: Vec<String> = Vec::new();
        if active_server != "No Server" && !active_server.is_empty() {
            servers.push(active_server.clone());
        }
        for entry in &history {
            if !entry.server.is_empty() && !servers.contains(&entry.server) {
                servers.push(entry.server.clone());
            }
        }

        let filtered_history: Vec<&TransferLogEntry> = if let Some(ref filter) = self.active_server_filter {
            history.iter().filter(|e| &e.server == filter).collect()
        } else {
            history.iter().collect()
        };

        v_flex()
            .key_context("FileTransferPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().terminal_background)
            // Header bar
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .bg(cx.theme().colors().tab_bar_background)
                    // Left: Panel title + Server tabs
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            // Main title label
                            .child(
                                h_flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_2()
                                    .py_0p5()
                                    .child(Icon::new(IconName::ArrowUpDown).size(IconSize::Small).color(Color::Accent))
                                    .child(
                                        Label::new("File Transfer")
                                            .size(LabelSize::Small)
                                            .weight(FontWeight::BOLD)
                                            .color(Color::Default),
                                    ),
                            )
                            // "All" tab
                            .child(
                                div()
                                    .id("tab-all-servers")
                                    .cursor_pointer()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_xs()
                                    .when(self.active_server_filter.is_none(), |this| {
                                        this.bg(cx.theme().colors().element_selected)
                                    })
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.active_server_filter = None;
                                        cx.notify();
                                    }))
                                    .child(
                                        Label::new("All")
                                            .size(LabelSize::Small)
                                            .color(if self.active_server_filter.is_none() { Color::Default } else { Color::Muted }),
                                    ),
                            )
                            // Server tabs
                            .children(servers.into_iter().map(|srv| {
                                let is_active = self.active_server_filter.as_deref() == Some(&srv);
                                let srv_clone = srv.clone();
                                div()
                                    .id(SharedString::from(format!("tab-server-{}", srv)))
                                    .cursor_pointer()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_xs()
                                    .when(is_active, |this| {
                                        this.bg(cx.theme().colors().element_selected)
                                    })
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        if this.active_server_filter.as_deref() == Some(&srv_clone) {
                                            this.active_server_filter = None;
                                        } else {
                                            this.active_server_filter = Some(srv_clone.clone());
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                Label::new(srv)
                                                    .size(LabelSize::Small)
                                                    .color(if is_active { Color::Default } else { Color::Muted }),
                                            ),
                                    )
                            })),
                    )
                    // Right: Actions (Auto-upload, Clear, Close)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            // Auto-upload pill
                            .child(
                                div()
                                    .id("auto-upload-toggle-pill")
                                    .cursor_pointer()
                                    .px_1p5()
                                    .py_0p5()
                                    .rounded_xs()
                                    .bg(if is_auto_upload { gpui::rgb(0x1d3a2e) } else { gpui::rgb(0x3a2e1d) })
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        if cx.has_global::<DeployShared>() {
                                            let current = cx.global::<DeployShared>().auto_upload;
                                            cx.global_mut::<DeployShared>().auto_upload = !current;
                                            cx.notify();
                                        }
                                    }))
                                    .child(
                                        Label::new(if is_auto_upload { "Auto-upload: ON" } else { "Auto-upload: OFF" })
                                            .size(LabelSize::Custom(Rems(0.72)))
                                            .color(if is_auto_upload { Color::Success } else { Color::Warning }),
                                    ),
                            )
                            // Clear Log button
                            .child(
                                IconButton::new("clear-transfer-log", IconName::Trash)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Clear Log"))
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        if cx.has_global::<DeployTransferGlobal>() {
                                            cx.global_mut::<DeployTransferGlobal>().history.clear();
                                            cx.notify();
                                        }
                                    })),
                            )
                            // Close bottom dock button
                            .child(
                                IconButton::new("close-transfer-panel", IconName::Close)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Hide Tool Window (Esc)"))
                                    .on_click(cx.listener(|_this, _, window, cx| {
                                        window.dispatch_action(Box::new(workspace::ToggleBottomDock), cx);
                                    })),
                            ),
                    ),
            )
            // Log body
            .child(
                v_flex()
                    .id("file-transfer-console-scroll")
                    .size_full()
                    .flex_1()
                    .overflow_y_scroll()
                    .font_buffer(cx)
                    .p_2p5()
                    .gap_1()
                    .when(filtered_history.is_empty(), |this| {
                        this.child(
                            div()
                                .p_4()
                                .items_center()
                                .child(
                                    Label::new("No file transfer logs. Saved file changes and manual uploads will appear here.")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                ),
                        )
                    })
                    .children(filtered_history.into_iter().enumerate().map(|(idx, entry)| {
                        let is_err = !entry.success;
                        let text_color = if is_err {
                            Color::Error
                        } else if entry.message.starts_with("Upload to") && entry.message.contains("completed") {
                            Color::Success
                        } else if entry.message.starts_with("Create folder") {
                            Color::Accent
                        } else {
                            Color::Default
                        };

                        h_flex()
                            .id(SharedString::from(format!("log-entry-{}", idx)))
                            .items_start()
                            .gap_2()
                            .child(
                                Label::new(format!("[{}]", entry.timestamp))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(
                                Label::new(entry.message.clone())
                                    .size(LabelSize::Small)
                                    .color(text_color),
                            )
                    })),
            )
    }
}
