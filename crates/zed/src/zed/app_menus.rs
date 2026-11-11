use collab_ui::collab_panel;
use gpui::{App, Menu, MenuItem, OsAction};
use release_channel::ReleaseChannel;
use php_panel;
use terminal_view::terminal_panel;
use zed_actions::{Quit, debug_panel, dev, git_panel, project_panel};
use editor::actions as editor_actions;
use git_ui::{git_panel as git_ui_panel, git_picker, push_dialog};

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
        // 1. PhpStorm / PhpWorkbench (macOS Application Menu)
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
                MenuItem::action("New…", workspace::NewFile),
                MenuItem::action("New Scratch File", workspace::NewFile),
                MenuItem::action("New Window", workspace::NewWindow),
                MenuItem::separator(),
                #[cfg(not(target_os = "macos"))]
                MenuItem::action("Open File...", workspace::OpenFiles),
                MenuItem::action(
                    if cfg!(not(target_os = "macos")) {
                        "Open Folder..."
                    } else {
                        "Open…"
                    },
                    workspace::Open::default(),
                ),
                MenuItem::action("Open Recent…", zed_actions::OpenRecent::default()),
                MenuItem::action("Connect to Remote…", zed_actions::OpenRemote::default()),
                MenuItem::separator(),
                MenuItem::action("Add Folder to Project…", workspace::AddFolderToProject),
                MenuItem::separator(),
                MenuItem::action("Save", workspace::Save { save_intent: None }),
                MenuItem::action("Save As…", workspace::SaveAs),
                MenuItem::action("Save All", workspace::SaveAll { save_intent: None }),
                MenuItem::action("Reload All from Disk", workspace::ReloadActiveItem),
                MenuItem::separator(),
                MenuItem::action("Repair IDE", workspace::ReloadActiveItem),
                MenuItem::action("Invalidate Caches…", workspace::Reload),
                MenuItem::submenu(Menu::new("Manage IDE Settings").items([
                    MenuItem::action("Open Settings", zed_actions::OpenSettings),
                    MenuItem::action("Export Settings…", zed_actions::OpenSettingsFile),
                    MenuItem::action("Restore Default Settings…", super::OpenDefaultSettings),
                ])),
                MenuItem::submenu(Menu::new("File Properties").items([
                    MenuItem::action("Make File Read-Only", workspace::ToggleReadOnlyFile),
                ])),
                MenuItem::separator(),
                MenuItem::action(
                    "Close Editor",
                    workspace::CloseActiveItem {
                        save_intent: None,
                        close_pinned: true,
                    },
                ),
                MenuItem::action("Close Project", workspace::CloseProject),
                MenuItem::action("Close Window", workspace::CloseWindow),
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
                MenuItem::action("Copy as Plain Text", editor_actions::CopyAndTrim),
                MenuItem::os_action("Paste", editor_actions::Paste, OsAction::Paste),
                MenuItem::action("Delete", editor_actions::Delete),
                MenuItem::separator(),
                MenuItem::action("Find", search::buffer_search::Deploy::find()),
                MenuItem::action("Find in Files…", workspace::DeploySearch::default()),
                MenuItem::action("Replace in Files…", workspace::DeploySearch::default()),
                MenuItem::action("Find Usages", editor_actions::FindAllReferences::default()),
                MenuItem::separator(),
                MenuItem::os_action(
                    "Select All",
                    editor_actions::SelectAll,
                    OsAction::SelectAll,
                ),
                MenuItem::action("Extend Selection", editor_actions::SelectLargerSyntaxNode),
                MenuItem::action("Shrink Selection", editor_actions::SelectSmallerSyntaxNode),
                MenuItem::action(
                    "Add Carets to Ends of Selected Lines",
                    editor_actions::SplitSelectionIntoLines::default(),
                ),
                MenuItem::action(
                    "Add Cursor Above",
                    editor_actions::AddSelectionAbove {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    "Add Cursor Below",
                    editor_actions::AddSelectionBelow {
                        skip_soft_wrap: true,
                    },
                ),
                MenuItem::action(
                    "Select Next Occurrence",
                    editor_actions::SelectNext {
                        replace_newest: false,
                    },
                ),
                MenuItem::action(
                    "Select Previous Occurrence",
                    editor_actions::SelectPrevious {
                        replace_newest: false,
                    },
                ),
                MenuItem::action("Select All Occurrences", editor_actions::SelectAllMatches),
                MenuItem::separator(),
                MenuItem::action("Move Line Up", editor_actions::MoveLineUp),
                MenuItem::action("Move Line Down", editor_actions::MoveLineDown),
                MenuItem::action("Duplicate Line or Selection", editor_actions::DuplicateLineDown),
                MenuItem::action("Indent Selection", editor_actions::Indent),
                MenuItem::action("Unindent Line or Selection", editor_actions::Outdent),
                MenuItem::submenu(Menu::new("Convert Indents").items([
                    MenuItem::action("To Spaces", editor_actions::ConvertIndentationToSpaces),
                    MenuItem::action("To Tabs", editor_actions::ConvertIndentationToTabs),
                ])),
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
                        MenuItem::action("Project (⌘1)", project_panel::ToggleFocus),
                        MenuItem::action("Commit (⌘0)", git_panel::ToggleFocus),
                        MenuItem::action("Database", php_panel::ToggleFocus),
                        MenuItem::action("Services (Remote Host)", php_panel::BrowseRemoteHost),
                        MenuItem::action("HTTP Client", php_panel::OpenHttpClient),
                        MenuItem::action("Terminal (⌥F12)", terminal_panel::Toggle),
                        MenuItem::action("Structure (⌘7)", outline_panel::ToggleFocus),
                        MenuItem::action("Problems / Diagnostics (⌘6)", diagnostics::Deploy),
                        MenuItem::action(
                            "Run / Tasks (⌃⌥R)",
                            zed_actions::Spawn::ViaModal {
                                reveal_target: None,
                            },
                        ),
                        MenuItem::action("Debug (⌘5)", debug_panel::ToggleFocus),
                        MenuItem::action("Git", git_panel::ToggleFocus),
                        MenuItem::action("Collab / Channels", collab_panel::ToggleFocus),
                        MenuItem::action("Notifications", workspace::ToggleBottomDock),
                        MenuItem::action("File Transfer", php_panel::BrowseRemoteHost),
                    ])),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu::new("Appearance").items([
                        MenuItem::action("Toggle Left Dock", workspace::ToggleLeftDock),
                        MenuItem::action("Toggle Right Dock", workspace::ToggleRightDock),
                        MenuItem::action("Toggle Bottom Dock", workspace::ToggleBottomDock),
                        MenuItem::action("Toggle All Docks", workspace::ToggleAllDocks),
                        MenuItem::action("Toggle Centered Layout", workspace::ToggleCenteredLayout),
                        MenuItem::action("Toggle Editor Zoom", workspace::ToggleEditorZoom),
                    ])),
                    MenuItem::submenu(Menu::new("Editor Layout").items([
                        MenuItem::action("Split Up", workspace::SplitUp::default()),
                        MenuItem::action("Split Down", workspace::SplitDown::default()),
                        MenuItem::action("Split Left", workspace::SplitLeft::default()),
                        MenuItem::action("Split Right", workspace::SplitRight::default()),
                    ])),
                    MenuItem::separator(),
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
                    MenuItem::action(
                        "Reset Font Size in All Editors",
                        zed_actions::ResetAllZoom { persist: false },
                    ),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Quick Definition",
                        editor_actions::GoToDefinition::default(),
                    ),
                    MenuItem::action(
                        "Quick Type Definition",
                        editor_actions::GoToTypeDefinition::default(),
                    ),
                    MenuItem::action("Recent Files", workspace::ToggleFileFinder::default()),
                    MenuItem::action("Recent Changes", git_panel::ToggleFocus),
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
                MenuItem::action("Search Everywhere", zed_actions::command_palette::Toggle),
                MenuItem::action("Class…", zed_actions::outline::ToggleOutline),
                MenuItem::action("File…", workspace::ToggleFileFinder::default()),
                MenuItem::action("Symbol…", zed_actions::outline::ToggleOutline),
                MenuItem::action("Line/Column…", editor_actions::ToggleGoToLine),
                MenuItem::separator(),
                MenuItem::action("Back", workspace::GoBack),
                MenuItem::action("Forward", workspace::GoForward),
                MenuItem::separator(),
                MenuItem::action("Next Highlighted Error", editor_actions::GoToDiagnostic::default()),
                MenuItem::action(
                    "Previous Highlighted Error",
                    editor_actions::GoToPreviousDiagnostic::default(),
                ),
                MenuItem::separator(),
                MenuItem::action(
                    "Declaration or Usages",
                    editor_actions::GoToDeclaration::default(),
                ),
                MenuItem::action(
                    "Implementation(s)",
                    editor_actions::GoToImplementation::default(),
                ),
                MenuItem::action(
                    "Type Declaration",
                    editor_actions::GoToTypeDefinition::default(),
                ),
                MenuItem::action(
                    "Find Usages",
                    editor_actions::FindAllReferences::default(),
                ),
                MenuItem::action("Call Hierarchy", call_hierarchy::ShowIncomingCalls),
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
                MenuItem::action("Code Completion", editor_actions::ConfirmCompletion::default()),
                MenuItem::action(
                    "Intention Actions / Code Actions",
                    editor_actions::ToggleCodeActions::default(),
                ),
                MenuItem::separator(),
                MenuItem::action("Reformat Code", editor_actions::Format),
                MenuItem::action("Auto-Indent Lines", editor_actions::AutoIndent),
                MenuItem::submenu(Menu::new("Folding").items([
                    MenuItem::action("Expand", editor_actions::UnfoldAll),
                    MenuItem::action("Collapse", editor_actions::FoldAll),
                    MenuItem::action("Toggle Fold", editor_actions::ToggleFold),
                    MenuItem::action("Fold Recursive", editor_actions::FoldRecursive),
                    MenuItem::action("Fold All", editor_actions::FoldAll),
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
                MenuItem::separator(),
                MenuItem::action("Move Statement Down", editor_actions::MoveLineDown),
                MenuItem::action("Move Statement Up", editor_actions::MoveLineUp),
                MenuItem::action("Duplicate Line or Selection", editor_actions::DuplicateLineDown),
                MenuItem::action("Inspect Code…", diagnostics::Deploy),
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
                MenuItem::action("Format Selection", editor_actions::FormatSelections),
                MenuItem::action("Copy…", editor_actions::Copy),
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
                MenuItem::action("Start Debugger", debugger_ui::Start),
                MenuItem::action("Edit Configurations…", zed_actions::OpenProjectDebugTasks),
                MenuItem::action("Edit Tasks…", zed_actions::OpenProjectTasks),
                MenuItem::separator(),
                MenuItem::action("Continue / Resume Program", debugger_ui::Continue),
                MenuItem::action("Step Over", debugger_ui::StepOver),
                MenuItem::action("Step Into", debugger_ui::StepInto),
                MenuItem::action("Step Out", debugger_ui::StepOut),
                MenuItem::action("Stop", workspace::ShutdownDebugAdapters),
                MenuItem::separator(),
                MenuItem::action("Toggle Line Breakpoint", editor_actions::ToggleBreakpoint),
                MenuItem::action("Edit Breakpoint…", editor_actions::EditLogBreakpoint),
                MenuItem::action("Clear All Breakpoints", debugger_ui::ClearAllBreakpoints),
            ],
        },

        // =========================================================================
        // 9. Tools
        // =========================================================================
        Menu {
            name: "Tools".into(),
            disabled: false,
            items: vec![
                MenuItem::submenu(Menu::new("Deployment").items([
                    MenuItem::action(
                        "Configuration…",
                        php_panel::OpenDeploymentDialog,
                    ),
                    MenuItem::action("Browse Remote Host", php_panel::BrowseRemoteHost),
                    MenuItem::action("Upload to Default Server", php_panel::OpenDeploymentDialog),
                    MenuItem::action("Download from Default Server", php_panel::OpenDeploymentDialog),
                    MenuItem::action("Options…", php_panel::OpenDeploymentDialog),
                ])),
                MenuItem::submenu(Menu::new("HTTP Client").items([
                    MenuItem::action("Open HTTP Client", php_panel::OpenHttpClient),
                    MenuItem::action("Create Request in HTTP Client", php_panel::CreateHttpRequest),
                    MenuItem::action("Show HTTP Requests History", php_panel::OpenHttpClient),
                    MenuItem::action("Convert cURL to HTTP Request", php_panel::CreateHttpRequest),
                ])),
                MenuItem::submenu(Menu::new("Composer").items([
                    MenuItem::action("Init Composer…", php_panel::RunComposer),
                    MenuItem::action("Install Dependencies", php_panel::RunComposer),
                    MenuItem::action("Update Dependencies", php_panel::RunComposer),
                    MenuItem::action("Diagnose", php_panel::RunComposer),
                ])),
                MenuItem::submenu(Menu::new("Database").items([
                    MenuItem::action(
                        "Data Sources and Drivers…",
                        php_panel::OpenDataSourcesDialog,
                    ),
                    MenuItem::action("Jump to Console", php_panel::OpenDatabaseConsole),
                ])),
                MenuItem::action(
                    "SSH Configurations…",
                    php_panel::OpenSshConfigsDialog,
                ),
                MenuItem::separator(),
                MenuItem::action("Tasks & Contexts", zed_actions::OpenProjectTasks),
                #[cfg(not(target_os = "windows"))]
                MenuItem::action("Create Command Line Launcher…", install_cli::InstallCliBinary),
            ],
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
                MenuItem::action("Fetch", git_ui_panel::OpenMenu),
                MenuItem::action("Branches…", git_picker::ActivateBranchesTab),
                MenuItem::action("Stash Changes…", git_picker::ActivateStashTab),
                MenuItem::action("File History", git::FileHistory),
                MenuItem::action("Git Blame", git::Blame),
                MenuItem::action("Show Git Log", git_panel::ToggleFocus),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Patch").items([
                    MenuItem::action("Create Patch from Local Changes…", git_ui_panel::FocusChanges),
                    MenuItem::action("Apply Patch…", git_ui_panel::FocusChanges),
                    MenuItem::action("Apply Patch from Clipboard…", git_ui_panel::FocusChanges),
                ])),
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
                MenuItem::action("About PhpWorkbench", zed_actions::About),
                MenuItem::action("Keyboard Shortcuts PDF", zed_actions::OpenDefaultKeymap),
                MenuItem::action("What's New in PhpWorkbench", zed_actions::About),
                MenuItem::action("Getting Started", zed_actions::OpenOnboarding),
                MenuItem::separator(),
                MenuItem::action("Submit Feedback…", zed_actions::feedback::RequestFeature),
                MenuItem::action("Submit a Bug Report…", zed_actions::feedback::FileBugReport),
                MenuItem::action("View Dependency Licenses", zed_actions::OpenLicenses),
            ],
        },
    ]
}
