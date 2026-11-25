use call_hierarchy;
use editor::actions as editor_actions;
use encoding_selector;
use git;
use git_ui::{git_panel as git_ui_panel, git_picker, manage_remotes_dialog, push_dialog, stash_picker};
use gpui::{App, Menu, MenuItem, OsAction};
use language_selector;
use line_ending_selector;
use php_panel;
use project_panel;
use release_channel::ReleaseChannel;
use search;
use snippets_ui;
use terminal_view::terminal_panel;
use zed_actions::{debug_panel, dev, git_panel, project_panel as zed_project_panel, Quit};

pub mod phpstorm_menu {
    use gpui::actions;

    actions!(
        phpstorm_menu,
        [
            AnnotateWithGitBlame,
            // File
            OpenInNewWindow,
            Print,
            NewProjectFromExistingFiles,
            PutLabel,
            RestoreDefaultSettings,
            BackupAndSync,
            SettingsForNewProjects,
            // Edit
            Macros,
            // View
            PresentationAssistant,
            ShowSource,
            RecentLocations,
            RecentlyChangedFiles,
            RecentChanges,
            QuickSwitchScheme,
            NavigationBar,
            BidiContentBased,
            BidiLeftToRight,
            BidiRightToLeft,
            // Navigate
            // Code
            // Refactor
            // Run
            ManageTargets,
            StopBackgroundProcesses,
            ShowRunningList,
            CompileAndReloadModifiedFiles,
            // Tools
            TasksSwitchTask,
            TasksOpenTask,
            TasksCloseActiveTask,
            TasksEditDefaultTask,
            TasksAddChangeList,
            TasksShowDescription,
            TasksOpenInBrowser,
            TasksAnalyzeStackTrace,
            TasksConfigureServers,
            TasksTimeTracking,
            TasksSaveContext,
            TasksLoadContext,
            TasksClearContext,
            SaveProjectAsTemplate,
            ManageProjectTemplates,
            ServicesDockerRegistry,
            ServicesDockerConnection,
            ServicesDockerFromContexts,
            ServicesDevContainersNewConfig,
            ServicesDevContainersManageBackends,
            ServicesDevContainersShow,
            MarkdownImportWordDocument,
            MarkdownExportMarkdownFileTo,
            MarkdownConfigurePandoc,
            ShowPluginLoadingState,
            AskAgentWithContext,
            AskAgentDependingOnContext,
            VagrantUp,
            VagrantReload,
            VagrantProvision,
            VagrantSuspend,
            VagrantResume,
            VagrantHalt,
            VagrantDestroy,
            VagrantShare,
            VagrantInitInProjectRoot,
            DbgpRegisterIde,
            DbgpCancelIdeRegistration,
            DbgpConfiguration,
            AnalyzeXdebugProfilerSnapshot,
            ComposerSelfUpdate,
            ComposerClearCache,
            ComposerShowLog,
            ComposerManageDependencies,
            ComposerValidate,
            ComposerDumpAutoloader,
            ComposerSimulateUpdate,
            ComposerCheckForChanges,
            ComposerListLicenses,
            RunCommand,
            RunGulpGruntNpmTask,
            QodanaTryAnalysis,
            QodanaAddToCi,
            QodanaLogIn,
            QodanaOpenLocalReport,
            // Git
            CreatePatchFromLocalChanges,
            ApplyPatch,
            ApplyPatchFromClipboard,
            ShowShelf,
            ShowLocalChangesAsUml,
            CompareWithRevision,
            CompareWithBranchOrTag,
            GitLabCreateMergeRequest,
            GitLabViewMergeRequests,
            GitLabCreateSnippet,
            GitLabShareProject,
            GitLabManageAccounts,
            GitHubCreatePullRequest,
            GitHubViewPullRequests,
            GitHubCreateGist,
            GitHubShareProject,
            GitHubManageAccounts,
            VcsOperationsPopup,
            // Window
            MergeAllProjectWindows,
            // Help
            GettingStarted,
            PhpWorkbenchOnYouTube,
            ContactSupport,
            SubmitBugReport,
            SubmitFeedback,
            CollectLogsAndDiagnosticData,
            DiagnosticActivityMonitor,
            DiagnosticDumpThreads,
            DiagnosticDebugLogSettings,
            DiagnosticSpecialFilesAndFolders,
            ChangeMemorySettings,
            EditCustomProperties,
            EditCustomVmOptions,
            DeleteLeftoverIdeDirectories,
            RegisterIdeLicense,
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
                MenuItem::action("Check for Updates…", zed_actions::CheckForUpdates),
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
                MenuItem::action("Open in New Window…", phpstorm_menu::OpenInNewWindow),
                MenuItem::action("Save", workspace::Save { save_intent: None }),
                MenuItem::action("Save As…", workspace::SaveAs),
                MenuItem::action("New Project from Existing Files…", phpstorm_menu::NewProjectFromExistingFiles),
                MenuItem::submenu(Menu::new("Recent Projects").items([
                    MenuItem::action("Manage Projects…", zed_actions::OpenRecent::default()),
                ])),
                MenuItem::action("Close Project", workspace::CloseWindow),
                MenuItem::action("Close All Projects", workspace::CloseAllItemsAndPanes::default()),
                MenuItem::action("Close Other Projects", workspace::CloseOtherProjects),
                MenuItem::action("Rename Project…", project_panel::Rename),
                MenuItem::separator(),
                MenuItem::action("Connect to Remote…", php_panel::OpenDeploymentDialog),
                MenuItem::separator(),
                MenuItem::action("Plugins…", zed_actions::Extensions::default()),
                MenuItem::submenu(Menu::new("File Properties").items([
                    MenuItem::action("Remove BOM", editor_actions::RemoveBom),
                    MenuItem::action("Add BOM", editor_actions::AddBom),
                    MenuItem::action("Associate with File Type…", language_selector::Toggle),
                    MenuItem::action("Make files read-only", workspace::ToggleReadOnlyFile),
                    MenuItem::submenu(Menu::new("Line Separators").items([
                        MenuItem::action("CRLF - Windows (\\r\\n)", line_ending_selector::SetCrlf),
                        MenuItem::action("LF - Unix and macOS (\\n)", line_ending_selector::SetLf),
                        MenuItem::action("CR - Classic Mac OS (\\r)", line_ending_selector::SetCr),
                        MenuItem::separator(),
                        MenuItem::action("Select Line Separator…", line_ending_selector::Toggle),
                    ])),
                    MenuItem::action("File Encoding…", encoding_selector::Toggle),
                ])),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Local History").items([
                    MenuItem::action("Show History", git::FileHistory),
                    MenuItem::action("Put Label…", phpstorm_menu::PutLabel),
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
                    MenuItem::action("Theme…", zed_actions::theme_selector::Toggle::default()),
                    MenuItem::separator(),
                    MenuItem::action("Import Settings…", zed_actions::OpenSettingsFile),
                    MenuItem::action("Export Settings…", zed_actions::OpenSettingsFile),
                    MenuItem::separator(),
                    MenuItem::action("Restore Default Settings…", phpstorm_menu::RestoreDefaultSettings),
                    MenuItem::action("Backup and Sync…", phpstorm_menu::BackupAndSync),
                ])),
                MenuItem::submenu(Menu::new("New Projects Setup").items([
                    MenuItem::action("Settings for New Projects…", phpstorm_menu::SettingsForNewProjects),
                ])),
                MenuItem::action("Save File as Template…", workspace::SaveFileAsTemplate),
                MenuItem::separator(),
                MenuItem::action("Export", zed_actions::OpenSettingsFile),
                MenuItem::action("Print…", phpstorm_menu::Print),
                MenuItem::separator(),
                MenuItem::action("Power Save Mode", workspace::PowerSaveMode),
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
                MenuItem::action("Copy as Plain Text", editor_actions::CopyAsPlainText),
                MenuItem::action("Copy Path/Reference…", zed_actions::workspace::CopyPath),
                MenuItem::os_action("Paste", editor_actions::Paste, OsAction::Paste),
                MenuItem::action("Delete", editor_actions::Delete),
                MenuItem::separator(),
                MenuItem::action("Search In Selection", search::ToggleSelection),
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
                MenuItem::action("Duplicate Selection", editor_actions::DuplicateLineDown),
                MenuItem::action("Fill Paragraph", editor_actions::Rewrap),
                MenuItem::action("Sort Lines", editor_actions::SortLinesCaseInsensitive),
                MenuItem::action("Reverse Lines", editor_actions::ReverseLines),
                MenuItem::action("Transpose", editor_actions::Transpose),
                MenuItem::separator(),
                MenuItem::action("Indent Selection", editor_actions::Indent),
                MenuItem::action("Unindent Line or Selection", editor_actions::Outdent),
                MenuItem::submenu(Menu::new("Convert Indents").items([
                    MenuItem::action("To Spaces", editor_actions::ConvertIndentationToSpaces),
                    MenuItem::action("To Tabs", editor_actions::ConvertIndentationToTabs),
                ])),
                MenuItem::separator(),
                MenuItem::action("Macros", phpstorm_menu::Macros),
                MenuItem::submenu(Menu::new("Bookmarks").items([
                    MenuItem::action("Previous Line Bookmark", editor_actions::GoToPreviousBookmark),
                    MenuItem::action("Next Line Bookmark", editor_actions::GoToNextBookmark),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Bookmark", editor_actions::ToggleBookmark),
                    MenuItem::action(
                        "Toggle Bookmark with Label",
                        editor_actions::ToggleBookmarkWithLabel,
                    ),
                    MenuItem::action("Show Line Bookmarks…", editor_actions::ViewBookmarks),
                    MenuItem::action("Go to Mnemonic…", editor_actions::ViewBookmarks),
                ])),
                MenuItem::separator(),
                MenuItem::action("Emoji & Symbols", editor_actions::ShowCharacterPalette),
                MenuItem::action("Encode XML/HTML Special Characters", editor_actions::EncodeXmlHtmlSpecialCharacters),
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
                        MenuItem::action("Commit (⌘0)", git_panel::ToggleFocus),
                        MenuItem::action("Project (⌘1)", zed_project_panel::ToggleFocus),
                        MenuItem::action("Agent Sessions", zed_actions::assistant::ToggleFocus),
                        MenuItem::action("Find (⌘3)", workspace::DeploySearch::default()),
                        MenuItem::action("Run (⌘4)", zed_actions::Spawn::ViaModal { reveal_target: None }),
                        MenuItem::action("Debug (⌘5)", debug_panel::ToggleFocus),
                        MenuItem::action("Problems (⌘6)", diagnostics::Deploy),
                        MenuItem::action("Structure (⌘7)", outline_panel::ToggleFocus),
                        MenuItem::action("Services (⌘8)", php_panel::ToggleFocus),
                        MenuItem::action("Git (⌘9)", git_panel::ToggleFocus),
                        MenuItem::action("Backup and Sync History", phpstorm_menu::BackupAndSync),
                        MenuItem::action("Bookmarks", editor_actions::ViewBookmarks),
                        MenuItem::action("Coverage", workspace::ManageCoverageReports),
                        MenuItem::action("Database", php_panel::ToggleFocus),
                        MenuItem::action("Database Changes", php_panel::ToggleFocus),
                        MenuItem::action("Database Output", php_panel::ToggleFocus),
                        MenuItem::action("File Transfer", php_panel::BrowseRemoteHost),
                        MenuItem::action("Hierarchy", outline_panel::ToggleFocus),
                        MenuItem::action("Laravel", php_panel::ToggleFocus),
                        MenuItem::action("make", zed_actions::Spawn::ViaModal { reveal_target: None }),
                        MenuItem::action("Merge Requests", phpstorm_menu::GitLabViewMergeRequests),
                        MenuItem::action("Notifications", workspace::ToggleNotifications),
                        MenuItem::action("npm", zed_actions::Spawn::ViaModal { reveal_target: None }),
                        MenuItem::action("Pull Requests", git_panel::ToggleFocus),
                        MenuItem::action("Remote Host", php_panel::BrowseRemoteHost),
                        MenuItem::action("Terminal (⌥F12)", terminal_panel::Toggle),
                        MenuItem::action("TODO", diagnostics::Deploy),
                        MenuItem::action("TypeScript", diagnostics::Deploy),
                    ])),
                    MenuItem::submenu(Menu::new("Appearance").items([
                        MenuItem::action("Enter Presentation Mode", workspace::TogglePresentationMode),
                        MenuItem::action("Enter Distraction Free Mode", workspace::ToggleCenteredLayout),
                        MenuItem::action("Enter Full Screen", super::ToggleFullScreen),
                        MenuItem::action("Enter Zen Mode", workspace::ToggleCenteredLayout),
                        MenuItem::separator(),
                        MenuItem::action("Compact Mode", workspace::ToggleCompactMode),
                        MenuItem::action("Zoom IDE (Current: 100%)…", workspace::CycleIdeZoom),
                        MenuItem::separator(),
                        MenuItem::action("Presentation Assistant", phpstorm_menu::PresentationAssistant),
                        MenuItem::separator(),
                        MenuItem::action("Toolbar", workspace::ToggleAllDocks),
                        MenuItem::action("Navigation Bar", phpstorm_menu::NavigationBar),
                        MenuItem::action("Tool Window Bars", workspace::ToggleAllDocks),
                        MenuItem::action("Status Bar", workspace::ToggleStatusBar),
                        MenuItem::submenu(Menu::new("Status Bar Widgets").items([
                            MenuItem::action("Status Text", workspace::ToggleStatusTextWidget),
                            MenuItem::separator(),
                            MenuItem::action("File System Sync", workspace::ToggleFileSystemSyncWidget),
                            MenuItem::action("PHP Language Level", workspace::TogglePhpLanguageLevelWidget),
                            MenuItem::action("Default Deployment Server", workspace::ToggleDefaultDeploymentServerWidget),
                            MenuItem::action("Line:Column Number", workspace::ToggleLineColumnWidget),
                            MenuItem::action("Language Services", workspace::ToggleLanguageServicesWidget),
                            MenuItem::action("Line Separator", workspace::ToggleLineSeparatorWidget),
                            MenuItem::action("File Encoding", workspace::ToggleFileEncodingWidget),
                            MenuItem::action("Power Save Mode", workspace::TogglePowerSaveModeWidget),
                            MenuItem::action("Editor Selection Mode", workspace::ToggleEditorSelectionModeWidget),
                            MenuItem::action("Indentation", workspace::ToggleIndentationWidget),
                            MenuItem::action("JSON Schema", workspace::ToggleJsonSchemaWidget),
                            MenuItem::action("MCP Server", workspace::ToggleMcpServerWidget),
                            MenuItem::action("Stop/Start Debug Listening", workspace::ToggleDebugListeningWidget),
                            MenuItem::action("Git Branch", workspace::ToggleGitBranchWidget),
                            MenuItem::action("Read-Only Attribute", workspace::ToggleReadOnlyAttributeWidget),
                            MenuItem::action("Memory Indicator", workspace::ToggleMemoryIndicatorWidget),
                        ])),
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
                    MenuItem::separator(),
                    MenuItem::action(
                        "Jump to Source",
                        editor_actions::GoToDeclaration::default(),
                    ),
                    MenuItem::action("Show Source", phpstorm_menu::ShowSource),
                    MenuItem::separator(),
                    MenuItem::action("Recent Locations", phpstorm_menu::RecentLocations),
                    MenuItem::action("Recent Files", workspace::ToggleFileFinder::default()),
                    MenuItem::action("Recently Changed Files", phpstorm_menu::RecentlyChangedFiles),
                    MenuItem::action("Recent Changes", phpstorm_menu::RecentChanges),
                    MenuItem::separator(),
                    MenuItem::action("Compare Files", git_ui::project_diff::CompareWithBranch),
                    MenuItem::action(
                        "Compare with Clipboard",
                        editor_actions::DiffClipboardWithSelection,
                    ),
                    MenuItem::separator(),
                    MenuItem::action("Quick Switch Scheme…", phpstorm_menu::QuickSwitchScheme),
                    MenuItem::separator(),
                    MenuItem::action("Reset Font Size in All Editors", zed_actions::ResetBufferFontSize { persist: true }),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu::new("Bidi Text Base Direction").items([
                        MenuItem::action("Content-Based", phpstorm_menu::BidiContentBased),
                        MenuItem::action("Left-to-Right", phpstorm_menu::BidiLeftToRight),
                        MenuItem::action("Right-to-Left", phpstorm_menu::BidiRightToLeft),
                    ])),
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
                MenuItem::separator(),
                MenuItem::action("Class...", workspace::ToggleProjectSymbols),
                MenuItem::action("File…", workspace::ToggleFileFinder::default()),
                MenuItem::action("Symbol…", workspace::ToggleProjectSymbols),
                MenuItem::action("Text…", workspace::DeploySearch::default()),
                MenuItem::action("Database Object…", php_panel::OpenDatabaseConsole),
                MenuItem::separator(),
                MenuItem::action("Next Highlighted Error", editor_actions::GoToDiagnostic::default()),
                MenuItem::action(
                    "Previous Highlighted Error",
                    editor_actions::GoToPreviousDiagnostic::default(),
                ),
                MenuItem::separator(),
                MenuItem::action("Next Emmet Edit Point", editor_actions::NextEmmetEditPoint),
                MenuItem::action("Previous Emmet Edit Point", editor_actions::PreviousEmmetEditPoint),
                MenuItem::action("Last Edit Location", workspace::GoBack),
                MenuItem::action("Next Edit Location", workspace::GoForward),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Navigate in File").items([
                    MenuItem::action("Next Method", editor_actions::NextMethod),
                    MenuItem::action("Previous Method", editor_actions::PreviousMethod),
                    MenuItem::action("Move Caret to Matching Brace", editor_actions::MoveToEnclosingBracket),
                    MenuItem::separator(),
                    MenuItem::action("Next Live Template Parameter", editor_actions::NextSnippetTabstop),
                    MenuItem::action("Previous Live Template Parameter", editor_actions::PreviousSnippetTabstop),
                ])),
                MenuItem::separator(),
                MenuItem::action("Jump to Navigation Bar", zed_actions::outline::ToggleOutline),
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
                MenuItem::action("Super Method", editor_actions::GoToImplementation::default()),
                MenuItem::action("Related Symbol…", editor_actions::FindAllReferences::default()),
                MenuItem::separator(),
                MenuItem::action("File Structure", zed_actions::outline::ToggleOutline),
                MenuItem::action("Type Hierarchy", editor_actions::GoToTypeDefinition::default()),
                MenuItem::action("Method Hierarchy", call_hierarchy::ShowIncomingCalls),
                MenuItem::action("Call Hierarchy", call_hierarchy::ShowIncomingCalls),
            ],
        },

        // =========================================================================
        // 6. Code
        // =========================================================================
        Menu {
            name: "Code".into(),
            disabled: false,
            items: vec![
                MenuItem::action("Override Methods…", editor_actions::ToggleCodeActions::default()),
                MenuItem::action("Implement Methods…", editor_actions::ToggleCodeActions::default()),
                MenuItem::action("Generate…", editor_actions::ToggleCodeActions::default()),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Code Completion").items([
                    MenuItem::action("Basic", editor_actions::ShowCompletions),
                    MenuItem::action("Type-Matching", editor_actions::ShowCompletions),
                    MenuItem::separator(),
                    MenuItem::action("Complete Current Statement", editor_actions::CompleteCurrentStatement),
                    MenuItem::separator(),
                    MenuItem::action("Cyclic Expand Word", editor_actions::CyclicExpandWord),
                    MenuItem::action("Cyclic Expand Word (Backward)", editor_actions::CyclicExpandWordBackward),
                    MenuItem::separator(),
                    MenuItem::action("Call Inline Completion", editor_actions::ShowEditPrediction),
                    MenuItem::action("Insert Inline Proposal", editor_actions::AcceptEditPrediction),
                    MenuItem::action("Enable Command Completion", editor_actions::ShowCompletions),
                ])),
                MenuItem::separator(),
                MenuItem::action("Inspect Code…", diagnostics::Deploy),
                MenuItem::action("Code Cleanup…", editor_actions::Format),
                MenuItem::submenu(Menu::new("Analyze Code").items([
                    MenuItem::action("Silent Code Cleanup", editor_actions::Format),
                    MenuItem::action("Run Inspection by Name…", diagnostics::Deploy),
                    MenuItem::action("Configure Current File Analysis…", diagnostics::Deploy),
                    MenuItem::action("View Offline Inspection Results…", diagnostics::Deploy),
                    MenuItem::action("Locate Duplicates…", workspace::DeploySearch::default()),
                    MenuItem::separator(),
                    MenuItem::action("Data Flow to Here…", editor_actions::FindAllReferences::default()),
                    MenuItem::action("Data Flow from Here…", editor_actions::GoToDefinition::default()),
                    MenuItem::separator(),
                    MenuItem::action("Vulnerable Dependencies…", diagnostics::Deploy),
                ])),
                MenuItem::action("Analyze Stack Trace or Thread Dump…", workspace::DeploySearch::default()),
                MenuItem::action("Detect PSR-0 Namespace Roots", editor_actions::OrganizeImports),
                MenuItem::separator(),
                MenuItem::action("Insert Live Template…", editor_actions::ShowWordCompletions),
                MenuItem::action("Save as Live Template…", snippets_ui::ConfigureSnippets),
                MenuItem::separator(),
                MenuItem::action("Surround With…", editor_actions::SurroundWith),
                MenuItem::action("Unwrap/Remove…", editor_actions::UnwrapSyntaxNode),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Folding").items([
                    MenuItem::action("Expand", editor_actions::Unfold),
                    MenuItem::action("Expand Recursively", editor_actions::UnfoldRecursive),
                    MenuItem::action("Expand All", editor_actions::UnfoldAll),
                    MenuItem::separator(),
                    MenuItem::action("Collapse", editor_actions::Fold),
                    MenuItem::action("Collapse Recursively", editor_actions::FoldRecursive),
                    MenuItem::action("Collapse All", editor_actions::FoldAll),
                    MenuItem::separator(),
                    MenuItem::action("Expand to Level", editor_actions::UnfoldAll),
                    MenuItem::action("Expand All to Level", editor_actions::UnfoldAll),
                    MenuItem::separator(),
                    MenuItem::action("Expand Doc Comments", editor_actions::ExpandDocComments),
                    MenuItem::action("Collapse Doc Comments", editor_actions::CollapseDocComments),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Folding", editor_actions::ToggleFold),
                    MenuItem::separator(),
                    MenuItem::action("Fold Selection / Remove Region", editor_actions::FoldSelectedRanges),
                    MenuItem::action("Fold Code Block", editor_actions::Fold),
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
                MenuItem::action("Reformat File…", editor_actions::Format),
                MenuItem::action("Auto-Indent Lines", editor_actions::AutoIndent),
                MenuItem::action("Optimize Imports", editor_actions::OrganizeImports),
                MenuItem::action("Rearrange Code", editor_actions::Format),
                MenuItem::separator(),
                MenuItem::action("Move Statement Down", editor_actions::MoveLineDown),
                MenuItem::action("Move Statement Up", editor_actions::MoveLineUp),
                MenuItem::action("Move Element Left", editor_actions::MoveToBeginningOfLine { stop_at_soft_wraps: false, stop_at_indent: false }),
                MenuItem::action("Move Element Right", editor_actions::MoveToEndOfLine { stop_at_soft_wraps: false }),
                MenuItem::action("Move Line Down", editor_actions::MoveLineDown),
                MenuItem::action("Move Line Up", editor_actions::MoveLineUp),
                MenuItem::separator(),
                MenuItem::action("Update Copyright…", editor_actions::Format),
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
                MenuItem::action("Change Signature…", editor_actions::ChangeSignature),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Extract/Introduce").items([
                    MenuItem::action("Variable…", editor_actions::ToggleCodeActions::default()),
                    MenuItem::action("Constant…", editor_actions::ToggleCodeActions::default()),
                    MenuItem::action("Field…", editor_actions::ToggleCodeActions::default()),
                    MenuItem::action("Parameter…", editor_actions::ToggleCodeActions::default()),
                    MenuItem::action("Method…", editor_actions::ToggleCodeActions::default()),
                ])),
                MenuItem::separator(),
                MenuItem::action("Move…", editor_actions::Rename),
                MenuItem::action("Copy File…", zed_actions::workspace::CopyPath),
                MenuItem::action("Safe Delete…", editor_actions::SafeDelete),
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
                    "Run 'Current'",
                    zed_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::action("Debug 'Current'", debugger_ui::Start),
                MenuItem::action("Run with Coverage", workspace::RunWithCoverage),
                MenuItem::action("Run in Parallel", workspace::RunWithParallel),
                MenuItem::action("Run with Mutation", workspace::RunWithMutation),
                MenuItem::action("Run with Profiler", workspace::RunWithProfiler),
                MenuItem::separator(),
                MenuItem::action(
                    "Run…",
                    zed_actions::Spawn::ViaModal {
                        reveal_target: None,
                    },
                ),
                MenuItem::action("Debug…", debugger_ui::Start),
                MenuItem::action("Edit Configurations…", zed_actions::OpenProjectDebugTasks),
                MenuItem::action("Manage Targets…", phpstorm_menu::ManageTargets),
                MenuItem::separator(),
                MenuItem::action("Stop", debugger_ui::Stop),
                MenuItem::action("Stop Background Processes…", phpstorm_menu::StopBackgroundProcesses),
                MenuItem::action("Show Running List", phpstorm_menu::ShowRunningList),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Debugging Actions").items([
                    MenuItem::action("Compile and Reload Modified Files", phpstorm_menu::CompileAndReloadModifiedFiles),
                    MenuItem::separator(),
                    MenuItem::action("Step Over", debugger_ui::StepOver),
                    MenuItem::action("Force Step Over", debugger_ui::StepOver),
                    MenuItem::action("Step Into", debugger_ui::StepInto),
                    MenuItem::action("Force Step Into", debugger_ui::StepInto),
                    MenuItem::action("Smart Step Into", debugger_ui::StepInto),
                    MenuItem::action("Step Out", debugger_ui::StepOut),
                    MenuItem::action("Run to Cursor", workspace::RunToCursor),
                    MenuItem::action("Reset Frame", workspace::ResetFrame),
                    MenuItem::action("Resume Program", debugger_ui::Continue),
                    MenuItem::separator(),
                    MenuItem::action("Evaluate Expression…", debugger_ui::FocusConsole),
                    MenuItem::action("Show Execution Point", debugger_ui::FocusFrames),
                ])),
                MenuItem::submenu(Menu::new("Toggle Breakpoint").items([
                    MenuItem::action("Line Breakpoint", editor_actions::ToggleBreakpoint),
                    MenuItem::action("Temporary Line Breakpoint", editor_actions::ToggleBreakpoint),
                    MenuItem::action("Toggle Breakpoint Enabled", editor_actions::ToggleBreakpointEnabled),
                ])),
                MenuItem::action("View Breakpoints…", editor_actions::EditLogBreakpoint),
                MenuItem::separator(),
                MenuItem::action("Test History", workspace::TestHistory),
                MenuItem::action("Import Tests from File…", workspace::ImportTestsFromFile),
                MenuItem::action("Manage Coverage Reports…", workspace::ManageCoverageReports),
                MenuItem::separator(),
                MenuItem::action(
                    "Start Listening for PHP Debug Connections",
                    workspace::StartListeningForPhpDebugConnections,
                ),
                MenuItem::action("Break at first line in PHP scripts", workspace::BreakAtFirstLineInPhpScripts),
                MenuItem::action("Web Server Debug Validation", workspace::WebServerDebugValidation),
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

                items.push(MenuItem::submenu(Menu::new("Tasks & Contexts").items([
                    MenuItem::action("Switch Task…", phpstorm_menu::TasksSwitchTask),
                    MenuItem::action("Open Task…", phpstorm_menu::TasksOpenTask),
                    MenuItem::action("Close Active Task…", phpstorm_menu::TasksCloseActiveTask),
                    MenuItem::separator(),
                    MenuItem::action("Edit 'Default task'", phpstorm_menu::TasksEditDefaultTask),
                    MenuItem::action("Add Change List for 'Default task'", phpstorm_menu::TasksAddChangeList),
                    MenuItem::action("Show Description", phpstorm_menu::TasksShowDescription),
                    MenuItem::action("Open in Browser", phpstorm_menu::TasksOpenInBrowser),
                    MenuItem::action("Analyze Stack Trace From Task…", phpstorm_menu::TasksAnalyzeStackTrace),
                    MenuItem::separator(),
                    MenuItem::action("Configure Servers…", phpstorm_menu::TasksConfigureServers),
                    MenuItem::action("Time Tracking", phpstorm_menu::TasksTimeTracking),
                    MenuItem::separator(),
                    MenuItem::action("Save Context…", phpstorm_menu::TasksSaveContext),
                    MenuItem::action("Load Context…", phpstorm_menu::TasksLoadContext),
                    MenuItem::action("Clear Context", phpstorm_menu::TasksClearContext),
                ])));

                items.push(MenuItem::action("Save Project as Template…", phpstorm_menu::SaveProjectAsTemplate));
                items.push(MenuItem::action("Manage Project Templates…", phpstorm_menu::ManageProjectTemplates));
                #[cfg(not(target_os = "windows"))]
                items.push(MenuItem::action("Create Command Line Launcher…", install_cli::InstallCliBinary));
                items.push(MenuItem::separator());

                items.push(MenuItem::submenu(Menu::new("Services").items([
                    MenuItem::action("Docker Registry…", phpstorm_menu::ServicesDockerRegistry),
                    MenuItem::submenu(Menu::new("Docker").items([
                        MenuItem::action("Connection…", phpstorm_menu::ServicesDockerConnection),
                        MenuItem::action("Connections From Contexts", phpstorm_menu::ServicesDockerFromContexts),
                    ])),
                    MenuItem::submenu(Menu::new("Dev Containers").items([
                        MenuItem::action("New Dev Container Configuration…", phpstorm_menu::ServicesDevContainersNewConfig),
                        MenuItem::action("Manage Dev Container Backends", phpstorm_menu::ServicesDevContainersManageBackends),
                        MenuItem::action("Show Dev Containers", phpstorm_menu::ServicesDevContainersShow),
                    ])),
                ])));

                items.push(MenuItem::submenu(Menu::new("Markdown").items([
                    MenuItem::action("Import Word Document…", phpstorm_menu::MarkdownImportWordDocument),
                    MenuItem::action("Export Markdown File To…", phpstorm_menu::MarkdownExportMarkdownFileTo),
                    MenuItem::action("Configure Pandoc…", phpstorm_menu::MarkdownConfigurePandoc),
                ])));

                items.push(MenuItem::submenu(Menu::new("XML Actions").items([
                    MenuItem::action("Convert Schema File…", php_panel::ConvertSchemaFileDialog),
                    MenuItem::action("Generate DTD from XML File…", php_panel::ConvertSchemaFileDialog),
                    MenuItem::action("Validate XML…", php_panel::ConvertSchemaFileDialog),
                ])));

                items.push(MenuItem::action("Show Plugin Loading State", phpstorm_menu::ShowPluginLoadingState));
                items.push(MenuItem::action("Ask Agent with Context", phpstorm_menu::AskAgentWithContext));
                items.push(MenuItem::action("Ask Agent Depending on Context", phpstorm_menu::AskAgentDependingOnContext));
                items.push(MenuItem::action("Launch AI Agent Terminal", php_panel::LaunchActiveAiAgent));
                items.push(MenuItem::action("AI CLI Settings…", php_panel::OpenAiCliSettings));

                items.push(MenuItem::submenu(Menu::new("Vagrant").items([
                    MenuItem::action("Up", phpstorm_menu::VagrantUp),
                    MenuItem::action("Reload", phpstorm_menu::VagrantReload),
                    MenuItem::action("Provision", phpstorm_menu::VagrantProvision),
                    MenuItem::action("Suspend", phpstorm_menu::VagrantSuspend),
                    MenuItem::action("Resume", phpstorm_menu::VagrantResume),
                    MenuItem::action("Halt", phpstorm_menu::VagrantHalt),
                    MenuItem::action("Destroy", phpstorm_menu::VagrantDestroy),
                    MenuItem::action("Share", phpstorm_menu::VagrantShare),
                    MenuItem::action("Init in Project Root", phpstorm_menu::VagrantInitInProjectRoot),
                ])));

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
                    let auto_upload_title = if is_auto_upload {
                        "✓ Automatic Upload (always)"
                    } else {
                        "   Automatic Upload (always)"
                    };
                    dep_items.push(MenuItem::action(auto_upload_title, php_panel::ToggleAutomaticUpload).checked(is_auto_upload));
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
                    MenuItem::action("Open HTTP Requests Collection", php_panel::OpenHttpClient),
                    MenuItem::action("Show HTTP Requests History", php_panel::OpenHttpClient),
                    MenuItem::action("Convert cURL to HTTP Request", php_panel::CreateHttpRequest),
                    MenuItem::action("Add to HTTP Client…", php_panel::CreateHttpRequest),
                    MenuItem::action("Import from Postman Collection File", phpstorm_menu::DbgpConfiguration),
                ])));

                items.push(MenuItem::submenu(Menu::new("DBGp Proxy").items([
                    MenuItem::action("Register IDE", phpstorm_menu::DbgpRegisterIde),
                    MenuItem::action("Cancel IDE Registration", phpstorm_menu::DbgpCancelIdeRegistration),
                    MenuItem::separator(),
                    MenuItem::action("Configuration…", phpstorm_menu::DbgpConfiguration),
                ])));

                items.push(MenuItem::action("Analyze Xdebug Profiler Snapshot…", phpstorm_menu::AnalyzeXdebugProfilerSnapshot));

                items.push(MenuItem::submenu(Menu::new("Composer").items([
                    MenuItem::action("Init Composer…", php_panel::RunComposer),
                    MenuItem::action("Self-Update", phpstorm_menu::ComposerSelfUpdate),
                    MenuItem::action("Clear Cache", phpstorm_menu::ComposerClearCache),
                    MenuItem::action("Show Log", phpstorm_menu::ComposerShowLog),
                    MenuItem::separator(),
                    MenuItem::action("Install", php_panel::RunComposer),
                    MenuItem::action("Update", php_panel::RunComposer),
                    MenuItem::action("Manage Dependencies…", phpstorm_menu::ComposerManageDependencies),
                    MenuItem::action("Validate…", phpstorm_menu::ComposerValidate),
                    MenuItem::action("Dump Autoloader…", phpstorm_menu::ComposerDumpAutoloader),
                    MenuItem::action("Simulate Update", phpstorm_menu::ComposerSimulateUpdate),
                    MenuItem::action("Check for Changes in Dependencies", phpstorm_menu::ComposerCheckForChanges),
                    MenuItem::action("List Licenses", phpstorm_menu::ComposerListLicenses),
                    MenuItem::action("Diagnose", php_panel::RunComposer),
                ])));

                items.push(MenuItem::action("Run Command...", phpstorm_menu::RunCommand));
                items.push(MenuItem::action("Run Gulp/Grunt/npm Task", phpstorm_menu::RunGulpGruntNpmTask));

                items.push(MenuItem::submenu(Menu::new("Qodana").items([
                    MenuItem::action("Try Code Analysis with Qodana", phpstorm_menu::QodanaTryAnalysis),
                    MenuItem::action("Add Qodana to CI Pipeline", phpstorm_menu::QodanaAddToCi),
                    MenuItem::action("Log in to Qodana…", phpstorm_menu::QodanaLogIn),
                    MenuItem::separator(),
                    MenuItem::action("Open Local Report…", phpstorm_menu::QodanaOpenLocalReport),
                ])));

                items
            },
        },

        // =========================================================================
        // 10. Git (100% matched to PhpStorm)
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
                MenuItem::action("Merge…", git::Merge),
                MenuItem::action("Rebase…", git::PullRebase),
                MenuItem::separator(),
                MenuItem::action("Branches…", git_picker::ActivateBranchesTab),
                MenuItem::action("New Branch…", zed_actions::git::Branch),
                MenuItem::action("New Tag…", git::CreateTagAtHead),
                MenuItem::action("Reset HEAD…", git::RestoreTrackedFiles),
                MenuItem::separator(),
                MenuItem::action("New Worktree…", zed_actions::CreateWorktree::default()),
                MenuItem::action("Worktrees…", zed_actions::SwitchWorktree::default()),
                MenuItem::separator(),
                MenuItem::action("Show Git Log", git_panel::ToggleFocus),
                MenuItem::submenu(Menu::new("Patch").items([
                    MenuItem::action("Create Patch from Local Changes…", phpstorm_menu::CreatePatchFromLocalChanges),
                    MenuItem::action("Apply Patch…", phpstorm_menu::ApplyPatch),
                    MenuItem::action("Apply Patch from Clipboard…", phpstorm_menu::ApplyPatchFromClipboard),
                ])),
                MenuItem::submenu(Menu::new("Uncommitted Changes").items([
                    MenuItem::action("Shelve Changes…", git::StashAll),
                    MenuItem::action("Show Shelf", phpstorm_menu::ShowShelf),
                    MenuItem::action("Show Git Stash", git_ui_panel::ActivateStashTab),
                    MenuItem::action("Stash Changes…", git::StashAll),
                    MenuItem::action("Unstash Changes…", stash_picker::Unstash),
                    MenuItem::action("Rollback…", git::RestoreTrackedFiles),
                    MenuItem::action("Show Local Changes as UML", phpstorm_menu::ShowLocalChangesAsUml),
                ])),
                MenuItem::submenu(Menu::new("Selected File").items([
                    MenuItem::action("Commit File…", git_panel::ToggleFocus),
                    MenuItem::action("Add", git::StageFile),
                    MenuItem::separator(),
                    MenuItem::action("Annotate with Git Blame", git::Blame),
                    MenuItem::action("Show Diff", git_ui::project_diff::CompareWithBranch),
                    MenuItem::action("Compare with Revision…", phpstorm_menu::CompareWithRevision),
                    MenuItem::action("Compare with Branch or Tag…", phpstorm_menu::CompareWithBranchOrTag),
                    MenuItem::action("Show History", git::FileHistory),
                ])),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("GitLab").items([
                    MenuItem::action("Create Merge Request…", phpstorm_menu::GitLabCreateMergeRequest),
                    MenuItem::action("View Merge Requests", phpstorm_menu::GitLabViewMergeRequests),
                    MenuItem::action("Create Snippet…", phpstorm_menu::GitLabCreateSnippet),
                    MenuItem::separator(),
                    MenuItem::action("Share Project on GitLab", phpstorm_menu::GitLabShareProject),
                    MenuItem::action("Clone Repository…", git::Clone),
                    MenuItem::separator(),
                    MenuItem::action("Manage Accounts…", phpstorm_menu::GitLabManageAccounts),
                ])),
                MenuItem::submenu(Menu::new("GitHub").items([
                    MenuItem::action("Create Pull Request…", phpstorm_menu::GitHubCreatePullRequest),
                    MenuItem::action("View Pull Requests", phpstorm_menu::GitHubViewPullRequests),
                    MenuItem::action("Create Gist…", phpstorm_menu::GitHubCreateGist),
                    MenuItem::separator(),
                    MenuItem::action("Share Project on GitHub", phpstorm_menu::GitHubShareProject),
                    MenuItem::action("Clone Repository…", git::Clone),
                    MenuItem::separator(),
                    MenuItem::action("Manage Accounts…", phpstorm_menu::GitHubManageAccounts),
                ])),
                MenuItem::action("Manage Remotes…", manage_remotes_dialog::ManageRemotes),
                MenuItem::action("Clone…", git::Clone),
                MenuItem::separator(),
                MenuItem::action("VCS Operations Popup…", phpstorm_menu::VcsOperationsPopup),
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
                    MenuItem::action("Default", workspace::ToggleAllDocks),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu::new("Custom").items([
                        MenuItem::action("Restore", workspace::LayoutCustomRestore),
                        MenuItem::action("Save Changes", workspace::LayoutCustomSaveChanges),
                        MenuItem::action("Rename", workspace::LayoutCustomRename),
                        MenuItem::separator(),
                        MenuItem::action("Delete…", workspace::LayoutCustomDelete),
                    ])),
                    MenuItem::separator(),
                    MenuItem::action("Save Current Layout as New…", workspace::ToggleAllDocks),
                ])),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Active Tool Window").items([
                    MenuItem::action("Hide Active Window", workspace::CloseActiveDock),
                    MenuItem::action("Hide All Tool Windows", workspace::ToggleAllDocks),
                    MenuItem::action("Jump to Last Tool Window", workspace::JumpToLastToolWindow),
                ])),
                MenuItem::submenu(Menu::new("Editor Tabs").items([
                    MenuItem::action("Select Next Tab", workspace::ActivateNextItem::default()),
                    MenuItem::action("Select Previous Tab", workspace::ActivatePreviousItem::default()),
                    MenuItem::action("Pin Tab", workspace::TogglePinTab),
                    MenuItem::action("Keep Tab Open", workspace::TogglePinTab),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Close Tab",
                        workspace::CloseActiveItem {
                            save_intent: None,
                            close_pinned: true,
                        },
                    ),
                    MenuItem::action("Close Other Tabs", workspace::CloseInactiveTabsAndPanes::default()),
                    MenuItem::action("Close All Tabs", workspace::CloseAllItemsAndPanes::default()),
                    MenuItem::action("Close Unmodified Tabs", workspace::CloseCleanItems::default()),
                    MenuItem::action("Close All but Pinned", workspace::CloseAllButPinned::default()),
                    MenuItem::action("Close Tabs to the Left", workspace::CloseItemsToTheLeft::default()),
                    MenuItem::action("Close Tabs to the Right", workspace::CloseItemsToTheRight::default()),
                    MenuItem::action("Close All Read-Only", workspace::CloseAllReadOnly),
                    MenuItem::separator(),
                    MenuItem::action("Reopen Closed Tab", workspace::ReopenClosedItem),
                    MenuItem::action("Split Right", workspace::SplitRight::default()),
                    MenuItem::action("Split Down", workspace::SplitDown::default()),
                    MenuItem::action("Stretch Editor to Top", workspace::StretchEditorTop),
                    MenuItem::action("Stretch Editor to Left", workspace::StretchEditorLeft),
                    MenuItem::action("Stretch Editor to Bottom", workspace::StretchEditorBottom),
                    MenuItem::action("Stretch Editor to Right", workspace::StretchEditorRight),
                    MenuItem::action("Change Splitter Orientation", workspace::ChangeSplitterOrientation),
                    MenuItem::action("Maximize Editor/Normalize Splits", workspace::MaximizeEditorNormalizeSplits),
                    MenuItem::action("Unsplit", workspace::Unsplit),
                    MenuItem::action("Unsplit All", workspace::UnsplitAll),
                    MenuItem::action("Go to Next Splitter", workspace::ActivateNextPane),
                    MenuItem::action("Go to Previous Splitter", workspace::ActivatePreviousPane),
                    MenuItem::separator(),
                    MenuItem::action("Configure Editor Tabs…", workspace::ConfigureEditorTabs),
                ])),
                MenuItem::action("Notifications", workspace::ToggleNotifications),
                MenuItem::action("Processes", terminal_panel::Toggle),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Project Window",
                    workspace::ActivateNextWindow,
                ),
                MenuItem::action(
                    "Previous Project Window",
                    workspace::ActivatePreviousWindow,
                ),
                MenuItem::action("Merge All Project Windows", phpstorm_menu::MergeAllProjectWindows),
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
                MenuItem::separator(),
                MenuItem::action("Help", zed_actions::OpenOnboarding),
                MenuItem::separator(),
                MenuItem::action("What's New in PhpWorkbench", zed_actions::About),
                MenuItem::action("Check for Updates…", zed_actions::CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Getting Started", phpstorm_menu::GettingStarted),
                MenuItem::action("PhpWorkbench on YouTube", phpstorm_menu::PhpWorkbenchOnYouTube),
                MenuItem::action("Keyboard Shortcuts PDF", zed_actions::OpenDefaultKeymap),
                MenuItem::separator(),
                MenuItem::action("Contact Support…", phpstorm_menu::ContactSupport),
                MenuItem::action("Submit a Bug Report…", phpstorm_menu::SubmitBugReport),
                MenuItem::action("Submit Feedback…", phpstorm_menu::SubmitFeedback),
                MenuItem::separator(),
                MenuItem::action("Show Log in Finder", workspace::RevealLogInFileManager),
                MenuItem::action("Collect Logs and Diagnostic Data", phpstorm_menu::CollectLogsAndDiagnosticData),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Diagnostic Tools").items([
                    MenuItem::action("Activity Monitor…", phpstorm_menu::DiagnosticActivityMonitor),
                    MenuItem::action("Dump Threads", phpstorm_menu::DiagnosticDumpThreads),
                    MenuItem::separator(),
                    MenuItem::action("Debug Log Settings…", phpstorm_menu::DiagnosticDebugLogSettings),
                    MenuItem::action("Special Files and Folders…", phpstorm_menu::DiagnosticSpecialFilesAndFolders),
                ])),
                MenuItem::action("Change Memory Settings", phpstorm_menu::ChangeMemorySettings),
                MenuItem::action("Edit Custom Properties…", phpstorm_menu::EditCustomProperties),
                MenuItem::action("Edit Custom VM Options…", phpstorm_menu::EditCustomVmOptions),
                MenuItem::action("Delete Leftover IDE Directories…", phpstorm_menu::DeleteLeftoverIdeDirectories),
                MenuItem::separator(),
                MenuItem::action("Register…", phpstorm_menu::RegisterIdeLicense),
            ],
        },
    ]
}
