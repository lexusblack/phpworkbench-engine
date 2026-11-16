use editor::actions as editor_actions;
use encoding_selector;
use git;
use git_ui::{git_panel as git_ui_panel, git_picker, push_dialog};
use gpui::{App, Menu, MenuItem, OsAction};
use line_ending_selector;
use php_panel;
use project_panel;
use release_channel::ReleaseChannel;
use terminal_view::terminal_panel;
use zed_actions::{debug_panel, dev, git_panel, project_panel as zed_project_panel, Quit};

pub mod phpstorm_menu {
    use gpui::actions;

    actions!(
        phpstorm_menu,
        [
            PowerSaveMode,
            Print,
            RecentLocations,
            LastEditLocation,
            OverrideMethods,
            ImplementMethods,
            GenerateCode,
            SurroundWith,
            ChangeSignature,
            ExtractMethod,
            ExtractVariable,
            MoveRefactoring,
            CopyFile,
            StartListeningForPhpDebugConnections,
            Merge,
        ]
    );
}

pub fn app_menus(cx: &mut App) -> Vec<Menu> {
    let mut dev_items = Vec::new();
    if ReleaseChannel::try_global(cx) == Some(ReleaseChannel::Dev) {
        dev_items.push(MenuItem::separator());
        dev_items.push(MenuItem::action(
            "Toggle GPUI Inspector",
            dev::ToggleInspector,
        ));
    }

    vec![
        // =========================================================================
        // 1. PhpWorkbench (macOS Application Menu)
        // =========================================================================
        Menu {
            name: "PhpWorkbench".into(),
            disabled: false,
            items: vec![
                MenuItem::action("About PhpWorkbench", zed_actions::About),
                MenuItem::action("Check for Updates…", zed_actions::About),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Settings…").items([
                    MenuItem::action("Open Settings", zed_actions::OpenSettings),
                    MenuItem::action("Open Settings File", super::OpenSettingsFile),
                    MenuItem::action("Open Project Settings", zed_actions::OpenProjectSettings),
                    MenuItem::action("Open Project Settings File", super::OpenProjectSettingsFile),
                    MenuItem::action("Open Default Settings", super::OpenDefaultSettings),
                    MenuItem::separator(),
                    MenuItem::action("Open Keymap", zed_actions::OpenKeymap),
                    MenuItem::action("Open Keymap File", zed_actions::OpenKeymapFile),
                    MenuItem::action("Open Default Key Bindings", zed_actions::OpenDefaultKeymap),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Select Theme...",
                        zed_actions::theme_selector::Toggle::default(),
                    ),
                    MenuItem::action(
                        "Select Icon Theme...",
                        zed_actions::icon_theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Extensions", zed_actions::Extensions::default()),
                #[cfg(not(target_os = "windows"))]
                MenuItem::action("Install CLI", install_cli::InstallCliBinary),
                MenuItem::separator(),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide PhpWorkbench", super::Hide),
                #[cfg(target_os = "macos")]
                MenuItem::action("Hide Others", super::HideOthers),
                #[cfg(target_os = "macos")]
                MenuItem::action("Show All", super::ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit PhpWorkbench", Quit),
            ],
        },

        // =========================================================================
        // 2. File
        // =========================================================================
        Menu {
            name: "File".into(),
            disabled: false,
            items: vec![
                MenuItem::action("New Project…", workspace::NewWindow),
                MenuItem::action("Get from Version Control…", git::Clone),
                MenuItem::action("New…", project_panel::NewFile),
                MenuItem::action("New Scratch File", workspace::NewFile),
                MenuItem::action(
                    if cfg!(not(target_os = "macos")) {
                        "Open Folder..."
                    } else {
                        "Open…"
                    },
                    workspace::Open::default(),
                ),
                MenuItem::action("Save As…", workspace::SaveAs),
                MenuItem::submenu(Menu::new("Recent Projects").items([
                    MenuItem::action("Manage Projects…", zed_actions::OpenRecent::default()),
                ])),
                MenuItem::action("Close Project", workspace::CloseWindow),
                MenuItem::action("Close All Projects", workspace::CloseAllItemsAndPanes::default()),
                MenuItem::action("Rename Project…", project_panel::Rename),
                MenuItem::separator(),
                MenuItem::action("Connect to Remote…", php_panel::OpenDeploymentDialog),
                MenuItem::separator(),
                MenuItem::action("Plugins…", zed_actions::Extensions::default()),
                MenuItem::submenu(Menu::new("File Properties").items([
                    MenuItem::action("Line Separators…", line_ending_selector::Toggle),
                    MenuItem::action("File Encoding…", encoding_selector::Toggle),
                ])),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Local History").items([
                    MenuItem::action("Show History", git::FileHistory),
                ])),
                MenuItem::separator(),
                MenuItem::action("Save All", workspace::SaveAll { save_intent: None }),
                MenuItem::action("Reload All from Disk", workspace::ReloadActiveItem),
                MenuItem::action("Repair IDE", workspace::ReloadActiveItem),
                MenuItem::action("Invalidate Caches…", workspace::Reload),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Manage IDE Settings").items([
                    MenuItem::action("Settings…", zed_actions::OpenSettings),
                    MenuItem::action("Keymap…", zed_actions::OpenKeymap),
                    MenuItem::action(
                        "Theme…",
                        zed_actions::theme_selector::Toggle::default(),
                    ),
                ])),
                MenuItem::action("Export", zed_actions::OpenSettingsFile),
                MenuItem::action("Print…", phpstorm_menu::Print),
                MenuItem::separator(),
                MenuItem::action("Power Save Mode", phpstorm_menu::PowerSaveMode),
            ],
        },

        // =========================================================================
        // 3. Edit
        // =========================================================================
        Menu {
            name: "Edit".into(),
            disabled: false,
            items: vec![
                MenuItem::os_action("Undo", editor_actions::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", editor_actions::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", editor_actions::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", editor_actions::Copy, OsAction::Copy),
                MenuItem::action("Copy Path/Reference…", zed_actions::workspace::CopyPath),
                MenuItem::os_action("Paste", editor_actions::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Find").items([
                    MenuItem::action("Find in File", search::buffer_search::Deploy::find()),
                    MenuItem::action("Replace in File", search::buffer_search::Deploy::replace()),
                    MenuItem::action("Find in Files…", workspace::DeploySearch::default()),
                    MenuItem::action(
                        "Replace in Files…",
                        workspace::DeploySearch {
                            replace_enabled: true,
                            ..Default::default()
                        },
                    ),
                ])),
                MenuItem::action("Find Usages", editor_actions::FindAllReferences::default()),
                MenuItem::separator(),
                MenuItem::os_action(
                    "Select All",
                    editor_actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::action(
                    "Add Carets to Ends of Selected Lines",
                    editor_actions::SplitSelectionIntoLines::default(),
                ),
                MenuItem::action("Extend Selection", editor_actions::SelectLargerSyntaxNode),
                MenuItem::action("Shrink Selection", editor_actions::SelectSmallerSyntaxNode),
                MenuItem::separator(),
                MenuItem::action("Toggle Case", editor_actions::ToggleCase),
                MenuItem::action("Join Lines", editor_actions::JoinLines),
                MenuItem::action("Duplicate Line or Selection", editor_actions::DuplicateLineDown),
                MenuItem::action("Sort Lines", editor_actions::SortLinesCaseInsensitive),
                MenuItem::action("Reverse Lines", editor_actions::ReverseLines),
                MenuItem::separator(),
                MenuItem::action("Indent Selection", editor_actions::Indent),
                MenuItem::action("Unindent Line or Selection", editor_actions::Outdent),
                MenuItem::submenu(Menu::new("Convert Indents").items([
                    MenuItem::action("To Spaces", editor_actions::ConvertIndentationToSpaces),
                    MenuItem::action("To Tabs", editor_actions::ConvertIndentationToTabs),
                ])),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Bookmarks").items([
                    MenuItem::action("Toggle Bookmark", editor_actions::ToggleBookmark),
                    MenuItem::action(
                        "Toggle Bookmark with Label",
                        editor_actions::ToggleBookmarkWithLabel,
                    ),
                    MenuItem::action("Show Bookmarks", editor_actions::ViewBookmarks),
                    MenuItem::action("Next Bookmark", editor_actions::GoToNextBookmark),
                    MenuItem::action("Previous Bookmark", editor_actions::GoToPreviousBookmark),
                ])),
                MenuItem::separator(),
                MenuItem::action("Emoji & Symbols", editor_actions::ShowCharacterPalette),
            ],
        },

        // =========================================================================
        // 4. View
        // =========================================================================
        Menu {
            name: "View".into(),
            disabled: false,
            items: {
                let mut items = vec![
                    MenuItem::submenu(Menu::new("Tool Windows").items([
                        MenuItem::action("Project (⌘1)", zed_project_panel::ToggleFocus),
                        MenuItem::action("Commit (⌘0)", git_panel::ToggleFocus),
                        MenuItem::action("Terminal (⌥F12)", terminal_panel::Toggle),
                        MenuItem::action("Database", php_panel::ToggleFocus),
                        MenuItem::action("Remote Host", php_panel::BrowseRemoteHost),
                        MenuItem::action("HTTP Client", php_panel::OpenHttpClient),
                        MenuItem::action("Structure (⌘7)", outline_panel::ToggleFocus),
                        MenuItem::action("Problems (⌘6)", diagnostics::Deploy),
                        MenuItem::action("Debug (⌘5)", debug_panel::ToggleFocus),
                    ])),
                    MenuItem::submenu(Menu::new("Appearance").items([
                        MenuItem::action(
                            "Zoom In",
                            zed_actions::IncreaseBufferFontSize { persist: false },
                        ),
                        MenuItem::action(
                            "Zoom Out",
                            zed_actions::DecreaseBufferFontSize { persist: false },
                        ),
                        MenuItem::action(
                            "Reset Zoom",
                            zed_actions::ResetBufferFontSize { persist: false },
                        ),
                        MenuItem::action("Toggle Full Screen", super::ToggleFullScreen),
                    ])),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Quick Definition",
                        editor_actions::GoToDefinition::default(),
                    ),
                    MenuItem::action(
                        "Quick Type Definition",
                        editor_actions::GoToTypeDefinition::default(),
                    ),
                    MenuItem::action(
                        "Jump to Source",
                        editor_actions::GoToDeclaration::default(),
                    ),
                    MenuItem::action("Open in Right Split", workspace::SplitRight::default()),
                    MenuItem::separator(),
                    MenuItem::action("Recent Files", workspace::ToggleFileFinder::default()),
                    MenuItem::action("Recent Locations", phpstorm_menu::RecentLocations),
                    MenuItem::action(
                        "Compare With…",
                        git_ui::project_diff::CompareWithBranch,
                    ),
                    MenuItem::action(
                        "Compare with Clipboard",
                        editor_actions::DiffClipboardWithSelection,
                    ),
                ];
                items.extend(dev_items);
                items
            },
        },

        // =========================================================================
        // 5. Navigate
        // =========================================================================
        Menu {
            name: "Navigate".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Back", workspace::GoBack),
                MenuItem::action("Forward", workspace::GoForward),
                MenuItem::separator(),
                MenuItem::action("Search Everywhere", zed_actions::command_palette::Toggle),
                MenuItem::action("Class…", workspace::ToggleProjectSymbols),
                MenuItem::action("File…", workspace::ToggleFileFinder::default()),
                MenuItem::action("Symbol…", workspace::ToggleProjectSymbols),
                MenuItem::separator(),
                MenuItem::action("Next Highlighted Error", editor_actions::GoToDiagnostic::default()),
                MenuItem::action(
                    "Previous Highlighted Error",
                    editor_actions::GoToPreviousDiagnostic::default(),
                ),
                MenuItem::action("Last Edit Location", phpstorm_menu::LastEditLocation),
                MenuItem::separator(),
                MenuItem::action(
                    "Declaration or Usages",
                    editor_actions::GoToDefinition::default(),
                ),
                MenuItem::action(
                    "Implementation(s)",
                    editor_actions::GoToImplementation::default(),
                ),
                MenuItem::action(
                    "Type Declaration",
                    editor_actions::GoToTypeDefinition::default(),
                ),
                MenuItem::separator(),
                MenuItem::action("File Structure", zed_actions::outline::ToggleOutline),
            ],
        },

        // =========================================================================
        // 6. Code
        // =========================================================================
        Menu {
            name: "Code".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Override Methods…", phpstorm_menu::OverrideMethods),
                MenuItem::action("Implement Methods…", phpstorm_menu::ImplementMethods),
                MenuItem::action("Generate…", phpstorm_menu::GenerateCode),
                MenuItem::separator(),
                MenuItem::action("Code Completion", editor_actions::ShowCompletions),
                MenuItem::action("Inspect Code…", diagnostics::Deploy),
                MenuItem::separator(),
                MenuItem::action("Surround With…", phpstorm_menu::SurroundWith),
                MenuItem::submenu(Menu::new("Folding").items([
                    MenuItem::action("Expand", editor_actions::UnfoldLines),
                    MenuItem::action("Collapse", editor_actions::Fold),
                    MenuItem::action("Expand All", editor_actions::UnfoldAll),
                    MenuItem::action("Collapse All", editor_actions::FoldAll),
                ])),
                MenuItem::separator(),
                MenuItem::action(
                    "Comment with Line Comment",
                    editor_actions::ToggleComments::default(),
                ),
                MenuItem::action(
                    "Comment with Block Comment",
                    editor_actions::ToggleBlockComments,
                ),
                MenuItem::action("Reformat Code", editor_actions::Format),
                MenuItem::action("Optimize Imports", editor_actions::OrganizeImports),
                MenuItem::action("Move Statement Up", editor_actions::MoveLineUp),
                MenuItem::action("Move Statement Down", editor_actions::MoveLineDown),
            ],
        },

        // =========================================================================
        // 7. Refactor
        // =========================================================================
        Menu {
            name: "Refactor".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Refactor This…", editor_actions::ToggleCodeActions::default()),
                MenuItem::action("Rename…", editor_actions::Rename),
                MenuItem::action("Change Signature…", phpstorm_menu::ChangeSignature),
                MenuItem::action("Extract Method…", phpstorm_menu::ExtractMethod),
                MenuItem::action("Extract Variable…", phpstorm_menu::ExtractVariable),
                MenuItem::action("Move…", phpstorm_menu::MoveRefactoring),
                MenuItem::action("Copy File…", phpstorm_menu::CopyFile),
            ],
        },

        // =========================================================================
        // 8. Run
        // =========================================================================
        Menu {
            name: "Run".into(),
            disabled: false,
            items: vec![
                MenuItem::action(
                    "Run…",
                    zed_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::action("Debug…", debugger_ui::Start),
                MenuItem::action("Edit Configurations…", zed_actions::OpenProjectDebugTasks),
                MenuItem::action("Stop", debugger_ui::Stop),
                MenuItem::action("Toggle Breakpoint", editor_actions::ToggleBreakpoint),
                MenuItem::action("View Breakpoints…", editor_actions::EditLogBreakpoint),
                MenuItem::action(
                    "Start Listening for PHP Debug Connections",
                    phpstorm_menu::StartListeningForPhpDebugConnections,
                ),
            ],
        },

        // =========================================================================
        // 9. Tools
        // =========================================================================
        Menu {
            name: "Tools".into(),
            disabled: false,
            items: {
                let (deploy_servers, _default_server, is_auto_upload) = if cx.has_global::<php_panel::DeployShared>() {
                    let shared = cx.global::<php_panel::DeployShared>();
                    (shared.servers.clone(), shared.active.clone(), shared.auto_upload)
                } else {
                    (Vec::new(), None, false)
                };

                let mut items = Vec::new();

                items.push(MenuItem::submenu(Menu::new("Deployment").items({
                    let mut dep_items = vec![
                        MenuItem::action("Upload to Default Server", php_panel::UploadToDeployServer::default()),
                    ];

                    if !deploy_servers.is_empty() {
                        let mut upload_to_items = Vec::new();
                        for s in &deploy_servers {
                            upload_to_items.push(MenuItem::action(
                                s.clone(),
                                php_panel::UploadToDeployServer {
                                    server: Some(s.clone()),
                                    ..Default::default()
                                },
                            ));
                        }
                        dep_items.push(MenuItem::submenu(Menu::new("Upload to…").items(upload_to_items)));
                    } else {
                        dep_items.push(MenuItem::action("Upload to…", php_panel::OpenDeploymentDialog));
                    }

                    dep_items.push(MenuItem::action(
                        "Upload All Open Files to Default Server",
                        php_panel::UploadAllOpenFilesToDeployServer::default(),
                    ));

                    if !deploy_servers.is_empty() {
                        let mut upload_all_items = Vec::new();
                        for s in &deploy_servers {
                            upload_all_items.push(MenuItem::action(
                                s.clone(),
                                php_panel::UploadAllOpenFilesToDeployServer {
                                    server: Some(s.clone()),
                                },
                            ));
                        }
                        dep_items.push(MenuItem::submenu(Menu::new("Upload All Open Files to…").items(upload_all_items)));
                    }

                    dep_items.push(MenuItem::action("Download from Default Server", php_panel::DownloadFromDeployServer::default()));

                    if !deploy_servers.is_empty() {
                        let mut dl_items = Vec::new();
                        for s in &deploy_servers {
                            dl_items.push(MenuItem::action(
                                s.clone(),
                                php_panel::DownloadFromDeployServer {
                                    server: Some(s.clone()),
                                    ..Default::default()
                                },
                            ));
                        }
                        dep_items.push(MenuItem::submenu(Menu::new("Download from…").items(dl_items)));
                    } else {
                        dep_items.push(MenuItem::action("Download from…", php_panel::OpenDeploymentDialog));
                    }

                    dep_items.push(MenuItem::separator());

                    dep_items.push(MenuItem::action(
                        "Compare Local File with Deployed Version",
                        php_panel::CompareWithDeployServer::default(),
                    ));

                    if !deploy_servers.is_empty() {
                        let mut cmp_items = Vec::new();
                        for s in &deploy_servers {
                            cmp_items.push(MenuItem::action(
                                s.clone(),
                                php_panel::CompareWithDeployServer {
                                    server: Some(s.clone()),
                                    ..Default::default()
                                },
                            ));
                        }
                        dep_items.push(MenuItem::submenu(Menu::new("Compare with Deployed to…").items(cmp_items)));
                    }

                    dep_items.push(MenuItem::action(
                        "Sync Local Subtree with Deployed",
                        php_panel::SyncWithDeployServer::default(),
                    ));

                    if !deploy_servers.is_empty() {
                        let mut sync_items = Vec::new();
                        for s in &deploy_servers {
                            sync_items.push(MenuItem::action(
                                s.clone(),
                                php_panel::SyncWithDeployServer {
                                    server: Some(s.clone()),
                                    ..Default::default()
                                },
                            ));
                        }
                        dep_items.push(MenuItem::submenu(Menu::new("Sync with Deployed to…").items(sync_items)));
                    }

                    dep_items.push(MenuItem::separator());
                    dep_items.push(MenuItem::action("Configuration…", php_panel::OpenDeploymentDialog));
                    dep_items.push(MenuItem::action("Options…", php_panel::OpenDeploymentDialog));
                    dep_items.push(MenuItem::action("Automatic Upload", php_panel::ToggleAutomaticUpload).checked(is_auto_upload));
                    dep_items.push(MenuItem::separator());
                    dep_items.push(MenuItem::action("Browse Remote Host", php_panel::BrowseRemoteHost));

                    dep_items
                })));

                if !deploy_servers.is_empty() {
                    let mut ssh_items = Vec::new();
                    for s in &deploy_servers {
                        ssh_items.push(MenuItem::action(
                            s.clone(),
                            php_panel::StartSshSession {
                                server: Some(s.clone()),
                            },
                        ));
                    }
                    ssh_items.push(MenuItem::separator());
                    ssh_items.push(MenuItem::action("SSH Configurations…", php_panel::OpenSshConfigsDialog));
                    items.push(MenuItem::submenu(Menu::new("Start SSH Session…").items(ssh_items)));
                } else {
                    items.push(MenuItem::action("Start SSH Session…", php_panel::StartSshSession::default()));
                }

                items.push(MenuItem::submenu(Menu::new("HTTP Client").items([
                    MenuItem::action("Open HTTP Client", php_panel::OpenHttpClient),
                    MenuItem::action("Create Request in HTTP Client", php_panel::CreateHttpRequest),
                    MenuItem::action("Show HTTP Requests History", php_panel::OpenHttpClient),
                    MenuItem::action("Convert cURL to HTTP Request", php_panel::CreateHttpRequest),
                ])));
                items.push(MenuItem::submenu(Menu::new("Composer").items([
                    MenuItem::action("Init Composer…", php_panel::RunComposer),
                    MenuItem::action("Install Dependencies", php_panel::RunComposer),
                    MenuItem::action("Update Dependencies", php_panel::RunComposer),
                    MenuItem::action("Diagnose", php_panel::RunComposer),
                ])));
                items.push(MenuItem::submenu(Menu::new("Database").items([
                    MenuItem::action(
                        "Data Sources and Drivers…",
                        php_panel::OpenDataSourcesDialog,
                    ),
                    MenuItem::action("Jump to Console", php_panel::OpenDatabaseConsole),
                ])));
                items.push(MenuItem::action(
                    "SSH Configurations…",
                    php_panel::OpenSshConfigsDialog,
                ));
                items.push(MenuItem::submenu(Menu::new("XML Actions").items([
                    MenuItem::action("Convert Schema File…", php_panel::ConvertSchemaFileDialog),
                    MenuItem::action("Generate DTD from XML File…", php_panel::ConvertSchemaFileDialog),
                    MenuItem::action("Validate XML…", php_panel::ConvertSchemaFileDialog),
                ])));
                items.push(MenuItem::separator());
                items.push(MenuItem::action("Tasks & Contexts", zed_actions::OpenProjectTasks));
                #[cfg(not(target_os = "windows"))]
                items.push(MenuItem::action("Create Command Line Launcher…", install_cli::InstallCliBinary));

                items
            },
        },

        // =========================================================================
        // 10. Git
        // =========================================================================
        Menu {
            name: "Git".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Commit…", git_panel::ToggleFocus),
                MenuItem::action("Push…", push_dialog::OpenPushDialog),
                MenuItem::action("Update Project…", git_ui_panel::OpenMenu),
                MenuItem::action("Pull…", git::Pull),
                MenuItem::action("Fetch", git::Fetch),
                MenuItem::separator(),
                MenuItem::action("Merge…", phpstorm_menu::Merge),
                MenuItem::action("Rebase…", git::PullRebase),
                MenuItem::separator(),
                MenuItem::action("Branches…", git_picker::ActivateBranchesTab),
                MenuItem::action("New Branch…", zed_actions::git::Branch),
                MenuItem::action("New Tag…", git::CreateTagAtHead),
                MenuItem::action("Reset HEAD…", git::RestoreTrackedFiles),
                MenuItem::separator(),
                MenuItem::action("Stash Changes…", git::StashAll),
                MenuItem::action("Unstash Changes…", git::StashPop),
                MenuItem::action("Show Git Log", git_panel::ToggleFocus),
            ],
        },

        // =========================================================================
        // 11. Window
        // =========================================================================
        Menu {
            name: "Window".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Minimize", super::Minimize),
                MenuItem::action("Zoom", super::Zoom),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Layouts").items([
                    MenuItem::action("Restore Default Layout", workspace::ToggleAllDocks),
                    MenuItem::action("Save Current Layout as Default", workspace::ToggleAllDocks),
                ])),
                MenuItem::submenu(Menu::new("Active Tool Window").items([
                    MenuItem::action("Hide Active Window", workspace::CloseActiveDock),
                    MenuItem::action("Hide All Tool Windows", workspace::ToggleAllDocks),
                    MenuItem::action("Jump to Last Tool Window", workspace::JumpToLastToolWindow),
                ])),
                MenuItem::submenu(Menu::new("Editor Tabs").items([
                    MenuItem::action(
                        "Close Active Tab",
                        workspace::CloseActiveItem {
                            save_intent: None,
                            close_pinned: true,
                        },
                    ),
                    MenuItem::action("Close All Tabs", workspace::CloseAllItemsAndPanes::default()),
                    MenuItem::action("Close Other Tabs", workspace::CloseInactiveTabsAndPanes::default()),
                ])),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Project Window",
                    workspace::ActivateNextItem { wrap_around: true },
                ),
                MenuItem::action(
                    "Previous Project Window",
                    workspace::ActivatePreviousItem { wrap_around: true },
                ),
            ],
        },

        // =========================================================================
        // 12. Help
        // =========================================================================
        Menu {
            name: "Help".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Find Action…", zed_actions::command_palette::Toggle),
                MenuItem::action("Help Topics", zed_actions::OpenOnboarding),
                MenuItem::action("What's New", zed_actions::About),
                MenuItem::action("Keyboard Shortcuts PDF", zed_actions::OpenDefaultKeymap),
                MenuItem::submenu(Menu::new("Diagnostic Tools").items([
                    MenuItem::action("Troubleshooting / Diagnostics", diagnostics::Deploy),
                    MenuItem::action("Open Log File", workspace::OpenLog),
                ])),
                MenuItem::action("Show Log in Finder", workspace::RevealLogInFileManager),
            ],
        },
    ]
}
