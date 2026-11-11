use std::path::{Path, PathBuf};
use std::time::Instant;

use editor::Editor;
use gpui::{
    App, Context, Entity, EventEmitter, FontWeight, IntoElement, ParentElement,
    Render, Styled, Subscription, Task, WeakEntity, Window,
    px, rgba,
};
use language::{Buffer, BufferEvent};
use ui::{
    Color, Icon, IconButton, IconName, IconSize, Label, LabelSize,
    Tooltip, prelude::*,
};
use util::ResultExt;
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteSyncStatus {
    Identical,
    Modified,
    Uploading,
    Uploaded,
    Failed(String),
}

pub struct ActiveRemoteFile {
    pub server: String,
    pub remote_path: String,
    pub local_path: PathBuf,
    pub root: PathBuf,
    pub status: RemoteSyncStatus,
    pub last_uploaded: Option<Instant>,
    pub editor: WeakEntity<Editor>,
    pub buffer: WeakEntity<Buffer>,
}

pub struct RemoteFileBanner {
    workspace: WeakEntity<Workspace>,
    active_file: Option<ActiveRemoteFile>,
    _buffer_subscription: Option<Subscription>,
    copied_path_instant: Option<Instant>,
}

impl EventEmitter<ToolbarItemEvent> for RemoteFileBanner {}

pub fn remote_temp_root() -> PathBuf {
    std::env::temp_dir().join("phpworkbench-remote")
}

pub fn remote_pristine_root() -> PathBuf {
    std::env::temp_dir().join("phpworkbench-remote-pristine")
}

/// Extract `(server_name, remote_path)` from any temp path created when opening a remote file.
pub fn parse_remote_path(path: &Path) -> Option<(String, String)> {
    let path_str = path.to_str()?.replace('\\', "/");
    let native_root = format!(
        "{}/",
        remote_temp_root()
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
    );
    let rest = if let Some(rest) = path_str.strip_prefix(&native_root) {
        rest
    } else if let Some(rest) = path_str.strip_prefix("/tmp/phpworkbench-remote/") {
        rest
    } else if let Some(rest) = path_str.strip_prefix("/private/tmp/phpworkbench-remote/") {
        rest
    } else {
        return None;
    };
    let mut parts = rest.splitn(2, '/');
    let server = parts.next()?;
    if server.is_empty() {
        return None;
    }
    let remote_rel = parts.next().unwrap_or("");
    let remote_path = format!("/{remote_rel}");
    Some((server.to_string(), remote_path))
}

pub fn compact_sftp_url(server: &str, path: &str) -> String {
    let full = format!("sftp://{server}{path}");
    if full.len() <= 45 {
        return full;
    }
    let p = Path::new(path);
    let file = p.file_name().and_then(|f| f.to_str()).unwrap_or(path);
    if let Some(parent) = p.parent().and_then(|p| p.file_name()).and_then(|f| f.to_str()) {
        if !parent.is_empty() && parent != "/" {
            return format!("sftp://{server}/…/{parent}/{file}");
        }
    }
    format!("sftp://{server}/…/{file}")
}

fn first_non_empty_line<'a>(iter: impl IntoIterator<Item = &'a str>) -> Option<String> {
    for line in iter {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

impl RemoteFileBanner {
    pub fn new(workspace: WeakEntity<Workspace>, _cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            active_file: None,
            _buffer_subscription: None,
            copied_path_instant: None,
        }
    }

    fn project_root(&self, cx: &App) -> PathBuf {
        self.workspace
            .upgrade()
            .and_then(|ws| {
                ws.read(cx)
                    .project()
                    .read(cx)
                    .visible_worktrees(cx)
                    .next()
                    .map(|w| w.read(cx).abs_path().to_path_buf())
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/Users/admin/phpstorm"))
    }

    fn clear_active_file(&mut self, cx: &mut Context<Self>) {
        if self.active_file.is_some() {
            self.active_file = None;
            self._buffer_subscription = None;
            self.copied_path_instant = None;
            cx.notify();
        }
    }

    fn setup_active_file(
        &mut self,
        server: String,
        remote_path: String,
        local_path: PathBuf,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
    ) {
        if let Some(active) = &self.active_file {
            if active.server == server
                && active.remote_path == remote_path
                && active.local_path == local_path
            {
                return;
            }
        }

        let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
            return;
        };

        let is_dirty = buffer.read(cx).is_dirty();
        let status = if is_dirty {
            RemoteSyncStatus::Modified
        } else {
            RemoteSyncStatus::Identical
        };
        let root = self.project_root(cx);

        let sub = cx.subscribe(&buffer, |this: &mut Self, _, event: &BufferEvent, cx| {
            match event {
                BufferEvent::Edited { .. } | BufferEvent::DirtyChanged => {
                    if let Some(active) = this.active_file.as_mut() {
                        if !matches!(active.status, RemoteSyncStatus::Uploading) {
                            active.status = RemoteSyncStatus::Modified;
                            cx.notify();
                        }
                    }
                }
                BufferEvent::Saved => {
                    // Auto-upload on save (Cmd+S)
                    this.upload_file(cx);
                }
                BufferEvent::Reloaded => {
                    if let Some(active) = this.active_file.as_mut() {
                        active.status = RemoteSyncStatus::Identical;
                        cx.notify();
                    }
                }
                _ => {}
            }
        });

        self._buffer_subscription = Some(sub);
        self.active_file = Some(ActiveRemoteFile {
            server,
            remote_path,
            local_path,
            root,
            status,
            last_uploaded: None,
            editor: editor.downgrade(),
            buffer: buffer.downgrade(),
        });
        cx.notify();
    }

    pub fn upload_file(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.active_file.as_mut() else {
            return;
        };
        active.status = RemoteSyncStatus::Uploading;
        cx.notify();

        let server = active.server.clone();
        let remote_path = active.remote_path.clone();
        let local_path = active.local_path.clone();
        let root = active.root.clone();

        let task: Task<Result<(), String>> = cx.background_spawn(async move {
            let output = std::process::Command::new(crate::resolve_helper_tool("php-deploy"))
                .arg("upload")
                .arg("--server")
                .arg(&server)
                .arg("--path")
                .arg(remote_path.trim_start_matches('/'))
                .arg("--from")
                .arg(&local_path)
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "upload failed".to_string()))
                }
                Err(e) => Err(e.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Some(active) = this.active_file.as_mut() {
                    match result {
                        Ok(()) => {
                            active.status = RemoteSyncStatus::Uploaded;
                            active.last_uploaded = Some(Instant::now());
                        }
                        Err(err) => {
                            active.status = RemoteSyncStatus::Failed(err);
                        }
                    }
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }

    pub fn refresh_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active_file.as_mut() else {
            return;
        };
        active.status = RemoteSyncStatus::Uploading;
        cx.notify();

        let server = active.server.clone();
        let remote_path = active.remote_path.clone();
        let local_path = active.local_path.clone();
        let root = active.root.clone();
        let buffer = active.buffer.clone();

        let task: Task<Result<(), String>> = cx.background_spawn(async move {
            let output = std::process::Command::new(crate::resolve_helper_tool("php-deploy"))
                .arg("download")
                .arg("--server")
                .arg(&server)
                .arg("--path")
                .arg(remote_path.trim_start_matches('/'))
                .arg("--to")
                .arg(&local_path)
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "refresh failed".to_string()))
                }
                Err(e) => Err(e.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if let Some(active) = this.active_file.as_mut() {
                    match result {
                        Ok(()) => {
                            active.status = RemoteSyncStatus::Identical;
                            if let Some(buffer) = buffer.upgrade() {
                                buffer.update(cx, |buffer, cx| {
                                    let _ = buffer.reload(cx);
                                });
                            }
                        }
                        Err(err) => {
                            active.status = RemoteSyncStatus::Failed(err);
                        }
                    }
                    cx.notify();
                }
            })
            .log_err();
        })
        .detach();
    }

    pub fn revert_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_file(window, cx);
    }

    pub fn compare_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = &self.active_file else {
            return;
        };
        let server = active.server.clone();
        let remote_path = active.remote_path.clone();
        let root = active.root.clone();
        let pristine_path = remote_pristine_root()
            .join(&server)
            .join(remote_path.trim_start_matches('/'));
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let task = cx.background_spawn({
            let pristine = pristine_path.clone();
            let server = server.clone();
            let remote_path = remote_path.clone();
            async move {
                if let Some(parent) = pristine.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::process::Command::new(crate::resolve_helper_tool("php-deploy"))
                    .arg("download")
                    .arg("--server")
                    .arg(&server)
                    .arg("--path")
                    .arg(remote_path.trim_start_matches('/'))
                    .arg("--to")
                    .arg(&pristine)
                    .current_dir(&root)
                    .output();
            }
        });

        cx.spawn_in(window, async move |_this, cx| {
            task.await;
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace
                        .split_abs_path(pristine_path, true, window, cx)
                        .detach_and_log_err(cx);
                })
                .log_err();
        })
        .detach();
    }
}

impl ToolbarItemView for RemoteFileBanner {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        if let Some(item) = active_pane_item {
            if let Some(editor) = item.act_as::<Editor>(cx) {
                let abs_path = editor.update(cx, |editor, cx| editor.target_file_abs_path(cx));
                if let Some(abs_path) = abs_path {
                    if let Some((server, remote_path)) = parse_remote_path(&abs_path) {
                        self.setup_active_file(server, remote_path, abs_path, editor, cx);
                        return ToolbarItemLocation::Secondary;
                    }
                }
            }
        }
        self.clear_active_file(cx);
        ToolbarItemLocation::Hidden
    }
}

impl Render for RemoteFileBanner {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active) = &self.active_file else {
            return gpui::div().into_any_element();
        };

        let active_server = active.server.clone();
        let active_path = active.remote_path.clone();
        let full_url = format!("sftp://{active_server}{active_path}");
        let display_url = compact_sftp_url(&active_server, &active_path);

        let just_copied = self
            .copied_path_instant
            .map_or(false, |t| t.elapsed().as_secs() < 3);

        let is_modified = matches!(&active.status, RemoteSyncStatus::Modified);

        let (status_text, status_color, status_icon) = match &active.status {
            RemoteSyncStatus::Identical => (
                "The file is identical to remote one.".to_string(),
                Color::Muted,
                IconName::Check,
            ),
            RemoteSyncStatus::Modified => (
                "The file has been modified.".to_string(),
                Color::Warning,
                IconName::Circle,
            ),
            RemoteSyncStatus::Uploading => (
                "Uploading to server…".to_string(),
                Color::Accent,
                IconName::RotateCw,
            ),
            RemoteSyncStatus::Uploaded => (
                "Uploaded to server ✓".to_string(),
                Color::Success,
                IconName::Check,
            ),
            RemoteSyncStatus::Failed(err) => (
                format!("Upload failed: {err}"),
                Color::Error,
                IconName::Close,
            ),
        };

        // PhpStorm blue banner styling: background #1a2a3e, border #284464
        h_flex()
            .id("remote-file-editor-banner")
            .w_full()
            .h(px(28.))
            .px_3()
            .items_center()
            .justify_between()
            .bg(rgba(0x1a2a3eff))
            .border_b_1()
            .border_color(rgba(0x284464ff))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Icon::new(IconName::Link)
                            .size(IconSize::XSmall)
                            .color(Color::Accent),
                    )
                    .child(
                        div()
                            .id("remote-banner-url-click")
                            .cursor_pointer()
                            .tooltip(Tooltip::text(format!("{full_url} — Click to copy path")))
                            .on_click(cx.listener({
                                let full = full_url.clone();
                                move |this, _, _window, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(full.clone()));
                                    this.copied_path_instant = Some(Instant::now());
                                    cx.notify();
                                }
                            }))
                            .child(
                                Label::new(display_url)
                                    .size(LabelSize::Small)
                                    .weight(FontWeight::BOLD)
                                    .color(Color::Default),
                            ),
                    )
                    .child(Label::new("—").color(Color::Muted))
                    .child(
                        if just_copied {
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    Icon::new(IconName::Check)
                                        .size(IconSize::XSmall)
                                        .color(Color::Success),
                                )
                                .child(
                                    Label::new("Copied SFTP path to clipboard!")
                                        .size(LabelSize::Small)
                                        .color(Color::Success),
                                )
                        } else if is_modified {
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    Icon::new(status_icon)
                                        .size(IconSize::XSmall)
                                        .color(status_color),
                                )
                                .child(
                                    Label::new("The file has been modified.")
                                        .size(LabelSize::Small)
                                        .color(status_color),
                                )
                                .child(
                                    Button::new("banner-upload-now", "Upload?")
                                        .style(ButtonStyle::Subtle)
                                        .label_size(LabelSize::Small)
                                        .color(Color::Accent)
                                        .tooltip(Tooltip::text(
                                            "Upload changes to remote server (Cmd+S also saves & uploads)",
                                        ))
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.upload_file(cx);
                                        })),
                                )
                        } else {
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .child(
                                    Icon::new(status_icon)
                                        .size(IconSize::XSmall)
                                        .color(status_color),
                                )
                                .child(
                                    Label::new(status_text)
                                        .size(LabelSize::Small)
                                        .color(status_color),
                                )
                        },
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    // 1. Refresh (⟳)
                    .child(
                        IconButton::new("remote-banner-refresh", IconName::RotateCw)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Download latest version from server (Refresh)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_file(window, cx);
                            })),
                    )
                    // 2. Compare with Remote (⇆)
                    .child(
                        IconButton::new("remote-banner-compare", IconName::ArrowRightLeft)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Compare with remote version on server (Diff)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.compare_file(window, cx);
                            })),
                    )
                    // 3. Revert (↶)
                    .child(
                        IconButton::new("remote-banner-revert", IconName::Undo)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Revert to remote version (Discard local changes)"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.revert_file(window, cx);
                            })),
                    )
                    // 4. Upload (⇡)
                    .child(
                        IconButton::new("remote-banner-upload", IconName::ArrowUp)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Upload to server (Cmd+S also uploads automatically)"))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.upload_file(cx);
                            })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_remote_path() {
        // Standard /tmp path from SFTP download
        let path1 = Path::new("/tmp/phpworkbench-remote/server1/home/gitlab-runner/builds/t3_7TY8kZ/0/lexusblack/asana/agent1_translations.json");
        let (server, remote_path) = parse_remote_path(path1).expect("should parse");
        assert_eq!(server, "server1");
        assert_eq!(
            remote_path,
            "/home/gitlab-runner/builds/t3_7TY8kZ/0/lexusblack/asana/agent1_translations.json"
        );

        // macOS canonicalized /private/tmp path
        let path2 = Path::new("/private/tmp/phpworkbench-remote/staging/var/www/index.php");
        let (server, remote_path) = parse_remote_path(path2).expect("should parse private tmp");
        assert_eq!(server, "staging");
        assert_eq!(remote_path, "/var/www/index.php");

        // Non-remote workspace file
        let path3 = Path::new("/Users/admin/phpstorm/vendor/zed/crates/php_panel/src/php_panel.rs");
        assert!(parse_remote_path(path3).is_none());

        // File at remote root
        let path4 = Path::new("/tmp/phpworkbench-remote/prod/README.md");
        let (server, remote_path) = parse_remote_path(path4).expect("should parse root file");
        assert_eq!(server, "prod");
        assert_eq!(remote_path, "/README.md");

        // Full SFTP url construction for clipboard and tooltip
        let full_sftp_url = format!("sftp://{}{}", server, remote_path);
        assert_eq!(full_sftp_url, "sftp://prod/README.md");
    }

    #[test]
    fn test_compact_sftp_url() {
        // Short path stays as is
        assert_eq!(compact_sftp_url("server1", "/var/www/app.php"), "sftp://server1/var/www/app.php");

        // Long path gets shortened with ellipsis
        let long_path = "/gitlab-runner/builds/t3_7TY8kZ/0/lexusblack/cleanny/database/factories/InventoryFactory.php";
        assert_eq!(
            compact_sftp_url("server1", long_path),
            "sftp://server1/…/factories/InventoryFactory.php"
        );
    }
}
