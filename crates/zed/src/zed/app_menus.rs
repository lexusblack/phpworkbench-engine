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
            // File
            PowerSaveMode,
            Print,
            NewProjectFromExistingFiles,
            CloseOtherProjects,
            MakeFileReadOnly,
            PutLabel,
            RestoreDefaultSettings,
            BackupAndSync,
            SettingsForNewProjects,
            SaveFileAsTemplate,
            // Edit
            CopyAsPlainText,
            SearchInSelection,
            FillParagraph,
            Transpose,
            Macros,
            EncodeXmlHtmlSpecialCharacters,
            // View
            EnterPresentationMode,
            EnterZenMode,
            CompactMode,
            ZoomIde,
            PresentationAssistant,
            ShowSource,
            RecentLocations,
            RecentlyChangedFiles,
            RecentChanges,
            QuickSwitchScheme,
            ResetFontSizeInAllEditors,
            NavigationBar,
            StatusBar,
            // Navigate
            TextSearch,
            DatabaseObjectSearch,
            NextEmmetEditPoint,
            PreviousEmmetEditPoint,
            LastEditLocation,
            NextEditLocation,
            NextMethod,
            PreviousMethod,
            MoveCaretToMatchingBrace,
            NextLiveTemplateParameter,
            PreviousLiveTemplateParameter,
            SelectIn,
            JumpToNavigationBar,
            SuperMethod,
            RelatedSymbol,
            TypeHierarchy,
            MethodHierarchy,
            CallHierarchy,
            // Code
            OverrideMethods,
            ImplementMethods,
            GenerateCode,
            SurroundWith,
            UnwrapRemove,
            CodeCompletionBasic,
            CodeCompletionTypeMatching,
            CodeCompletionCompleteStatement,
            CyclicExpandWord,
            CyclicExpandWordBackward,
            CallInlineCompletion,
            InsertInlineProposal,
            EnableCommandCompletion,
            SilentCodeCleanup,
            RunInspectionByName,
            ConfigureCurrentFileAnalysis,
            ViewOfflineInspectionResults,
            LocateDuplicates,
            DataFlowToHere,
            DataFlowFromHere,
            VulnerableDependencies,
            AnalyzeStackTraceOrThreadDump,
            DetectPsr0NamespaceRoots,
            InsertLiveTemplate,
            SaveAsLiveTemplate,
            ExpandRecursively,
            CollapseRecursively,
            ExpandToLevel,
            ExpandAllToLevel,
            ExpandDocComments,
            CollapseDocComments,
            ToggleFolding,
            FoldSelectionRemoveRegion,
            FoldCodeBlock,
            ReformatFile,
            AutoIndentLines,
            RearrangeCode,
            MoveElementLeft,
            MoveElementRight,
            UpdateCopyright,
            // Refactor
            ChangeSignature,
            ExtractMethod,
            ExtractVariable,
            ExtractConstant,
            ExtractField,
            ExtractParameter,
            MoveRefactoring,
            CopyFile,
            SafeDelete,
            // Run
            RunWithCoverage,
            RunWithParallel,
            RunWithMutation,
            RunWithProfiler,
            ManageTargets,
            StopBackgroundProcesses,
            ShowRunningList,
            CompileAndReloadModifiedFiles,
            ForceStepOver,
            ForceStepInto,
            SmartStepInto,
            ForceRunToCursor,
            ResetFrame,
            EvaluateExpression,
            ShowExecutionPoint,
            TestHistory,
            ImportTestsFromFile,
            ManageCoverageReports,
            ResumeProgram,
            StartListeningForPhpDebugConnections,
            BreakAtFirstLineInPhpScripts,
            WebServerDebugValidation,
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
            ServicesDocker,
            ServicesDevContainers,
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
            ComposerInit,
            ComposerSelfUpdate,
            ComposerClearCache,
            ComposerShowLog,
            ComposerInstall,
            ComposerUpdate,
            ComposerManageDependencies,
            ComposerValidate,
            ComposerDumpAutoloader,
            ComposerSimulateUpdate,
            ComposerCheckForChanges,
            ComposerListLicenses,
            ComposerDiagnose,
            RunCommand,
            RunGulpGruntNpmTask,
            QodanaTryAnalysis,
            QodanaAddToCi,
            QodanaLogIn,
            QodanaOpenLocalReport,
            // Git
            Merge,
            NewWorktree,
            Worktrees,
            CreatePatchFromLocalChanges,
            ApplyPatch,
            ApplyPatchFromClipboard,
            ShowShelf,
            ShowGitStash,
            Rollback,
            ShowLocalChangesAsUml,
            AnnotateWithGitBlame,
            CompareWithRevision,
            CompareWithBranchOrTag,
            GitLabCreateMergeRequest,
            GitLabViewMergeRequests,
            GitLabCreateSnippet,
            GitLabShareProject,
            GitLabCloneRepo,
            GitLabManageAccounts,
            GitHubCreatePullRequest,
            GitHubViewPullRequests,
            GitHubCreateGist,
            GitHubShareProject,
            GitHubCloneRepo,
            GitHubManageAccounts,
            ManageRemotes,
            VcsOperationsPopup,
            // Window
            NotificationsWindow,
            ProcessesWindow,
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
                MenuItem::action("New Project from Existing Files…", phpstorm_menu::NewProjectFromExistingFiles),
                MenuItem::submenu(Menu::new("Recent Projects").items([
                    MenuItem::action("Manage Projects…", zed_actions::OpenRecent::default()),
                ])),
                MenuItem::action("Close Project", workspace::CloseWindow),
                MenuItem::action("Close All Projects", workspace::CloseAllItemsAndPanes::default()),
                MenuItem::action("Close Other Projects", phpstorm_menu::CloseOtherProjects),
                MenuItem::action("Rename Project…", project_panel::Rename),
                MenuItem::separator(),
                MenuItem::action("Connect to Remote…", php_panel::OpenDeploymentDialog),
                MenuItem::separator(),
                MenuItem::action("Plugins…", zed_actions::Extensions::default()),
                MenuItem::submenu(Menu::new("File Properties").items([
                    MenuItem::action("File Encoding…", encoding_selector::Toggle),
                    MenuItem::action("Line Separators…", line_ending_selector::Toggle),
                    MenuItem::action("Make File Read-Only", phpstorm_menu::MakeFileReadOnly),
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
                MenuItem::action("Save File as Template…", phpstorm_menu::SaveFileAsTemplate),
                MenuItem::separator(),
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
                MenuItem::action("Copy as Plain Text", phpstorm_menu::CopyAsPlainText),
                MenuItem::action("Copy Path/Reference…", zed_actions::workspace::CopyPath),
                MenuItem::os_action("Paste", editor_actions::Paste, OsAction::Paste),
                MenuItem::action("Delete", editor_actions::Delete),
                MenuItem::separator(),
                MenuItem::action("Search In Selection", phpstorm_menu::SearchInSelection),
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
                MenuItem::action("Fill Paragraph", phpstorm_menu::FillParagraph),
                MenuItem::action("Sort Lines", editor_actions::SortLinesCaseInsensitive),
                MenuItem::action("Reverse Lines", editor_actions::ReverseLines),
                MenuItem::action("Transpose", phpstorm_menu::Transpose),
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
                MenuItem::action("Encode XML/HTML Special Characters", phpstorm_menu::EncodeXmlHtmlSpecialCharacters),
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
                        MenuItem::action("Bookmarks", editor_actions::ViewBookmarks),
                        MenuItem::action("Database", php_panel::ToggleFocus),
                        MenuItem::action("File Transfer", php_panel::BrowseRemoteHost),
                        MenuItem::action("Remote Host", php_panel::BrowseRemoteHost),
                        MenuItem::action("HTTP Client", php_panel::OpenHttpClient),
                        MenuItem::action("Terminal (⌥F12)", terminal_panel::Toggle),
                    ])),
                    MenuItem::submenu(Menu::new("Appearance").items([
                        MenuItem::action("Enter Presentation Mode", phpstorm_menu::EnterPresentationMode),
                        MenuItem::action("Enter Distraction Free Mode", super::ToggleFullScreen),
                        MenuItem::action("Enter Full Screen", super::ToggleFullScreen),
                        MenuItem::action("Enter Zen Mode", phpstorm_menu::EnterZenMode),
                        MenuItem::separator(),
                        MenuItem::action("Compact Mode", phpstorm_menu::CompactMode),
                        MenuItem::action("Zoom IDE (Current: 100%)…", phpstorm_menu::ZoomIde),
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
                        MenuItem::separator(),
                        MenuItem::action("Toolbar", workspace::ToggleAllDocks),
                        MenuItem::action("Navigation Bar", phpstorm_menu::NavigationBar),
                        MenuItem::action("Tool Window Bars", workspace::ToggleAllDocks),
                        MenuItem::action("Status Bar", phpstorm_menu::StatusBar),
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
                    MenuItem::action("Open in Right Split", workspace::SplitRight::default()),
                    MenuItem::action("Show Source", phpstorm_menu::ShowSource),
                    MenuItem::separator(),
                    MenuItem::action("Recent Locations", phpstorm_menu::RecentLocations),
                    MenuItem::action("Recent Files", workspace::ToggleFileFinder::default()),
                    MenuItem::action("Recently Changed Files", phpstorm_menu::RecentlyChangedFiles),
                    MenuItem::action("Recent Changes", phpstorm_menu::RecentChanges),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Compare With…",
                        git_ui::project_diff::CompareWithBranch,
                    ),
                    MenuItem::action("Compare File with Editor", git_ui::project_diff::CompareWithBranch),
                    MenuItem::action(
                        "Compare with Clipboard",
                        editor_actions::DiffClipboardWithSelection,
                    ),
                    MenuItem::separator(),
                    MenuItem::action("Quick Switch Scheme…", phpstorm_menu::QuickSwitchScheme),
                    MenuItem::separator(),
                    MenuItem::action("Reset Font Size in All Editors", zed_actions::ResetBufferFontSize { persist: true }),
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
                MenuItem::action("Database Object…", phpstorm_menu::DatabaseObjectSearch),
                MenuItem::separator(),
                MenuItem::action("Next Highlighted Error", editor_actions::GoToDiagnostic::default()),
                MenuItem::action(
                    "Previous Highlighted Error",
                    editor_actions::GoToPreviousDiagnostic::default(),
                ),
                MenuItem::separator(),
                MenuItem::action("Next Emmet Edit Point", phpstorm_menu::NextEmmetEditPoint),
                MenuItem::action("Previous Emmet Edit Point", phpstorm_menu::PreviousEmmetEditPoint),
                MenuItem::action("Last Edit Location", phpstorm_menu::LastEditLocation),
                MenuItem::action("Next Edit Location", phpstorm_menu::NextEditLocation),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Navigate in File").items([
                    MenuItem::action("Next Method", phpstorm_menu::NextMethod),
                    MenuItem::action("Previous Method", phpstorm_menu::PreviousMethod),
                    MenuItem::action("Move Caret to Matching Brace", editor_actions::MoveToEnclosingBracket),
                    MenuItem::separator(),
                    MenuItem::action("Next Live Template Parameter", phpstorm_menu::NextLiveTemplateParameter),
                    MenuItem::action("Previous Live Template Parameter", phpstorm_menu::PreviousLiveTemplateParameter),
                ])),
                MenuItem::separator(),
                MenuItem::action("Select In…", phpstorm_menu::SelectIn),
                MenuItem::action("Jump to Navigation Bar", phpstorm_menu::JumpToNavigationBar),
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
                MenuItem::action("Super Method", phpstorm_menu::SuperMethod),
                MenuItem::action("Related Symbol…", phpstorm_menu::RelatedSymbol),
                MenuItem::separator(),
                MenuItem::action("File Structure", zed_actions::outline::ToggleOutline),
                MenuItem::action("File Path", zed_actions::workspace::CopyPath),
                MenuItem::action("Type Hierarchy", phpstorm_menu::TypeHierarchy),
                MenuItem::action("Method Hierarchy", phpstorm_menu::MethodHierarchy),
                MenuItem::action("Call Hierarchy", phpstorm_menu::CallHierarchy),
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
                MenuItem::submenu(Menu::new("Code Completion").items([
                    MenuItem::action("Basic", editor_actions::ShowCompletions),
                    MenuItem::action("Type-Matching", phpstorm_menu::CodeCompletionTypeMatching),
                    MenuItem::separator(),
                    MenuItem::action("Complete Current Statement", phpstorm_menu::CodeCompletionCompleteStatement),
                    MenuItem::separator(),
                    MenuItem::action("Cyclic Expand Word", phpstorm_menu::CyclicExpandWord),
                    MenuItem::action("Cyclic Expand Word (Backward)", phpstorm_menu::CyclicExpandWordBackward),
                    MenuItem::separator(),
                    MenuItem::action("Call Inline Completion", phpstorm_menu::CallInlineCompletion),
                    MenuItem::action("Insert Inline Proposal", phpstorm_menu::InsertInlineProposal),
                    MenuItem::action("Enable Command Completion", phpstorm_menu::EnableCommandCompletion),
                ])),
                MenuItem::separator(),
                MenuItem::action("Inspect Code…", diagnostics::Deploy),
                MenuItem::action("Code Cleanup…", editor_actions::Format),
                MenuItem::submenu(Menu::new("Analyze Code").items([
                    MenuItem::action("Silent Code Cleanup", phpstorm_menu::SilentCodeCleanup),
                    MenuItem::action("Run Inspection by Name…", phpstorm_menu::RunInspectionByName),
                    MenuItem::action("Configure Current File Analysis…", phpstorm_menu::ConfigureCurrentFileAnalysis),
                    MenuItem::action("View Offline Inspection Results…", phpstorm_menu::ViewOfflineInspectionResults),
                    MenuItem::action("Locate Duplicates…", phpstorm_menu::LocateDuplicates),
                    MenuItem::separator(),
                    MenuItem::action("Data Flow to Here…", phpstorm_menu::DataFlowToHere),
                    MenuItem::action("Data Flow from Here…", phpstorm_menu::DataFlowFromHere),
                    MenuItem::separator(),
                    MenuItem::action("Vulnerable Dependencies…", phpstorm_menu::VulnerableDependencies),
                ])),
                MenuItem::action("Analyze Stack Trace or Thread Dump…", phpstorm_menu::AnalyzeStackTraceOrThreadDump),
                MenuItem::action("Detect PSR-0 Namespace Roots", phpstorm_menu::DetectPsr0NamespaceRoots),
                MenuItem::separator(),
                MenuItem::action("Insert Live Template…", phpstorm_menu::InsertLiveTemplate),
                MenuItem::action("Save as Live Template…", phpstorm_menu::SaveAsLiveTemplate),
                MenuItem::separator(),
                MenuItem::action("Surround With…", phpstorm_menu::SurroundWith),
                MenuItem::action("Unwrap/Remove…", phpstorm_menu::UnwrapRemove),
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Folding").items([
                    MenuItem::action("Expand", editor_actions::UnfoldLines),
                    MenuItem::action("Expand Recursively", phpstorm_menu::ExpandRecursively),
                    MenuItem::action("Expand All", editor_actions::UnfoldAll),
                    MenuItem::separator(),
                    MenuItem::action("Collapse", editor_actions::Fold),
                    MenuItem::action("Collapse Recursively", phpstorm_menu::CollapseRecursively),
                    MenuItem::action("Collapse All", editor_actions::FoldAll),
                    MenuItem::separator(),
                    MenuItem::action("Expand to Level", phpstorm_menu::ExpandToLevel),
                    MenuItem::action("Expand All to Level", phpstorm_menu::ExpandAllToLevel),
                    MenuItem::separator(),
                    MenuItem::action("Expand Doc Comments", phpstorm_menu::ExpandDocComments),
                    MenuItem::action("Collapse Doc Comments", phpstorm_menu::CollapseDocComments),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Folding", editor_actions::Fold),
                    MenuItem::separator(),
                    MenuItem::action("Fold Selection / Remove Region", phpstorm_menu::FoldSelectionRemoveRegion),
                    MenuItem::action("Fold Code Block", phpstorm_menu::FoldCodeBlock),
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
                MenuItem::action("Reformat File…", phpstorm_menu::ReformatFile),
                MenuItem::action("Auto-Indent Lines", phpstorm_menu::AutoIndentLines),
                MenuItem::action("Optimize Imports", editor_actions::OrganizeImports),
                MenuItem::action("Rearrange Code", phpstorm_menu::RearrangeCode),
                MenuItem::separator(),
                MenuItem::action("Move Statement Down", editor_actions::MoveLineDown),
                MenuItem::action("Move Statement Up", editor_actions::MoveLineUp),
                MenuItem::action("Move Element Left", phpstorm_menu::MoveElementLeft),
                MenuItem::action("Move Element Right", phpstorm_menu::MoveElementRight),
                MenuItem::action("Move Line Down", editor_actions::MoveLineDown),
                MenuItem::action("Move Line Up", editor_actions::MoveLineUp),
                MenuItem::separator(),
                MenuItem::action("Update Copyright…", phpstorm_menu::UpdateCopyright),
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
                MenuItem::separator(),
                MenuItem::submenu(Menu::new("Extract/Introduce").items([
                    MenuItem::action("Variable…", phpstorm_menu::ExtractVariable),
                    MenuItem::action("Constant…", phpstorm_menu::ExtractConstant),
                    MenuItem::action("Field…", phpstorm_menu::ExtractField),
                    MenuItem::action("Parameter…", phpstorm_menu::ExtractParameter),
                    MenuItem::action("Method…", phpstorm_menu::ExtractMethod),
                ])),
                MenuItem::separator(),
                MenuItem::action("Move…", phpstorm_menu::MoveRefactoring),
                MenuItem::action("Copy File…", phpstorm_menu::CopyFile),
                MenuItem::action("Safe Delete…", phpstorm_menu::SafeDelete),
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
                MenuItem::action("Run with Coverage", phpstorm_menu::RunWithCoverage),
                MenuItem::action("Run in Parallel", phpstorm_menu::RunWithParallel),
                MenuItem::action("Run with Mutation", phpstorm_menu::RunWithMutation),
                MenuItem::action("Run with Profiler", phpstorm_menu::RunWithProfiler),
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
                    MenuItem::action("Force Step Over", phpstorm_menu::ForceStepOver),
                    MenuItem::action("Step Into", debugger_ui::StepInto),
                    MenuItem::action("Force Step Into", phpstorm_menu::ForceStepInto),
                    MenuItem::action("Smart Step Into", phpstorm_menu::SmartStepInto),
                    MenuItem::action("Step Out", debugger_ui::StepOut),
                    MenuItem::action("Run to Cursor", phpstorm_menu::ForceRunToCursor),
                    MenuItem::action("Reset Frame", phpstorm_menu::ResetFrame),
                    MenuItem::action("Resume Program", phpstorm_menu::ResumeProgram),
                    MenuItem::separator(),
                    MenuItem::action("Evaluate Expression…", phpstorm_menu::EvaluateExpression),
                    MenuItem::action("Show Execution Point", phpstorm_menu::ShowExecutionPoint),
                ])),
                MenuItem::action("Toggle Breakpoint", editor_actions::ToggleBreakpoint),
                MenuItem::action("View Breakpoints…", editor_actions::EditLogBreakpoint),
                MenuItem::separator(),
                MenuItem::action("Test History", phpstorm_menu::TestHistory),
                MenuItem::action("Import Tests from File…", phpstorm_menu::ImportTestsFromFile),
                MenuItem::action("Manage Coverage Reports…", phpstorm_menu::ManageCoverageReports),
                MenuItem::separator(),
                MenuItem::action(
                    "Start Listening for PHP Debug Connections",
                    phpstorm_menu::StartListeningForPhpDebugConnections,
                ),
                MenuItem::action("Break at first line in PHP scripts", phpstorm_menu::BreakAtFirstLineInPhpScripts),
                MenuItem::action("Web Server Debug Validation", phpstorm_menu::WebServerDebugValidation),
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
                    MenuItem::action("Docker", phpstorm_menu::ServicesDocker),
                    MenuItem::action("Dev Containers", phpstorm_menu::ServicesDevContainers),
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
                MenuItem::action("Merge…", phpstorm_menu::Merge),
                MenuItem::action("Rebase…", git::PullRebase),
                MenuItem::separator(),
                MenuItem::action("Branches…", git_picker::ActivateBranchesTab),
                MenuItem::action("New Branch…", zed_actions::git::Branch),
                MenuItem::action("New Tag…", git::CreateTagAtHead),
                MenuItem::action("Reset HEAD…", git::RestoreTrackedFiles),
                MenuItem::separator(),
                MenuItem::action("New Worktree…", phpstorm_menu::NewWorktree),
                MenuItem::action("Worktrees…", phpstorm_menu::Worktrees),
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
                    MenuItem::action("Show Git Stash", phpstorm_menu::ShowGitStash),
                    MenuItem::action("Stash Changes…", git::StashAll),
                    MenuItem::action("Unstash Changes…", git::StashPop),
                    MenuItem::action("Rollback…", phpstorm_menu::Rollback),
                    MenuItem::action("Show Local Changes as UML", phpstorm_menu::ShowLocalChangesAsUml),
                ])),
                MenuItem::submenu(Menu::new("Selected File").items([
                    MenuItem::action("Commit File…", git_panel::ToggleFocus),
                    MenuItem::action("Add", git::StageFile),
                    MenuItem::separator(),
                    MenuItem::action("Annotate with Git Blame", phpstorm_menu::AnnotateWithGitBlame),
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
                MenuItem::action("Manage Remotes…", phpstorm_menu::ManageRemotes),
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
                    MenuItem::action("Custom", workspace::ToggleAllDocks),
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
                    MenuItem::action("Split Right", workspace::SplitRight::default()),
                    MenuItem::action("Split Down", workspace::SplitDown::default()),
                    MenuItem::separator(),
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
                MenuItem::action("Notifications", phpstorm_menu::NotificationsWindow),
                MenuItem::action("Processes", phpstorm_menu::ProcessesWindow),
                MenuItem::separator(),
                MenuItem::action(
                    "Next Project Window",
                    workspace::ActivateNextItem { wrap_around: true },
                ),
                MenuItem::action(
                    "Previous Project Window",
                    workspace::ActivatePreviousItem { wrap_around: true },
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
