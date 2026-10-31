mod components;
mod extension_suggestions;
mod extension_version_selector;
mod plugin_manager;

use std::sync::OnceLock;
use std::{any::TypeId, ops::Range, sync::Arc};

use cloud_api_types::{ExtensionMetadata, ExtensionProvides};
use collections::{BTreeMap, BTreeSet};
use command_palette_hooks::CommandPaletteFilter;
use editor::{Editor, EditorElement, EditorStyle};
use extension_host::{ExtensionIndexEntry, ExtensionManifest, ExtensionStatus, ExtensionStore};
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use git::{GitHostingProviderRegistry, parse_git_remote_url};
use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, Focusable,
    FontWeight, InteractiveElement, KeyContext, ParentElement, Render, Styled, Task, TaskExt,
    TextStyle, UniformListScrollHandle, WeakEntity, Window, actions, point, uniform_list,
};

use picker::{Picker, PickerDelegate};
use project::DirectoryLister;

use schemars::JsonSchema;
use serde::Deserialize;
use settings::{Settings, SettingsContent};
use theme_settings::ThemeSettings;
use ui::{
    Banner, Chip, CommonAnimationExt, ContextMenu, Divider, ListItem, ListItemSpacing, PopoverMenu,
    ScrollableHandle, Switch, WithScrollbar, prelude::*,
};
use util::ResultExt;
use vim_mode_setting::VimModeSetting;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
    workspace_error::{ErrorAction, ErrorSeverity, WorkspaceError},
};
use zed_actions::ExtensionCategoryFilter;

use crate::components::extension_provides_label;
use crate::extension_version_selector::{
    ExtensionVersionSelector, ExtensionVersionSelectorDelegate,
};
use crate::plugin_manager::{
    Category, PluginEntry, PluginManagerState, matches_category, matches_search, sort_entries,
};

actions!(
    zed,
    [
        /// Installs an extension from a local directory for development.
        InstallDevExtension,
    ]
);

/// Rebuilds an installed dev extension.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, gpui::Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct RebuildDevExtension {
    /// The ID of the dev extension to rebuild.
    ///
    /// Default: opens a picker if multiple dev extensions are installed.
    #[serde(default)]
    pub extension_id: Option<String>,
}

#[derive(Default)]
struct DevExtensionNotInstalledError {
    extension_id: Option<SharedString>,
}

impl WorkspaceError for DevExtensionNotInstalledError {
    fn primary_message(&self) -> SharedString {
        match &self.extension_id {
            Some(extension_id) => {
                format!("Dev extension '{extension_id}' is not installed.").into()
            }
            None => "No dev extensions are installed.".into(),
        }
    }

    fn primary_action(&self) -> ErrorAction {
        ErrorAction::new("Install Dev Extension", InstallDevExtension)
    }

    fn severity(&self) -> ErrorSeverity {
        ErrorSeverity::Warning
    }
}

/// Maps an `ExtensionProvides` tag to the nearest plugins manager category.
fn category_for_provides(provides: ExtensionProvides) -> Category {
    match provides {
        ExtensionProvides::Themes | ExtensionProvides::IconThemes => Category::Themes,
        ExtensionProvides::Languages | ExtensionProvides::Grammars => Category::Languages,
        ExtensionProvides::LanguageServers => Category::LanguageServers,
        ExtensionProvides::DebugAdapters => Category::DebugAdapters,
        ExtensionProvides::Snippets => Category::Snippets,
        _ => Category::Marketplace,
    }
}

fn is_extension_busy(status: ExtensionStatus) -> bool {
    matches!(
        status,
        ExtensionStatus::Installing | ExtensionStatus::Upgrading | ExtensionStatus::Removing
    )
}

fn update_rebuild_dev_extension_visibility(store: &Entity<ExtensionStore>, cx: &mut App) {
    let has_dev_extensions = store.read(cx).dev_extensions().next().is_some();
    CommandPaletteFilter::update_global(cx, |filter, _cx| {
        if has_dev_extensions {
            filter.show_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        } else {
            filter.hide_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        }
    });
}

pub fn init(cx: &mut App) {
    let store = ExtensionStore::global(cx);
    update_rebuild_dev_extension_visibility(&store, cx);
    cx.observe(&store, |store, cx| {
        update_rebuild_dev_extension_visibility(&store, cx);
    })
    .detach();
    extension_suggestions::init(cx);

    cx.observe_new(move |workspace: &mut Workspace, window, _cx| {
        if window.is_none() {
            return;
        }
        workspace
            .register_action(
                move |workspace, action: &zed_actions::Extensions, window, cx| {
                    let provides_filter = action.category_filter.map(|category| match category {
                        ExtensionCategoryFilter::Themes => ExtensionProvides::Themes,
                        ExtensionCategoryFilter::IconThemes => ExtensionProvides::IconThemes,
                        ExtensionCategoryFilter::Languages => ExtensionProvides::Languages,
                        ExtensionCategoryFilter::Grammars => ExtensionProvides::Grammars,
                        ExtensionCategoryFilter::LanguageServers => {
                            ExtensionProvides::LanguageServers
                        }
                        ExtensionCategoryFilter::ContextServers => {
                            ExtensionProvides::ContextServers
                        }
                        ExtensionCategoryFilter::Snippets => ExtensionProvides::Snippets,
                        ExtensionCategoryFilter::DebugAdapters => ExtensionProvides::DebugAdapters,
                    });

                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<ExtensionsPage>());

                    if let Some(existing) = existing {
                        existing.update(cx, |extensions_page, cx| {
                            if provides_filter.is_some() {
                                extensions_page.change_provides_filter(provides_filter, cx);
                            }
                            if let Some(id) = action.id.as_ref() {
                                extensions_page.focus_extension(id, window, cx);
                            }
                        });

                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let extensions_page = ExtensionsPage::new(
                            workspace,
                            provides_filter,
                            action.id.as_deref(),
                            window,
                            cx,
                        );
                        workspace.add_item_to_active_pane(
                            Box::new(extensions_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                },
            )
            .register_action(move |workspace, _: &InstallDevExtension, window, cx| {
                let store = ExtensionStore::global(cx);
                let prompt = workspace.prompt_for_open_path(
                    gpui::PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: None,
                    },
                    DirectoryLister::Local(
                        workspace.project().clone(),
                        workspace.app_state().fs.clone(),
                    ),
                    window,
                    cx,
                );

                let workspace_handle = cx.entity().downgrade();
                window
                    .spawn(cx, async move |cx| {
                        let extension_path = match prompt.await.map_err(anyhow::Error::from) {
                            Ok(Some(mut paths)) => paths.pop()?,
                            Ok(None) => return None,
                            Err(err) => {
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        workspace.show_error(
                                            workspace::workspace_error::PortalError::new(
                                                err.to_string(),
                                            ),
                                            cx,
                                        );
                                    })
                                    .ok();
                                return None;
                            }
                        };

                        let install_task = store.update(cx, |store, cx| {
                            store.install_dev_extension(extension_path, cx)
                        });

                        match install_task.await {
                            Ok(_) => {}
                            Err(err) => {
                                log::error!("Failed to install dev extension: {:?}", err);
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        // NOTE: using `anyhow::context` here ends up not printing
                                        // the error
                                        workspace.show_error(
                                            format!("Failed to install dev extension: {}", err),
                                            cx,
                                        );
                                    })
                                    .ok();
                            }
                        }

                        Some(())
                    })
                    .detach();
            })
            .register_action(move |workspace, action: &RebuildDevExtension, window, cx| {
                if let Some(target_id) = action.extension_id.as_deref() {
                    let extension_id = ExtensionStore::global(cx)
                        .read(cx)
                        .dev_extensions()
                        .find_map(|m| {
                            if m.id.as_ref() == target_id {
                                Some(m.id.clone())
                            } else {
                                None
                            }
                        });
                    if let Some(extension_id) = extension_id {
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    } else {
                        workspace.show_error(
                            DevExtensionNotInstalledError {
                                extension_id: Some(SharedString::from(target_id.to_owned())),
                            },
                            cx,
                        );
                    }
                    return;
                }

                let dev_extensions = ExtensionStore::global(cx)
                    .read(cx)
                    .dev_extensions()
                    .cloned()
                    .collect::<Vec<_>>();

                match dev_extensions.len() {
                    0 => {
                        workspace.show_error(DevExtensionNotInstalledError::default(), cx);
                    }
                    1 => {
                        let extension_id = dev_extensions[0].id.clone();
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    }
                    _ => {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            let delegate = DevExtensionRebuildPickerDelegate::new(dev_extensions);
                            Picker::uniform_list(delegate, window, cx)
                        });
                    }
                }
            });
    })
    .detach();
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum Feature {
    AgentClaude,
    AgentCodex,
    AgentGemini,
    ExtensionBasedpyright,
    ExtensionRuff,
    ExtensionTailwind,
    ExtensionTy,
    Git,
    LanguageBash,
    LanguageC,
    LanguageCpp,
    LanguageGo,
    LanguagePython,
    LanguageReact,
    LanguageRust,
    LanguageTypescript,
    OpenIn,
    Vim,
}

fn keywords_by_feature() -> &'static BTreeMap<Feature, Vec<&'static str>> {
    static KEYWORDS_BY_FEATURE: OnceLock<BTreeMap<Feature, Vec<&'static str>>> = OnceLock::new();
    KEYWORDS_BY_FEATURE.get_or_init(|| {
        BTreeMap::from_iter([
            (
                Feature::AgentClaude,
                vec!["claude", "claude code", "claude agent"],
            ),
            (Feature::AgentCodex, vec!["codex", "codex cli"]),
            (Feature::AgentGemini, vec!["gemini", "gemini cli"]),
            (
                Feature::ExtensionBasedpyright,
                vec!["basedpyright", "pyright"],
            ),
            (Feature::ExtensionRuff, vec!["ruff"]),
            (Feature::ExtensionTailwind, vec!["tail", "tailwind"]),
            (Feature::ExtensionTy, vec!["ty"]),
            (Feature::Git, vec!["git"]),
            (Feature::LanguageBash, vec!["sh", "bash"]),
            (Feature::LanguageC, vec!["c", "clang"]),
            (Feature::LanguageCpp, vec!["c++", "cpp", "clang"]),
            (Feature::LanguageGo, vec!["go", "golang"]),
            (Feature::LanguagePython, vec!["python", "py"]),
            (Feature::LanguageReact, vec!["react"]),
            (Feature::LanguageRust, vec!["rust", "rs"]),
            (
                Feature::LanguageTypescript,
                vec!["type", "typescript", "ts"],
            ),
            (
                Feature::OpenIn,
                vec![
                    "github",
                    "gitlab",
                    "bitbucket",
                    "codeberg",
                    "sourcehut",
                    "permalink",
                    "link",
                    "open in",
                ],
            ),
            (Feature::Vim, vec!["vim"]),
        ])
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtensionFetchState {
    Fetching,
    Succeeded,
    Failed,
}

pub struct ExtensionsPage {
    workspace: WeakEntity<Workspace>,
    extension_store: Entity<ExtensionStore>,
    provider_registry: Arc<GitHostingProviderRegistry>,
    list: UniformListScrollHandle,
    fetch_state: ExtensionFetchState,
    /// All installed extensions, sorted by name.
    installed_search_results: Vec<ExtensionIndexEntry>,
    /// Results of the latest fetch, empty if it failed.
    remote_extensions: Vec<ExtensionMetadata>,
    /// Rows currently visible in the plugins manager.
    entries: Vec<PluginEntry>,
    query_editor: Entity<Editor>,
    query_contains_error: bool,
    plugin_manager: PluginManagerState,
    _subscriptions: [gpui::Subscription; 2],
    extension_fetch_task: Option<Task<()>>,
    upsells: BTreeSet<Feature>,
}

impl ExtensionsPage {
    pub fn new(
        workspace: &Workspace,
        provides_filter: Option<ExtensionProvides>,
        focus_extension_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let extension_store = ExtensionStore::global(cx);
            let workspace_handle = workspace.weak_handle();
            let subscriptions = [
                cx.observe(&extension_store, |_: &mut Self, _, cx| cx.notify()),
                cx.subscribe_in(
                    &extension_store,
                    window,
                    move |this, _, event, window, cx| match event {
                        extension_host::Event::ExtensionsUpdated => {
                            this.update_local_search_results(cx)
                        }
                        extension_host::Event::ExtensionInstalled(extension_id) => this
                            .on_extension_installed(
                                workspace_handle.clone(),
                                extension_id,
                                window,
                                cx,
                            ),
                        _ => {}
                    },
                ),
            ];

            let query_editor = cx.new(|cx| {
                let mut input = Editor::single_line(window, cx);
                input.set_placeholder_text("Search extensions...", window, cx);
                if let Some(id) = focus_extension_id {
                    input.set_text(format!("id:{id}"), window, cx);
                }
                input
            });
            cx.subscribe(&query_editor, Self::on_query_change).detach();

            let scroll_handle = UniformListScrollHandle::new();
            let provider_registry = GitHostingProviderRegistry::default_global(cx);

            let mut plugin_manager = PluginManagerState::new();
            if let Some(provides_filter) = provides_filter {
                plugin_manager.select_category(category_for_provides(provides_filter));
            }
            if let Some(extension_id) = focus_extension_id {
                let is_installed = extension_store
                    .read(cx)
                    .installed_extensions()
                    .contains_key(extension_id);
                plugin_manager.select_category(if is_installed {
                    Category::Installed
                } else {
                    Category::Marketplace
                });
                plugin_manager.select(Arc::<str>::from(extension_id));
            }

            let mut this = Self {
                workspace: workspace.weak_handle(),
                extension_store,
                provider_registry,
                list: scroll_handle,
                fetch_state: ExtensionFetchState::Fetching,
                installed_search_results: Vec::new(),
                remote_extensions: Vec::new(),
                entries: Vec::new(),
                query_contains_error: false,
                plugin_manager,
                extension_fetch_task: None,
                _subscriptions: subscriptions,
                query_editor,
                upsells: BTreeSet::default(),
            };
            this.update_local_search_results(cx);
            this.fetch_extensions(cx);
            this
        })
    }

    fn get_repository_icon(&self, repository_url: &str) -> IconName {
        parse_git_remote_url(Arc::clone(&self.provider_registry), repository_url)
            .map(|(provider, _)| ui::git_hosting_provider_icon(provider.name().as_str()))
            .unwrap_or(IconName::Link)
    }

    fn on_extension_installed(
        &mut self,
        workspace: WeakEntity<Workspace>,
        extension_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let extension_store = self.extension_store.read(cx);
        let themes = extension_store
            .extension_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::theme_selector::Toggle {
                            themes_filter: Some(themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
            return;
        }

        let icon_themes = extension_store
            .extension_icon_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !icon_themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::icon_theme_selector::Toggle {
                            themes_filter: Some(icon_themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
        }
    }

    /// Collects all installed extensions (sorted by name) and refreshes the
    /// visible rows. Search and category filtering are applied locally.
    fn update_local_search_results(&mut self, cx: &mut Context<Self>) {
        let mut installed_extensions = self
            .extension_store
            .read(cx)
            .installed_extensions()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        installed_extensions
            .sort_by_cached_key(|extension| extension.manifest.name.to_lowercase());
        self.installed_search_results = installed_extensions;
        self.rebuild_entries(cx);
    }

    /// Rebuilds `entries` from the installed and remote extensions, applying
    /// the selected category, the search query, and the installed-first sort.
    fn rebuild_entries(&mut self, cx: &mut Context<Self>) {
        let query = self.search_query(cx);
        let category = self.plugin_manager.category;
        let installed_extensions = self.extension_store.read(cx).installed_extensions();

        let mut entries = Vec::new();

        for installed in &self.installed_search_results {
            entries.push(PluginEntry {
                id: installed.manifest.id.clone(),
                name: installed.manifest.name.clone(),
                version: installed.manifest.version.to_string(),
                description: installed.manifest.description.clone(),
                authors: installed.manifest.authors.join(", "),
                repository_url: installed.manifest.repository.clone(),
                provides: installed.manifest.provides(),
                installed: true,
                dev: installed.dev,
                downloads: None,
            });
        }

        for remote in &self.remote_extensions {
            if installed_extensions.contains_key(&remote.id) {
                continue;
            }
            entries.push(PluginEntry {
                id: remote.id.clone(),
                name: remote.manifest.name.clone(),
                version: remote.manifest.version.to_string(),
                description: remote.manifest.description.clone(),
                authors: remote.manifest.authors.join(", "),
                repository_url: Some(remote.manifest.repository.clone()),
                provides: remote.manifest.provides.clone(),
                installed: false,
                dev: false,
                downloads: Some(remote.download_count),
            });
        }

        entries.retain(|entry| matches_category(category, entry.installed, &entry.provides));

        if let Some(query) = query.as_deref() {
            if let Some(extension_id) = query.strip_prefix("id:") {
                entries.retain(|entry| entry.id.as_ref() == extension_id);
            } else {
                entries.retain(|entry| {
                    matches_search(query, &entry.name, entry.description.as_deref())
                });
            }
        }

        sort_entries(&mut entries);
        self.entries = entries;
        self.plugin_manager.retain_selection(&self.entries);
        cx.notify();
    }

    fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.list.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Fetches every remote extension once. Category and search filtering are
    /// applied locally so switching categories never triggers a refetch.
    fn fetch_extensions(&mut self, cx: &mut Context<Self>) {
        self.fetch_state = ExtensionFetchState::Fetching;
        cx.notify();

        self.extension_fetch_task = Some(cx.spawn(async move |this, cx| {
            let Ok(fetch_task) = this.update(cx, |this, cx| {
                this.extension_store
                    .update(cx, |store, cx| store.fetch_extensions(None, None, cx))
            }) else {
                return;
            };
            let fetch_result = fetch_task.await;

            this.update(cx, |this, cx| {
                match fetch_result {
                    Ok(remote_extensions) => {
                        this.fetch_state = ExtensionFetchState::Succeeded;
                        this.remote_extensions = remote_extensions;
                    }
                    Err(error) => {
                        log::error!("failed to fetch extensions: {error:#}");
                        this.fetch_state = ExtensionFetchState::Failed;
                        this.remote_extensions.clear();
                    }
                }
                this.rebuild_entries(cx);
            })
            .ok();
        }));
    }

    fn render_entries(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        range
            .filter_map(|index| {
                let entry = self.entries.get(index)?.clone();
                Some(self.render_entry(entry, cx))
            })
            .collect()
    }

    fn render_entry(&self, entry: PluginEntry, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let selected = self.plugin_manager.selected.as_deref() == Some(entry.id.as_ref());
        let busy = is_extension_busy(self.extension_store.read(cx).extension_status(&entry.id));
        let initial = entry
            .name
            .chars()
            .next()
            .map(|character| character.to_uppercase().to_string())
            .unwrap_or_default();
        let row_id = SharedString::from(format!("plugin-row-{}", entry.id));
        let click_id = entry.id.clone();

        h_flex()
            .id(row_id)
            .h(rems_from_px(64_f32))
            .w_full()
            .px_3()
            .gap_3()
            .items_center()
            .cursor_pointer()
            .border_b_1()
            .border_color(colors.border_variant)
            .when(selected, |this| this.bg(colors.element_selected))
            .hover(|style| style.bg(colors.ghost_element_hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.plugin_manager.select(click_id.clone());
                cx.notify();
            }))
            .child(
                div()
                    .size(rems_from_px(36_f32))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border_variant)
                    .bg(colors.element_background)
                    .child(
                        Label::new(initial)
                            .size(LabelSize::Default)
                            .weight(FontWeight::SEMIBOLD)
                            .color(if selected {
                                Color::Accent
                            } else {
                                Color::Muted
                            }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Label::new(entry.name.clone())
                                    .size(LabelSize::Default)
                                    .weight(FontWeight::SEMIBOLD)
                                    .truncate(),
                            )
                            .child(
                                Label::new(format!("v{}", entry.version))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        Label::new(
                            entry
                                .description
                                .clone()
                                .unwrap_or_else(|| "No description".to_string()),
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                    ),
            )
            .child(self.render_entry_actions(&entry, busy, cx))
            .into_any_element()
    }

    fn render_entry_actions(
        &self,
        entry: &PluginEntry,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = entry.id.clone();
        let action = if entry.installed {
            Button::new(
                SharedString::from(format!("uninstall-plugin-{}", id)),
                "Uninstall",
            )
            .style(ButtonStyle::OutlinedGhost)
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    telemetry::event!("Extension Uninstalled", id);
                    ExtensionStore::global(cx).update(cx, |store, cx| {
                        store
                            .uninstall_extension(id.clone(), cx)
                            .detach_and_log_err(cx);
                    });
                })
                .into_any_element()
        } else {
            Button::new(SharedString::from(format!("install-plugin-{}", id)), "Install")
                .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                .start_icon(
                    Icon::new(IconName::Download)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    telemetry::event!("Extension Installed");
                    ExtensionStore::global(cx).update(cx, |store, cx| {
                        store.install_latest_extension(id.clone(), cx)
                    });
                })
                .into_any_element()
        };

        h_flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .when(entry.installed, |this| {
                this.child(Chip::new("Installed").label_color(Color::Muted))
            })
            .child(action)
            .child(self.render_entry_menu(entry, cx))
            .into_any_element()
    }

    fn render_entry_menu(&self, entry: &PluginEntry, cx: &mut Context<Self>) -> AnyElement {
        let id = entry.id.clone();
        let dev = entry.dev;
        let installed = entry.installed;
        let entity = cx.entity();
        let menu_id = SharedString::from(format!("plugin-menu-{}", entry.id));
        let trigger_id = SharedString::from(format!("plugin-menu-trigger-{}", entry.id));

        PopoverMenu::new(menu_id)
            .trigger(
                IconButton::new(trigger_id, IconName::Ellipsis)
                    .icon_size(IconSize::Small)
                    .aria_label("More plugin actions"),
            )
            .menu(move |window, cx| {
                let entity = entity.clone();
                let id = id.clone();
                Some(ContextMenu::build(window, cx, move |menu, window, _| {
                    let mut menu = menu;
                    if dev {
                        let rebuild_id = id.clone();
                        menu = menu.entry("Rebuild", None, move |_, cx| {
                            ExtensionStore::global(cx).update(cx, |store, cx| {
                                store.rebuild_dev_extension(rebuild_id.clone(), cx);
                            });
                        });
                    }
                    if !installed {
                        let version_id = id.clone();
                        let entity = entity.clone();
                        menu = menu.entry(
                            "Install Another Version...",
                            None,
                            window.handler_for(
                                &entity,
                                move |this: &mut ExtensionsPage, window, cx| {
                                    this.show_extension_version_list(version_id.clone(), window, cx);
                                },
                            ),
                        );
                    }
                    let copy_id = id.clone();
                    menu.entry("Copy Extension ID", None, move |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_id.to_string()));
                    })
                }))
            })
            .into_any_element()
    }

    fn render_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        v_flex()
            .w(rems_from_px(180_f32))
            .flex_shrink_0()
            .h_full()
            .px_2()
            .py_3()
            .gap_0p5()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border_variant)
            .child(
                div().px_2().pb_2().child(
                    Label::new("Plugins")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            )
            .children(Category::ALL.into_iter().map(|category| {
                let selected = self.plugin_manager.category == category;
                h_flex()
                    .id(SharedString::from(format!(
                        "plugin-category-{}",
                        category.label()
                    )))
                    .w_full()
                    .h(rems_from_px(30_f32))
                    .px_2()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |this| this.bg(colors.element_selected))
                    .hover(|style| style.bg(colors.ghost_element_hover))
                    .child(
                        Label::new(category.label())
                            .size(LabelSize::Default)
                            .color(if selected {
                                Color::Accent
                            } else {
                                Color::Default
                            })
                            .weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.plugin_manager.select_category(category);
                        this.rebuild_entries(cx);
                        this.scroll_to_top(cx);
                    }))
            }))
    }

    fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let category = self.plugin_manager.category;
        let count = self.entries.len();
        let scroll_handle = &self.list;

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(
                        Label::new(category.label())
                            .size(LabelSize::Default)
                            .weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        Label::new(format!("{count}"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_hidden()
                    .px_3()
                    .map(|this| {
                        if count == 0 {
                            this.child(self.render_empty_state(cx)).into_any_element()
                        } else {
                            this.when(
                                self.fetch_state == ExtensionFetchState::Failed
                                    && category != Category::Installed,
                                |this| {
                                    this.child(
                                        div().pt_3().child(
                                            Banner::new()
                                                .severity(Severity::Warning)
                                                .child(
                                                    Label::new(
                                                        "Failed to load plugins. Showing installed plugins only.",
                                                    )
                                                    .mt_0p5(),
                                                )
                                                .action_slot(self.retry_button(cx)),
                                        ),
                                    )
                                },
                            )
                            .child(
                                uniform_list(
                                    "plugin-entries",
                                    count,
                                    cx.processor(Self::render_entries),
                                )
                                .flex_grow_1()
                                .pb_4()
                                .track_scroll(scroll_handle),
                            )
                            .vertical_scrollbar_for(scroll_handle, window, cx)
                                .into_any_element()
                        }
                    }),
            )
            .into_any_element()
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let has_search = self.search_query(cx).is_some();
        let category = self.plugin_manager.category;
        let fetch_is_relevant = category != Category::Installed;
        let fetch_failure_is_relevant =
            self.fetch_state == ExtensionFetchState::Failed && fetch_is_relevant;
        let is_loading = self.fetch_state == ExtensionFetchState::Fetching && fetch_is_relevant;

        let message: SharedString = if is_loading {
            "Loading plugins…".into()
        } else if fetch_failure_is_relevant {
            "Failed to load plugins. Please check your connection and try again.".into()
        } else if category == Category::Updates {
            "No updates available".into()
        } else if has_search {
            "No plugins match your search.".into()
        } else {
            match category {
                Category::Installed => "No installed plugins.".into(),
                Category::Marketplace => "No plugins available.".into(),
                _ => "No plugins in this category.".into(),
            }
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .pt_8()
            .child(
                h_flex()
                    .gap_1p5()
                    .when(is_loading, |this| {
                        this.child(
                            Icon::new(IconName::LoadCircle)
                                .size(IconSize::Small)
                                .color(Color::Accent)
                                .with_rotate_animation(3),
                        )
                    })
                    .when(fetch_failure_is_relevant, |this| {
                        this.child(
                            Icon::new(IconName::Warning)
                                .size(IconSize::Small)
                                .color(Color::Warning),
                        )
                    })
                    .child(Label::new(message).color(Color::Muted)),
            )
            .when(fetch_failure_is_relevant, |this| {
                this.child(self.retry_button(cx))
            })
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.plugin_manager.selected.as_deref()?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id.as_ref() == selected)?
            .clone();
        let colors = cx.theme().colors();
        let busy = is_extension_busy(self.extension_store.read(cx).extension_status(&entry.id));
        let provided = entry
            .provides
            .iter()
            .filter(|provides| !provides.is_deprecated())
            .map(|provides| extension_provides_label(*provides))
            .collect::<Vec<_>>();

        let mut actions = v_flex().gap_2();
        if entry.installed {
            let uninstall_id = entry.id.clone();
            actions = actions.child(
                Button::new("detail-uninstall", "Uninstall")
                    .style(ButtonStyle::Outlined)
                    .full_width()
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        telemetry::event!("Extension Uninstalled", uninstall_id);
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store
                                .uninstall_extension(uninstall_id.clone(), cx)
                                .detach_and_log_err(cx);
                        });
                    }),
            );
            if entry.dev {
                let rebuild_id = entry.id.clone();
                actions = actions.child(
                    Button::new("detail-rebuild", "Rebuild")
                        .style(ButtonStyle::Outlined)
                        .full_width()
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            ExtensionStore::global(cx).update(cx, |store, cx| {
                                store.rebuild_dev_extension(rebuild_id.clone(), cx);
                            });
                        }),
                );
            }
        } else {
            let install_id = entry.id.clone();
            actions = actions.child(
                Button::new("detail-install", "Install")
                    .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                    .full_width()
                    .disabled(busy)
                    .on_click(move |_, _, cx| {
                        telemetry::event!("Extension Installed");
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.install_latest_extension(install_id.clone(), cx)
                        });
                    }),
            );
        }

        let description_text = entry.description.clone();
        let has_description = description_text.is_some();

        Some(
            v_flex()
                .w(rems_from_px(360_f32))
                .flex_shrink_0()
                .h_full()
                .p_4()
                .gap_3()
                .bg(colors.panel_background)
                .border_l_1()
                .border_color(colors.border_variant)
                .child(Headline::new(entry.name.clone()).size(HeadlineSize::Large))
                .child(
                    Label::new(format!("v{} · {}", entry.version, entry.authors))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(description_text.unwrap_or_else(|| "No description".to_string()))
                        .size(LabelSize::Default)
                        .color(if has_description {
                            Color::Default
                        } else {
                            Color::Muted
                        }),
                )
                .when(!provided.is_empty(), |this| {
                    this.child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(provided.into_iter().map(Chip::new)),
                    )
                })
                .when_some(entry.repository_url.clone(), |this, repository_url| {
                    let icon = self.get_repository_icon(&repository_url);
                    this.child(
                        h_flex().gap_2().child(
                            Button::new("detail-repository", "Repository")
                                .style(ButtonStyle::OutlinedGhost)
                                .start_icon(Icon::new(icon).size(IconSize::Small))
                                .on_click(move |_, _, cx| {
                                    cx.open_url(&repository_url);
                                }),
                        ),
                    )
                })
                .when_some(entry.downloads, |this, downloads| {
                    this.child(
                        Label::new(format!("{downloads} downloads"))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                })
                .child(div().flex_1())
                .child(actions)
                .into_any_element(),
        )
    }

    fn show_extension_version_list(
        &mut self,
        extension_id: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            let extension_versions_task = this.update(cx, |this, cx| {
                this.extension_store.update(cx, |store, cx| {
                    store.fetch_extension_versions(&extension_id, cx)
                })
            })?;

            let extension_versions = extension_versions_task.await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let fs = workspace.project().read(cx).fs().clone();
                workspace.toggle_modal(window, cx, |window, cx| {
                    let delegate = ExtensionVersionSelectorDelegate::new(
                        fs,
                        cx.entity().downgrade(),
                        extension_versions,
                    );

                    ExtensionVersionSelector::new(delegate, window, cx)
                });
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_search(&self, cx: &mut Context<Self>) -> Div {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("BufferSearchBar");

        let editor_border = if self.query_contains_error {
            Color::Error.color(cx)
        } else {
            cx.theme().colors().border
        };

        h_flex()
            .key_context(key_context)
            .h_8()
            .min_w(rems_from_px(384_f32))
            .flex_1()
            .pl_1p5()
            .pr_2()
            .gap_2()
            .border_1()
            .border_color(editor_border)
            .rounded_md()
            .child(Icon::new(IconName::MagnifyingGlass).color(Color::Muted))
            .child(
                div()
                    .flex_1()
                    .child(self.render_text_input(&self.query_editor, cx)),
            )
            .when(self.fetch_state == ExtensionFetchState::Fetching, |this| {
                this.child(
                    Icon::new(IconName::LoadCircle)
                        .size(IconSize::Small)
                        .color(Color::Muted)
                        .with_rotate_animation(2),
                )
            })
    }

    fn retry_button(&self, cx: &mut Context<Self>) -> Button {
        Button::new("retry-fetch-extensions", "Retry")
            .style(ButtonStyle::Outlined)
            .on_click(cx.listener(|this, _, _, cx| {
                this.fetch_extensions(cx);
            }))
    }

    fn render_text_input(
        &self,
        editor: &Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let settings = ThemeSettings::get_global(cx);
        let text_style = TextStyle {
            color: if editor.read(cx).read_only(cx) {
                cx.theme().colors().text_disabled
            } else {
                cx.theme().colors().text
            },
            font_family: settings.ui_font.family.clone(),
            font_features: settings.ui_font.features.clone(),
            font_fallbacks: settings.ui_font.fallbacks.clone(),
            font_size: rems(0.875).into(),
            font_weight: settings.ui_font.weight,
            line_height: relative(1.3),
            ..Default::default()
        };

        EditorElement::new(
            editor,
            EditorStyle {
                background: cx.theme().colors().editor_background,
                local_player: cx.theme().players().local(),
                text: text_style,
                ..Default::default()
            },
        )
    }

    fn on_query_change(
        &mut self,
        _: Entity<Editor>,
        event: &editor::EditorEvent,
        cx: &mut Context<Self>,
    ) {
        if let editor::EditorEvent::Edited { .. } = event {
            self.query_contains_error = false;
            self.refresh_search(cx);
        }
    }

    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        self.update_local_search_results(cx);
        self.refresh_feature_upsells(cx);
    }

    pub fn focus_extension(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query_editor.update(cx, |editor, cx| {
            editor.set_text(format!("id:{id}"), window, cx)
        });
        let is_installed = self
            .extension_store
            .read(cx)
            .installed_extensions()
            .contains_key(id);
        self.plugin_manager.select_category(if is_installed {
            Category::Installed
        } else {
            Category::Marketplace
        });
        self.plugin_manager.select(Arc::<str>::from(id));
        self.refresh_search(cx);
    }

    pub fn change_provides_filter(
        &mut self,
        provides_filter: Option<ExtensionProvides>,
        cx: &mut Context<Self>,
    ) {
        let category = provides_filter
            .map(category_for_provides)
            .unwrap_or(Category::Marketplace);
        self.plugin_manager.select_category(category);
        self.rebuild_entries(cx);
        self.scroll_to_top(cx);
    }

    pub fn search_query(&self, cx: &mut App) -> Option<String> {
        let search = self.query_editor.read(cx).text(cx);
        if search.trim().is_empty() {
            None
        } else {
            Some(search)
        }
    }

    fn update_settings(
        &mut self,
        selection: &ToggleState,
        cx: &mut Context<Self>,
        callback: impl 'static + Send + Fn(&mut SettingsContent, bool),
    ) {
        if let Some(workspace) = self.workspace.upgrade() {
            let fs = workspace.read(cx).app_state().fs.clone();
            let selection = *selection;
            settings::update_settings_file(fs, cx, move |settings, _| {
                let value = match selection {
                    ToggleState::Unselected => false,
                    ToggleState::Selected => true,
                    _ => return,
                };

                callback(settings, value)
            });
        }
    }

    fn refresh_feature_upsells(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.search_query(cx) else {
            self.upsells.clear();
            return;
        };

        if let Some(id) = search.strip_prefix("id:") {
            self.upsells.clear();

            let upsell = match id.to_lowercase().as_str() {
                "ruff" => Some(Feature::ExtensionRuff),
                "basedpyright" => Some(Feature::ExtensionBasedpyright),
                "ty" => Some(Feature::ExtensionTy),
                _ => None,
            };

            if let Some(upsell) = upsell {
                self.upsells.insert(upsell);
            }

            return;
        }

        let search = search.to_lowercase();
        let search_terms = search
            .split_whitespace()
            .map(|term| term.trim())
            .collect::<Vec<_>>();

        for (feature, keywords) in keywords_by_feature() {
            if keywords
                .iter()
                .any(|keyword| search_terms.contains(keyword))
            {
                self.upsells.insert(*feature);
            } else {
                self.upsells.remove(feature);
            }
        }
    }

    fn render_feature_upsell_banner(
        &self,
        label: &'static str,
        docs_url: &'static str,
        vim: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let docs_url_button = Button::new("open_docs", "View Documentation")
            .end_icon(Icon::new(IconName::ArrowUpRight).size(IconSize::Small))
            .on_click({
                move |_event, _window, cx| {
                    telemetry::event!(
                        "Documentation Viewed",
                        source = "Feature Upsell",
                        url = docs_url,
                    );
                    cx.open_url(docs_url)
                }
            });

        div()
            .pt_4()
            .px_4()
            .child(
                Banner::new()
                    .severity(Severity::Success)
                    .child(Label::new(SharedString::new_static(label)).mt_0p5())
                    .map(|this| {
                        if vim {
                            this.action_slot(
                                h_flex()
                                    .gap_1()
                                    .child(docs_url_button)
                                    .child(Divider::vertical().color(ui::DividerColor::Border))
                                    .child(
                                        h_flex()
                                            .pl_1()
                                            .gap_1()
                                            .child(Label::new("Enable Vim mode"))
                                            .child(
                                                Switch::new(
                                                    "enable-vim",
                                                    if VimModeSetting::get_global(cx).0 {
                                                        ui::ToggleState::Selected
                                                    } else {
                                                        ui::ToggleState::Unselected
                                                    },
                                                )
                                                .on_click(cx.listener(
                                                    move |this, selection, _, cx| {
                                                        telemetry::event!(
                                                            "Vim Mode Toggled",
                                                            source = "Feature Upsell"
                                                        );
                                                        this.update_settings(
                                                            selection,
                                                            cx,
                                                            |setting, value| {
                                                                setting.vim_mode = Some(value)
                                                            },
                                                        );
                                                    },
                                                )),
                                            ),
                                    ),
                            )
                        } else {
                            this.action_slot(docs_url_button)
                        }
                    }),
            )
            .into_any_element()
    }

    fn render_feature_upsells(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex().children(self.upsells.iter().map(|feature| match feature {
            Feature::AgentClaude => self.render_feature_upsell_banner(
                "Claude Agent support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#claude-agent",
                false,
                cx,
            ),
            Feature::AgentCodex => self.render_feature_upsell_banner(
                "Codex CLI support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#codex-cli",
                false,
                cx,
            ),
            Feature::AgentGemini => self.render_feature_upsell_banner(
                "Gemini CLI support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#gemini-cli",
                false,
                cx,
            ),
            Feature::ExtensionBasedpyright => self.render_feature_upsell_banner(
                "Basedpyright (Python language server) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python#basedpyright",
                false,
                cx,
            ),
            Feature::ExtensionRuff => self.render_feature_upsell_banner(
                "Ruff (linter for Python) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python#code-formatting--linting",
                false,
                cx,
            ),
            Feature::ExtensionTailwind => self.render_feature_upsell_banner(
                "Tailwind CSS support is built-in to Zed!",
                "https://zed.dev/docs/languages/tailwindcss",
                false,
                cx,
            ),
            Feature::ExtensionTy => self.render_feature_upsell_banner(
                "Ty (Python language server) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python",
                false,
                cx,
            ),
            Feature::Git => self.render_feature_upsell_banner(
                "Zed comes with basic Git support—more features are coming in the future.",
                "https://zed.dev/docs/git",
                false,
                cx,
            ),
            Feature::LanguageBash => self.render_feature_upsell_banner(
                "Shell support is built-in to Zed!",
                "https://zed.dev/docs/languages/bash",
                false,
                cx,
            ),
            Feature::LanguageC => self.render_feature_upsell_banner(
                "C support is built-in to Zed!",
                "https://zed.dev/docs/languages/c",
                false,
                cx,
            ),
            Feature::LanguageCpp => self.render_feature_upsell_banner(
                "C++ support is built-in to Zed!",
                "https://zed.dev/docs/languages/cpp",
                false,
                cx,
            ),
            Feature::LanguageGo => self.render_feature_upsell_banner(
                "Go support is built-in to Zed!",
                "https://zed.dev/docs/languages/go",
                false,
                cx,
            ),
            Feature::LanguagePython => self.render_feature_upsell_banner(
                "Python support is built-in to Zed!",
                "https://zed.dev/docs/languages/python",
                false,
                cx,
            ),
            Feature::LanguageReact => self.render_feature_upsell_banner(
                "React support is built-in to Zed!",
                "https://zed.dev/docs/languages/typescript",
                false,
                cx,
            ),
            Feature::LanguageRust => self.render_feature_upsell_banner(
                "Rust support is built-in to Zed!",
                "https://zed.dev/docs/languages/rust",
                false,
                cx,
            ),
            Feature::LanguageTypescript => self.render_feature_upsell_banner(
                "Typescript support is built-in to Zed!",
                "https://zed.dev/docs/languages/typescript",
                false,
                cx,
            ),
            Feature::OpenIn => self.render_feature_upsell_banner(
                "Zed supports linking to a source line on GitHub and others.",
                "https://zed.dev/docs/git#git-integrations",
                false,
                cx,
            ),
            Feature::Vim => self.render_feature_upsell_banner(
                "Vim support is built-in to Zed!",
                "https://zed.dev/docs/vim",
                true,
                cx,
            ),
        }))
    }
}

struct DevExtensionRebuildPickerDelegate {
    entries: Vec<Arc<ExtensionManifest>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl DevExtensionRebuildPickerDelegate {
    fn new(manifests: Vec<Arc<ExtensionManifest>>) -> Self {
        let matches = manifests
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatch {
                candidate_id: ix,
                score: 0.0,
                positions: Vec::new(),
                string: manifest.name.clone(),
            })
            .collect();

        Self {
            entries: manifests,
            matches,
            selected_index: 0,
        }
    }
}

impl PickerDelegate for DevExtensionRebuildPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "dev-extension-rebuild"
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn selected_index_changed(
        &self,
        _ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App) + 'static>> {
        None
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatchCandidate::new(ix, manifest.name.as_ref()))
            .collect::<Vec<_>>();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update(cx, |this, _cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = this
                    .delegate
                    .selected_index
                    .min(this.delegate.matches.len().saturating_sub(1));
            })
            .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(mat) = self.matches.get(self.selected_index) else {
            return;
        };

        let extension_id = self.entries[mat.candidate_id].id.clone();
        ExtensionStore::global(cx).update(cx, |store, cx| {
            store.rebuild_dev_extension(extension_id, cx);
        });

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        Arc::from("Rebuild dev extension…")
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let mat = self.matches.get(ix)?;
        let entry = self.entries.get(mat.candidate_id)?;

        let item = ListItem::new(("dev-extension-list-item", mat.candidate_id))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .child(
                h_flex()
                    .w_full()
                    .py_px()
                    .justify_between()
                    .gap_2()
                    .child(Label::new(entry.name.clone()))
                    .child(
                        Label::new(format!("{} • v{}", entry.id, entry.version))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            );

        Some(item)
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some("No dev extensions found".into())
    }
}

impl Render for ExtensionsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        v_flex()
            .size_full()
            .bg(colors.editor_background)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .child(self.render_search(cx))
                    .child(
                        Button::new("install-dev-extension", "Install Dev Extension")
                            .style(ButtonStyle::Outlined)
                            .size(ButtonSize::Medium)
                            .on_click(|_event, window, cx| {
                                window.dispatch_action(Box::new(InstallDevExtension), cx)
                            }),
                    ),
            )
            .child(self.render_feature_upsells(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_rail(cx))
                    .child(self.render_center(window, cx))
                    .children(self.render_detail(cx)),
            )
    }
}

impl EventEmitter<ItemEvent> for ExtensionsPage {}

impl Focusable for ExtensionsPage {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.query_editor.read(cx).focus_handle(cx)
    }
}

impl Item for ExtensionsPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Extensions".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Extensions Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        f(*event)
    }
}
