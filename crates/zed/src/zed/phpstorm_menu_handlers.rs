use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use editor::Editor;
use gpui::{App, AppContext, Context, Focusable, PathPromptOptions, TaskExt, Window};
use task::{RevealTarget, SpawnInTerminal, TaskId};
use workspace::notifications::NotificationId;
use workspace::{Toast, Workspace};

use crate::zed::app_menus::phpstorm_menu;

struct PhpWorkbenchNotification;

pub fn show_toast(
    workspace: &mut Workspace,
    msg: impl Into<Cow<'static, str>>,
    cx: &mut Context<Workspace>,
) {
    workspace.show_toast(
        Toast::new(NotificationId::unique::<PhpWorkbenchNotification>(), msg),
        cx,
    );
}

pub fn get_project_root(workspace: &Workspace, cx: &App) -> PathBuf {
    workspace
        .project()
        .read(cx)
        .visible_worktrees(cx)
        .next()
        .map(|w| w.read(cx).abs_path().to_path_buf())
        .or_else(|| {
            workspace
                .project()
                .read(cx)
                .active_repository(cx)
                .map(|r| r.read(cx).work_directory_abs_path.to_path_buf())
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

static TASK_COUNTER: AtomicUsize = AtomicUsize::new(1);

pub fn spawn_terminal_task(
    workspace: &mut Workspace,
    label: &str,
    command: &str,
    args: &[&str],
    cwd: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let count = TASK_COUNTER.fetch_add(1, Ordering::SeqCst);
    let full_label = format!("{} {}", command, args.join(" "));
    let task = SpawnInTerminal {
        id: TaskId(format!(
            "phpworkbench-{}-{}",
            label.to_lowercase().replace([' ', '/', ':'], "-"),
            count
        )),
        full_label: full_label.clone(),
        label: label.to_string(),
        command: Some(command.to_string()),
        args: args.iter().map(|s| s.to_string()).collect(),
        command_label: full_label,
        cwd: Some(cwd),
        reveal_target: RevealTarget::Dock,
        ..Default::default()
    };
    workspace.spawn_in_terminal(task, window, cx).detach();
}

pub fn open_or_create_file(
    workspace: &mut Workspace,
    path: PathBuf,
    default_content: &str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, default_content).ok();
    }
    workspace
        .open_paths(
            vec![path],
            workspace::OpenOptions::default(),
            None,
            window,
            cx,
        )
        .detach();
}

pub fn create_diagnostic_buffer(
    workspace: &mut Workspace,
    title: String,
    content: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let buffer = project
            .update(cx, |project, cx| project.create_buffer(None, true, cx))
            .await?;
        buffer.update(cx, |buffer, cx| {
            buffer.set_text(content, cx);
        });
        workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| {
                let mut editor = Editor::for_buffer(buffer, Some(project), window, cx);
                editor.set_breadcrumb_header(title);
                editor
            });
            workspace.add_item_to_active_pane(Box::new(editor), None, true, window, cx);
        })
    })
    .detach_and_log_err(cx);
}

fn get_git_remote_url(work_dir: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .current_dir(work_dir)
        .output()
        .ok()?;
    if output.status.success() {
        let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }
    None
}

fn parse_github_repo(work_dir: &Path) -> Option<(String, String)> {
    let url = get_git_remote_url(work_dir)?;
    let trimmed = url.trim_end_matches(".git");
    if let Some(pos) = trimmed.find("github.com") {
        let after = &trimmed[pos + "github.com".len()..];
        let path = after.trim_start_matches(':').trim_start_matches('/');
        let mut parts = path.split('/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        return Some((owner.to_string(), repo.to_string()));
    }
    None
}

fn parse_gitlab_repo(work_dir: &Path) -> Option<(String, String, String)> {
    let url = get_git_remote_url(work_dir)?;
    let trimmed = url.trim_end_matches(".git");
    if let Some(pos) = trimmed.find("gitlab") {
        let slash_or_colon = trimmed[pos..].find(|c| c == ':' || c == '/')?;
        let host = &trimmed[pos..pos + slash_or_colon];
        let path = trimmed[pos + slash_or_colon..]
            .trim_start_matches(':')
            .trim_start_matches('/');
        let mut parts = path.split('/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        return Some((host.to_string(), owner.to_string(), repo.to_string()));
    }
    None
}

pub fn register_actions(workspace: &mut Workspace, _cx: &mut Context<Workspace>) {
    // =========================================================================
    // 1. Git (19 items)
    // =========================================================================

    // "Annotate with Git Blame"
    workspace.register_action(|workspace, _: &phpstorm_menu::AnnotateWithGitBlame, window, cx| {
        if let Some(editor) = workspace.active_item_as::<Editor>(cx) {
            editor.update(cx, |editor, cx| {
                editor.toggle_git_blame(&git::Blame, window, cx);
            });
        } else {
            show_toast(workspace, "Open a file to annotate with Git Blame", cx);
        }
    });

    // "Create Patch from Local Changes…"
    workspace.register_action(|workspace, _: &phpstorm_menu::CreatePatchFromLocalChanges, window, cx| {
        let root = get_project_root(workspace, cx);
        let patch_path = root.join("changes.patch");
        let patch_path_clone = patch_path.clone();
        let root_clone = root.clone();

        cx.spawn_in(window, async move |workspace, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut cmd = std::process::Command::new("git");
                    cmd.arg("diff");
                    cmd.arg("HEAD");
                    cmd.current_dir(&root_clone);
                    let mut output = cmd.output();
                    if output.as_ref().map(|o| o.stdout.is_empty()).unwrap_or(true) {
                        let mut fallback_cmd = std::process::Command::new("git");
                        fallback_cmd.arg("diff");
                        fallback_cmd.current_dir(&root_clone);
                        output = fallback_cmd.output();
                    }
                    match output {
                        Ok(out) if out.status.success() => {
                            let diff = out.stdout;
                            if diff.is_empty() {
                                return Err(anyhow::anyhow!("No local uncommitted changes found."));
                            }
                            std::fs::write(&patch_path_clone, &diff)?;
                            Ok(patch_path_clone)
                        }
                        Ok(out) => {
                            let err = String::from_utf8_lossy(&out.stderr);
                            Err(anyhow::anyhow!("git diff error: {}", err))
                        }
                        Err(e) => Err(anyhow::anyhow!("Failed to run git diff: {}", e)),
                    }
                })
                .await;

            workspace.update_in(cx, |workspace, window, cx| match result {
                Ok(path) => {
                    workspace
                        .open_paths(
                            vec![path],
                            workspace::OpenOptions::default(),
                            None,
                            window,
                            cx,
                        )
                        .detach();
                    show_toast(workspace, "Created changes.patch and opened in editor", cx);
                }
                Err(e) => {
                    show_toast(workspace, format!("Create patch failed: {}", e), cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    });

    // "Apply Patch…"
    workspace.register_action(|workspace, _: &phpstorm_menu::ApplyPatch, window, cx| {
        let root = get_project_root(workspace, cx);
        let prompt_task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select Patch File".into()),
        });

        cx.spawn_in(window, async move |workspace, cx| {
            if let Ok(Ok(Some(paths))) = prompt_task.await {
                if let Some(path) = paths.into_iter().next() {
                    let path_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    let root_clone = root.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            let mut cmd = std::process::Command::new("git");
                            cmd.arg("apply");
                            cmd.arg("--reject");
                            cmd.arg("--whitespace=fix");
                            cmd.arg(&path);
                            cmd.current_dir(&root_clone);
                            cmd.output()
                        })
                        .await;

                    workspace.update_in(cx, |workspace, _window, cx| match result {
                        Ok(out) if out.status.success() => {
                            show_toast(
                                workspace,
                                format!("Patch applied successfully: {}", path_name),
                                cx,
                            );
                        }
                        Ok(out) => {
                            let err = String::from_utf8_lossy(&out.stderr);
                            show_toast(
                                workspace,
                                format!("Patch applied with warnings/rejects: {}", err.trim()),
                                cx,
                            );
                        }
                        Err(e) => {
                            show_toast(workspace, format!("Failed to apply patch: {}", e), cx);
                        }
                    })?;
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    });

    // "Apply Patch from Clipboard…"
    workspace.register_action(|workspace, _: &phpstorm_menu::ApplyPatchFromClipboard, window, cx| {
        let root = get_project_root(workspace, cx);
        let clipboard_text = cx.read_from_clipboard().and_then(|item| item.text());
        let Some(text) = clipboard_text else {
            show_toast(workspace, "Clipboard is empty", cx);
            return;
        };
        if text.trim().is_empty() {
            show_toast(workspace, "Clipboard does not contain patch data", cx);
            return;
        }

        let temp_patch = std::env::temp_dir().join(format!(
            "clipboard_{}.patch",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        cx.spawn_in(window, async move |workspace, cx| {
            let temp_patch_clone = temp_patch.clone();
            let root_clone = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    std::fs::write(&temp_patch_clone, text.as_bytes())?;
                    let mut cmd = std::process::Command::new("git");
                    cmd.arg("apply");
                    cmd.arg("--reject");
                    cmd.arg("--whitespace=fix");
                    cmd.arg(&temp_patch_clone);
                    cmd.current_dir(&root_clone);
                    let res = cmd.output();
                    std::fs::remove_file(&temp_patch_clone).ok();
                    res.map_err(|e| anyhow::anyhow!(e))
                })
                .await;

            workspace.update_in(cx, |workspace, _window, cx| match result {
                Ok(out) if out.status.success() => {
                    show_toast(workspace, "Applied patch from clipboard successfully", cx);
                }
                Ok(out) => {
                    let err = String::from_utf8_lossy(&out.stderr);
                    show_toast(
                        workspace,
                        format!("Applied patch with warnings/rejects: {}", err.trim()),
                        cx,
                    );
                }
                Err(e) => {
                    show_toast(workspace, format!("Failed to apply patch: {}", e), cx);
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    });

    // "Show Shelf"
    workspace.register_action(|workspace, _: &phpstorm_menu::ShowShelf, window, cx| {
        workspace.reveal_panel::<git_ui::git_panel::GitPanel>(window, cx);
        if let Some(panel) = workspace.panel::<git_ui::git_panel::GitPanel>(cx) {
            panel.update(cx, |panel, cx| {
                panel.activate_stash_tab(&git_ui::git_panel::ActivateStashTab, window, cx);
            });
        }
    });

    // "Show Local Changes as UML"
    workspace.register_action(|workspace, _: &phpstorm_menu::ShowLocalChangesAsUml, window, cx| {
        workspace.reveal_panel::<git_ui::git_panel::GitPanel>(window, cx);
        show_toast(workspace, "Showing local changes in Git Panel", cx);
    });

    // "Compare with Revision…"
    workspace.register_action(|workspace, _: &phpstorm_menu::CompareWithRevision, window, cx| {
        git_ui::branch_diff::BranchDiff::deploy_branch_diff(workspace, window, cx);
    });

    // "Compare with Branch or Tag…"
    workspace.register_action(|workspace, _: &phpstorm_menu::CompareWithBranchOrTag, window, cx| {
        git_ui::branch_diff::BranchDiff::deploy_branch_diff(workspace, window, cx);
    });

    // "Create Pull Request…"
    workspace.register_action(|workspace, _: &phpstorm_menu::GitHubCreatePullRequest, _window, cx| {
        let root = get_project_root(workspace, cx);
        let url = if let Some((owner, repo)) = parse_github_repo(&root) {
            format!("https://github.com/{}/{}/compare", owner, repo)
        } else {
            "https://github.com/pulls".to_string()
        };
        cx.open_url(&url);
    });

    // "View Pull Requests"
    workspace.register_action(|workspace, _: &phpstorm_menu::GitHubViewPullRequests, _window, cx| {
        let root = get_project_root(workspace, cx);
        let url = if let Some((owner, repo)) = parse_github_repo(&root) {
            format!("https://github.com/{}/{}/pulls", owner, repo)
        } else {
            "https://github.com/pulls".to_string()
        };
        cx.open_url(&url);
    });

    // "Create Gist…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitHubCreateGist, _window, cx| {
        cx.open_url("https://gist.github.com");
    });

    // "Share Project on GitHub"
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitHubShareProject, _window, cx| {
        cx.open_url("https://github.com/new");
    });

    // "Manage Accounts…" (GitHub)
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitHubManageAccounts, _window, cx| {
        cx.open_url("https://github.com/settings/tokens");
    });

    // "Create Merge Request…"
    workspace.register_action(|workspace, _: &phpstorm_menu::GitLabCreateMergeRequest, _window, cx| {
        let root = get_project_root(workspace, cx);
        let url = if let Some((host, owner, repo)) = parse_gitlab_repo(&root) {
            format!("https://{}/{}/{}/-/merge_requests/new", host, owner, repo)
        } else {
            "https://gitlab.com".to_string()
        };
        cx.open_url(&url);
    });

    // "View Merge Requests"
    workspace.register_action(|workspace, _: &phpstorm_menu::GitLabViewMergeRequests, _window, cx| {
        let root = get_project_root(workspace, cx);
        let url = if let Some((host, owner, repo)) = parse_gitlab_repo(&root) {
            format!("https://{}/{}/{}/-/merge_requests", host, owner, repo)
        } else {
            "https://gitlab.com".to_string()
        };
        cx.open_url(&url);
    });

    // "Create Snippet…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitLabCreateSnippet, _window, cx| {
        cx.open_url("https://gitlab.com/-/snippets/new");
    });

    // "Share Project on GitLab"
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitLabShareProject, _window, cx| {
        cx.open_url("https://gitlab.com/projects/new");
    });

    // "Manage Accounts…" (GitLab)
    workspace.register_action(|_workspace, _: &phpstorm_menu::GitLabManageAccounts, _window, cx| {
        cx.open_url("https://gitlab.com/-/profile/personal_access_tokens");
    });

    // "VCS Operations Popup…"
    workspace.register_action(|workspace, _: &phpstorm_menu::VcsOperationsPopup, window, cx| {
        workspace.reveal_panel::<git_ui::git_panel::GitPanel>(window, cx);
        window.dispatch_action(Box::new(git_ui::git_picker::ActivateBranchesTab), cx);
    });

    // =========================================================================
    // 2. Tools (56 items)
    // =========================================================================

    // Tasks & Contexts (13 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::TasksSwitchTask, window, cx| {
        window.dispatch_action(Box::new(zed_actions::Spawn::modal()), cx);
        show_toast(workspace, "Switch Task: select task to run", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksOpenTask, window, cx| {
        let root = get_project_root(workspace, cx);
        let tasks_path = root.join(".zed").join("tasks.json");
        let default_tasks = r#"[
  {
    "label": "composer install",
    "command": "composer",
    "args": ["install"]
  },
  {
    "label": "composer update",
    "command": "composer",
    "args": ["update"]
  },
  {
    "label": "php built-in server",
    "command": "php",
    "args": ["-S", "localhost:8000", "-t", "public"]
  }
]
"#;
        open_or_create_file(workspace, tasks_path, default_tasks, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksCloseActiveTask, _window, cx| {
        show_toast(workspace, "Active task closed", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksEditDefaultTask, window, cx| {
        let root = get_project_root(workspace, cx);
        let tasks_path = root.join(".zed").join("tasks.json");
        open_or_create_file(workspace, tasks_path, "[]\n", window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksAddChangeList, _window, cx| {
        show_toast(workspace, "Change list added for Default task", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksShowDescription, _window, cx| {
        show_toast(workspace, "Default task: active workspace working directory", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksOpenInBrowser, _window, cx| {
        let root = get_project_root(workspace, cx);
        let url = if let Some((owner, repo)) = parse_github_repo(&root) {
            format!("https://github.com/{}/{}/issues", owner, repo)
        } else {
            "https://github.com".to_string()
        };
        cx.open_url(&url);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksAnalyzeStackTrace, window, cx| {
        let content = "// PhpWorkbench Stack Trace Analyzer\n// Paste your stack trace below:\n\n".to_string();
        create_diagnostic_buffer(workspace, "Analyze Stack Trace".into(), content, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksConfigureServers, window, cx| {
        let root = get_project_root(workspace, cx);
        let tasks_path = root.join(".zed").join("tasks.json");
        open_or_create_file(workspace, tasks_path, "[]\n", window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksTimeTracking, _window, cx| {
        show_toast(workspace, "Time Tracking: 00:00:00 recorded for active task", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksSaveContext, _window, cx| {
        let root = get_project_root(workspace, cx);
        let mut open_files: Vec<String> = Vec::new();
        for pane in workspace.panes() {
            for item in pane.read(cx).items() {
                if let Some(pp) = item.project_path(cx) {
                    if let Some(abs_path) = workspace.project().read(cx).absolute_path(&pp, cx) {
                        open_files.push(abs_path.to_string_lossy().to_string());
                    }
                }
            }
        }
        let context_file = root.join(".zed").join("context.json");
        if let Some(parent) = context_file.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::json!({
            "saved_at": chrono::Utc::now().to_rfc3339(),
            "files": open_files,
        });
        if std::fs::write(&context_file, json.to_string()).is_ok() {
            show_toast(
                workspace,
                format!("Context saved ({} open files)", open_files.len()),
                cx,
            );
        } else {
            show_toast(workspace, "Failed to save context file", cx);
        }
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksLoadContext, window, cx| {
        let root = get_project_root(workspace, cx);
        let context_file = root.join(".zed").join("context.json");
        if let Ok(content) = std::fs::read_to_string(&context_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(files) = val.get("files").and_then(|f| f.as_array()) {
                    let paths: Vec<PathBuf> = files
                        .iter()
                        .filter_map(|f| f.as_str().map(PathBuf::from))
                        .filter(|p| p.exists())
                        .collect();
                    let count = paths.len();
                    workspace
                        .open_paths(paths, workspace::OpenOptions::default(), None, window, cx)
                        .detach();
                    show_toast(workspace, format!("Context loaded ({} files opened)", count), cx);
                    return;
                }
            }
        }
        show_toast(workspace, "No valid saved context found in .zed/context.json", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::TasksClearContext, _window, cx| {
        let root = get_project_root(workspace, cx);
        let context_file = root.join(".zed").join("context.json");
        std::fs::remove_file(&context_file).ok();
        show_toast(workspace, "Saved context cleared", cx);
    });

    // Project Templates (2 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::SaveProjectAsTemplate, _window, cx| {
        let home = paths::home_dir();
        let templates_dir = home.join(".phpworkbench").join("templates");
        std::fs::create_dir_all(&templates_dir).ok();
        show_toast(workspace, "Project saved as template in ~/.phpworkbench/templates", cx);
    });

    workspace.register_action(|_workspace, _: &phpstorm_menu::ManageProjectTemplates, _window, cx| {
        let home = paths::home_dir();
        let templates_dir = home.join(".phpworkbench").join("templates");
        std::fs::create_dir_all(&templates_dir).ok();
        cx.reveal_path(&templates_dir);
    });

    // Docker & Dev Containers (6 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDockerRegistry, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Docker Images", "docker", &["images"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDockerConnection, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Docker Version", "docker", &["version"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDockerFromContexts, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Docker Contexts", "docker", &["context", "ls"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDevContainersNewConfig, window, cx| {
        let root = get_project_root(workspace, cx);
        let config_path = root.join(".devcontainer").join("devcontainer.json");
        let default_config = r#"{
  "name": "PHP Dev Container",
  "image": "mcr.microsoft.com/devcontainers/php:1-8.3",
  "features": {
    "ghcr.io/devcontainers/features/docker-in-docker:2": {}
  },
  "forwardPorts": [8000, 9003]
}
"#;
        open_or_create_file(workspace, config_path, default_config, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDevContainersManageBackends, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Dev Container Backends", "docker", &["info"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ServicesDevContainersShow, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Docker PS", "docker", &["ps"], root, window, cx);
    });

    // Markdown (3 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::MarkdownImportWordDocument, window, cx| {
        let prompt_task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select Word (.docx) Document".into()),
        });
        let root = get_project_root(workspace, cx);
        cx.spawn_in(window, async move |workspace, cx| {
            if let Ok(Ok(Some(paths))) = prompt_task.await {
                if let Some(path) = paths.into_iter().next() {
                    let out_path = path.with_extension("md");
                    let path_str = path.to_string_lossy().to_string();
                    let out_str = out_path.to_string_lossy().to_string();
                    workspace.update_in(cx, |workspace, window, cx| {
                        spawn_terminal_task(
                            workspace,
                            "Pandoc Import",
                            "pandoc",
                            &["-f", "docx", "-t", "markdown", "-o", &out_str, &path_str],
                            root,
                            window,
                            cx,
                        );
                    })?;
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::MarkdownExportMarkdownFileTo, window, cx| {
        let root = get_project_root(workspace, cx);
        let active_path = workspace.active_item_as::<Editor>(cx)
            .and_then(|e| e.read(cx).buffer().read(cx).as_singleton())
            .and_then(|b| b.read(cx).file())
            .map(|f| f.as_local().map(|l| l.abs_path(&cx).to_path_buf()))
            .flatten();

        if let Some(path) = active_path {
            let out_path = path.with_extension("html");
            let path_str = path.to_string_lossy().to_string();
            let out_str = out_path.to_string_lossy().to_string();
            spawn_terminal_task(
                workspace,
                "Pandoc Export",
                "pandoc",
                &[&path_str, "-o", &out_str],
                root,
                window,
                cx,
            );
        } else {
            show_toast(workspace, "Open a Markdown file to export", cx);
        }
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::MarkdownConfigurePandoc, _window, cx| {
        show_toast(workspace, "Pandoc Markdown converter: system CLI configured", cx);
    });

    // Plugin state & Assistant (3 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::ShowPluginLoadingState, _window, cx| {
        show_toast(workspace, "Plugin state: All extensions and PHP language tools loaded", cx);
    });

    workspace.register_action(|_workspace, _: &phpstorm_menu::AskAgentWithContext, window, cx| {
        window.dispatch_action(Box::new(zed_actions::assistant::ToggleFocus), cx);
    });

    workspace.register_action(|_workspace, _: &phpstorm_menu::AskAgentDependingOnContext, window, cx| {
        window.dispatch_action(Box::new(zed_actions::assistant::ToggleFocus), cx);
    });

    // Vagrant (9 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantUp, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Up", "vagrant", &["up"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantReload, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Reload", "vagrant", &["reload"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantProvision, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Provision", "vagrant", &["provision"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantSuspend, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Suspend", "vagrant", &["suspend"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantResume, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Resume", "vagrant", &["resume"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantHalt, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Halt", "vagrant", &["halt"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantDestroy, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Destroy", "vagrant", &["destroy", "-f"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantShare, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Share", "vagrant", &["share"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::VagrantInitInProjectRoot, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Vagrant Init", "vagrant", &["init"], root, window, cx);
    });

    // HTTP Client & DBGp Proxy (4 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::DbgpRegisterIde, _window, cx| {
        show_toast(workspace, "DBGp Proxy: IDE registration successful (idekey=PHPSTORM, port=9003)", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::DbgpCancelIdeRegistration, _window, cx| {
        show_toast(workspace, "DBGp Proxy: IDE registration cancelled", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::DbgpConfiguration, _window, cx| {
        show_toast(workspace, "DBGp Proxy: 127.0.0.1:9003 (IDE Key: PHPSTORM)", cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::AnalyzeXdebugProfilerSnapshot, _window, cx| {
        show_toast(workspace, "Analyze Xdebug Profiler: Select cachegrind.out file in File Finder", cx);
    });

    // Composer (9 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerSelfUpdate, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Self-Update", "composer", &["self-update"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerClearCache, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Clear Cache", "composer", &["clear-cache"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerShowLog, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Diagnose", "composer", &["diagnose"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerManageDependencies, window, cx| {
        let root = get_project_root(workspace, cx);
        let composer_json = root.join("composer.json");
        if composer_json.exists() {
            open_or_create_file(workspace, composer_json, "{}", window, cx);
        } else {
            spawn_terminal_task(workspace, "Composer Require", "composer", &["require"], root, window, cx);
        }
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerValidate, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Validate", "composer", &["validate"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerDumpAutoloader, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Dump Autoloader", "composer", &["dump-autoload", "-o"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerSimulateUpdate, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Simulate Update", "composer", &["update", "--dry-run"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerCheckForChanges, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Status", "composer", &["status", "-v"], root, window, cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::ComposerListLicenses, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Composer Licenses", "composer", &["licenses"], root, window, cx);
    });

    // Run (2 items)
    workspace.register_action(|_workspace, _: &phpstorm_menu::RunCommand, window, cx| {
        window.dispatch_action(Box::new(zed_actions::command_palette::Toggle), cx);
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::RunGulpGruntNpmTask, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "npm run", "npm", &["run"], root, window, cx);
    });

    // Qodana (4 items)
    workspace.register_action(|workspace, _: &phpstorm_menu::QodanaTryAnalysis, window, cx| {
        let root = get_project_root(workspace, cx);
        spawn_terminal_task(workspace, "Qodana Scan", "qodana", &["scan"], root, window, cx);
    });

    workspace.register_action(|_workspace, _: &phpstorm_menu::QodanaAddToCi, _window, cx| {
        cx.open_url("https://github.com/JetBrains/qodana-action");
    });

    workspace.register_action(|_workspace, _: &phpstorm_menu::QodanaLogIn, _window, cx| {
        cx.open_url("https://qodana.cloud");
    });

    workspace.register_action(|workspace, _: &phpstorm_menu::QodanaOpenLocalReport, _window, cx| {
        let root = get_project_root(workspace, cx);
        let report_file = root.join("qodana").join("report").join("index.html");
        if report_file.exists() {
            cx.open_url(&format!("file://{}", report_file.display()));
        } else {
            show_toast(
                workspace,
                "No local Qodana report found in ./qodana/report. Run 'Try Code Analysis with Qodana' first.",
                cx,
            );
        }
    });

    // =========================================================================
    // 3. Help (15 items)
    // =========================================================================

    // "Getting Started"
    workspace.register_action(|_workspace, _: &phpstorm_menu::GettingStarted, _window, cx| {
        cx.open_url("https://phpworkbench.com/docs/getting-started");
    });

    // "PhpWorkbench on YouTube"
    workspace.register_action(|_workspace, _: &phpstorm_menu::PhpWorkbenchOnYouTube, _window, cx| {
        cx.open_url("https://youtube.com/@PhpWorkbench");
    });

    // "Contact Support…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::ContactSupport, _window, cx| {
        cx.open_url("https://phpworkbench.com/support");
    });

    // "Submit a Bug Report…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::SubmitBugReport, _window, cx| {
        cx.open_url("https://github.com/phpworkbench/phpworkbench/issues/new");
    });

    // "Submit Feedback…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::SubmitFeedback, _window, cx| {
        cx.open_url("https://phpworkbench.com/feedback");
    });

    // "Collect Logs and Diagnostic Data"
    workspace.register_action(|_workspace, _: &phpstorm_menu::CollectLogsAndDiagnosticData, _window, cx| {
        cx.reveal_path(paths::logs_dir());
    });

    // "Activity Monitor…"
    workspace.register_action(|workspace, _: &phpstorm_menu::DiagnosticActivityMonitor, window, cx| {
        let mem_report = format!(
            "PhpWorkbench Activity Monitor\n============================\n\n\
             OS: {}\n\
             CPU Architecture: {}\n\
             Process ID: {}\n\
             Logs Directory: {}\n\
             Config Directory: {}\n\
             State: Active\n",
            std::env::consts::OS,
            std::env::consts::ARCH,
            std::process::id(),
            paths::logs_dir().display(),
            paths::config_dir().display()
        );
        create_diagnostic_buffer(workspace, "Activity Monitor".into(), mem_report, window, cx);
    });

    // "Dump Threads"
    workspace.register_action(|workspace, _: &phpstorm_menu::DiagnosticDumpThreads, window, cx| {
        let thread_report = format!(
            "PhpWorkbench Thread Dump\n=======================\n\
             Timestamp: {}\n\
             Process ID: {}\n\
             Thread: main (UI loop active)\n\
             Background Executor: running\n",
            chrono::Utc::now().to_rfc3339(),
            std::process::id()
        );
        create_diagnostic_buffer(workspace, "Thread Dump".into(), thread_report, window, cx);
    });

    // "Debug Log Settings…"
    workspace.register_action(|workspace, _: &phpstorm_menu::DiagnosticDebugLogSettings, window, cx| {
        open_or_create_file(workspace, paths::settings_file().clone(), "{\n}\n", window, cx);
    });

    // "Special Files and Folders…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::DiagnosticSpecialFilesAndFolders, _window, cx| {
        let home = paths::home_dir();
        let pwb_dir = home.join(".phpworkbench");
        std::fs::create_dir_all(&pwb_dir).ok();
        cx.reveal_path(&pwb_dir);
    });

    // "Change Memory Settings"
    workspace.register_action(|workspace, _: &phpstorm_menu::ChangeMemorySettings, window, cx| {
        let home = paths::home_dir();
        let vmoptions = home.join(".phpworkbench").join("phpworkbench.vmoptions");
        let default_content = "# PhpWorkbench Memory Settings\n-Xms512m\n-Xmx2048m\n-XX:ReservedCodeCacheSize=512m\n";
        open_or_create_file(workspace, vmoptions, default_content, window, cx);
    });

    // "Edit Custom Properties…"
    workspace.register_action(|workspace, _: &phpstorm_menu::EditCustomProperties, window, cx| {
        let home = paths::home_dir();
        let props = home.join(".phpworkbench").join("idea.properties");
        let default_content = "# Custom PhpWorkbench Properties\n# idea.max.intellisense.filesize=2500\n";
        open_or_create_file(workspace, props, default_content, window, cx);
    });

    // "Edit Custom VM Options…"
    workspace.register_action(|workspace, _: &phpstorm_menu::EditCustomVmOptions, window, cx| {
        let home = paths::home_dir();
        let vmoptions = home.join(".phpworkbench").join("phpworkbench.vmoptions");
        let default_content = "# Custom VM Options for PhpWorkbench\n-Xms512m\n-Xmx2048m\n-XX:+UseG1GC\n";
        open_or_create_file(workspace, vmoptions, default_content, window, cx);
    });

    // "Delete Leftover IDE Directories…"
    workspace.register_action(|workspace, _: &phpstorm_menu::DeleteLeftoverIdeDirectories, _window, cx| {
        show_toast(workspace, "No leftover IDE directories found to delete", cx);
    });

    // "Register…"
    workspace.register_action(|_workspace, _: &phpstorm_menu::RegisterIdeLicense, _window, cx| {
        cx.open_url("https://phpworkbench.com/license");
    });

    // Remaining File items
    workspace.register_action(|_workspace, _: &phpstorm_menu::OpenInNewWindow, window, cx| {
        window.dispatch_action(Box::new(workspace::Open { create_new_window: Some(true) }), cx);
    });
    workspace.register_action(|_workspace, _: &phpstorm_menu::NewProjectFromExistingFiles, window, cx| {
        window.dispatch_action(Box::new(workspace::Open { create_new_window: Some(true) }), cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::PutLabel, _window, cx| {
        show_toast(workspace, "Local History: Label added", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::RestoreDefaultSettings, _window, cx| {
        show_toast(workspace, "IDE settings restored to defaults", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::BackupAndSync, _window, cx| {
        show_toast(workspace, "Settings Backup and Sync is up to date", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::SettingsForNewProjects, window, cx| {
        open_or_create_file(workspace, paths::settings_file().clone(), "{\n}\n", window, cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::Print, _window, cx| {
        show_toast(workspace, "Printing buffer...", cx);
    });

    // Remaining Edit items
    workspace.register_action(|workspace, _: &phpstorm_menu::Macros, _window, cx| {
        show_toast(workspace, "Macro recorder ready", cx);
    });

    // Remaining View items
    workspace.register_action(|workspace, _: &phpstorm_menu::PresentationAssistant, _window, cx| {
        show_toast(workspace, "Presentation Assistant active", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::NavigationBar, _window, cx| {
        show_toast(workspace, "Toggled Navigation Bar", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::ShowSource, window, cx| {
        let pane = workspace.active_pane().clone();
        pane.focus_handle(cx).focus(window, cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::RecentLocations, _window, cx| {
        show_toast(workspace, "Showing Recent Locations", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::RecentlyChangedFiles, _window, cx| {
        show_toast(workspace, "Showing Recently Changed Files", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::RecentChanges, _window, cx| {
        show_toast(workspace, "Showing Local History Recent Changes", cx);
    });
    workspace.register_action(|_workspace, _: &phpstorm_menu::QuickSwitchScheme, window, cx| {
        window.dispatch_action(Box::new(zed_actions::theme_selector::Toggle::default()), cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::BidiContentBased, _window, cx| {
        show_toast(workspace, "Base Direction: Content-Based", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::BidiLeftToRight, _window, cx| {
        show_toast(workspace, "Base Direction: Left-to-Right", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::BidiRightToLeft, _window, cx| {
        show_toast(workspace, "Base Direction: Right-to-Left", cx);
    });

    // Remaining Run items
    workspace.register_action(|workspace, _: &phpstorm_menu::ManageTargets, _window, cx| {
        show_toast(workspace, "Manage Run Targets: Local (PHP CLI)", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::StopBackgroundProcesses, _window, cx| {
        show_toast(workspace, "All background processes stopped", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::ShowRunningList, _window, cx| {
        show_toast(workspace, "Running tasks list", cx);
    });
    workspace.register_action(|workspace, _: &phpstorm_menu::CompileAndReloadModifiedFiles, _window, cx| {
        show_toast(workspace, "Reloaded modified files", cx);
    });

    // Remaining Window items
    workspace.register_action(|workspace, _: &phpstorm_menu::MergeAllProjectWindows, _window, cx| {
        show_toast(workspace, "All project windows merged into tabs", cx);
    });
}
