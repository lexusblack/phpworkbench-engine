//! Pure state and filtering logic for the PhpStorm-style Plugins manager.
//!
//! Everything in this module is independent of `gpui` and the `ExtensionStore`
//! so that the filtering, sorting and selection rules can be unit tested
//! without spinning up an application context.

use std::{collections::BTreeSet, sync::Arc};

use cloud_api_types::ExtensionProvides;

/// Categories shown in the left rail of the plugins manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum Category {
    #[default]
    Installed,
    Marketplace,
    Updates,
    Themes,
    Languages,
    LanguageServers,
    DebugAdapters,
    Snippets,
}

impl Category {
    /// Order in which categories appear in the left rail.
    pub(crate) const ALL: [Category; 8] = [
        Category::Installed,
        Category::Marketplace,
        Category::Updates,
        Category::Themes,
        Category::Languages,
        Category::LanguageServers,
        Category::DebugAdapters,
        Category::Snippets,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Category::Installed => "Installed",
            Category::Marketplace => "Marketplace",
            Category::Updates => "Updates",
            Category::Themes => "Themes",
            Category::Languages => "Languages",
            Category::LanguageServers => "Language Servers",
            Category::DebugAdapters => "Debug Adapters",
            Category::Snippets => "Snippets",
        }
    }

    /// The `ExtensionProvides` tag backing a tag category, if any.
    pub(crate) fn provides_filter(self) -> Option<ExtensionProvides> {
        match self {
            Category::Themes => Some(ExtensionProvides::Themes),
            Category::Languages => Some(ExtensionProvides::Languages),
            Category::LanguageServers => Some(ExtensionProvides::LanguageServers),
            Category::DebugAdapters => Some(ExtensionProvides::DebugAdapters),
            Category::Snippets => Some(ExtensionProvides::Snippets),
            Category::Installed | Category::Marketplace | Category::Updates => None,
        }
    }
}

/// The display data for a single row in the plugins manager.
#[derive(Debug, Clone)]
pub(crate) struct PluginEntry {
    pub id: Arc<str>,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub authors: String,
    pub repository_url: Option<String>,
    pub provides: BTreeSet<ExtensionProvides>,
    pub installed: bool,
    pub dev: bool,
    pub downloads: Option<u64>,
}

/// Returns whether `entry` belongs in `category`.
///
/// `Installed` and `Marketplace` are list categories keyed off installation
/// state, `Updates` never matches (no update data is fetched), and the tag
/// categories match the `ExtensionProvides` tags rendered on each row.
pub(crate) fn matches_category(
    category: Category,
    installed: bool,
    provides: &BTreeSet<ExtensionProvides>,
) -> bool {
    match category {
        Category::Installed => installed,
        Category::Marketplace => !installed,
        Category::Updates => false,
        Category::Themes
        | Category::Languages
        | Category::LanguageServers
        | Category::DebugAdapters
        | Category::Snippets => provides.contains(&category.provides_filter().expect("tag category")),
    }
}

/// Case-insensitive substring search over the name and description.
pub(crate) fn matches_search(query: &str, name: &str, description: Option<&str>) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    name.to_lowercase().contains(&query)
        || description.is_some_and(|description| description.to_lowercase().contains(&query))
}

/// Sorts installed entries first, then by case-insensitive name.
pub(crate) fn sort_entries(entries: &mut [PluginEntry]) {
    entries.sort_by(|a, b| {
        b.installed
            .cmp(&a.installed)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// UI state owned by the plugins manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginManagerState {
    pub category: Category,
    pub selected: Option<Arc<str>>,
}

impl Default for PluginManagerState {
    fn default() -> Self {
        Self {
            category: Category::Installed,
            selected: None,
        }
    }
}

impl PluginManagerState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Switches category and clears the selection when the category changes.
    pub(crate) fn select_category(&mut self, category: Category) {
        if self.category != category {
            self.category = category;
            self.selected = None;
        }
    }

    pub(crate) fn select(&mut self, id: impl Into<Arc<str>>) {
        self.selected = Some(id.into());
    }

    /// Clears the selection if the selected plugin is no longer in `entries`.
    pub(crate) fn retain_selection(&mut self, entries: &[PluginEntry]) {
        if let Some(selected) = &self.selected
            && !entries.iter().any(|entry| &entry.id == selected)
        {
            self.selected = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provides(items: impl IntoIterator<Item = ExtensionProvides>) -> BTreeSet<ExtensionProvides> {
        items.into_iter().collect()
    }

    fn entry(id: &str, name: &str, installed: bool) -> PluginEntry {
        PluginEntry {
            id: Arc::from(id),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: None,
            authors: "Author".to_string(),
            repository_url: None,
            provides: BTreeSet::new(),
            installed,
            dev: false,
            downloads: None,
        }
    }

    #[test]
    fn installed_category_only_matches_installed() {
        let none = BTreeSet::new();
        assert!(matches_category(Category::Installed, true, &none));
        assert!(!matches_category(Category::Installed, false, &none));
    }

    #[test]
    fn marketplace_category_only_matches_available() {
        let none = BTreeSet::new();
        assert!(!matches_category(Category::Marketplace, true, &none));
        assert!(matches_category(Category::Marketplace, false, &none));
    }

    #[test]
    fn updates_category_is_always_empty() {
        let themes = provides([ExtensionProvides::Themes]);
        assert!(!matches_category(Category::Updates, true, &themes));
        assert!(!matches_category(Category::Updates, false, &themes));
    }

    #[test]
    fn every_tag_category_maps_to_its_tag() {
        let cases = [
            (Category::Themes, ExtensionProvides::Themes),
            (Category::Languages, ExtensionProvides::Languages),
            (
                Category::LanguageServers,
                ExtensionProvides::LanguageServers,
            ),
            (Category::DebugAdapters, ExtensionProvides::DebugAdapters),
            (Category::Snippets, ExtensionProvides::Snippets),
        ];

        for (category, tag) in cases {
            assert_eq!(category.provides_filter(), Some(tag));
            assert!(matches_category(category, false, &provides([tag])));
            assert!(!matches_category(
                category,
                true,
                &provides([ExtensionProvides::ContextServers])
            ));
        }
    }

    #[test]
    fn tags_do_not_cross_match() {
        let languages = provides([ExtensionProvides::Languages]);
        assert!(!matches_category(Category::Themes, true, &languages));
        assert!(matches_category(Category::Languages, true, &languages));
    }

    #[test]
    fn search_matches_name_case_insensitively() {
        assert!(matches_search("PHP", "PHP", None));
        assert!(matches_search("php", "My PHP Extension", Some("other")));
        assert!(!matches_search("php", "Rust", Some("not here")));
    }

    #[test]
    fn search_matches_description_case_insensitively() {
        assert!(matches_search(
            "LANGUAGE",
            "Some Extension",
            Some("Language support")
        ));
        assert!(!matches_search("python", "Some Extension", Some("PHP support")));
    }

    #[test]
    fn empty_search_matches_everything() {
        assert!(matches_search("", "Anything", None));
        assert!(matches_search("   ", "Anything", None));
    }

    #[test]
    fn sort_puts_installed_first_then_name() {
        let mut entries = vec![
            entry("z", "zeta", false),
            entry("b", "beta", false),
            entry("m", "Alpha", true),
            entry("a", "alpha", true),
        ];

        sort_entries(&mut entries);

        let names = entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha", "Alpha", "beta", "zeta"]);
    }

    #[test]
    fn selection_transitions() {
        let mut state = PluginManagerState::new();
        assert_eq!(state.category, Category::Installed);
        assert_eq!(state.selected, None);

        state.select("php");
        assert_eq!(state.selected.as_deref(), Some("php"));

        // Switching categories clears the selection.
        state.select_category(Category::Marketplace);
        assert_eq!(state.category, Category::Marketplace);
        assert_eq!(state.selected, None);

        // Re-selecting the current category keeps the selection.
        state.select("emmet");
        state.select_category(Category::Marketplace);
        assert_eq!(state.selected.as_deref(), Some("emmet"));
    }

    #[test]
    fn retain_selection_drops_missing_ids() {
        let mut state = PluginManagerState::new();
        state.select("emmet");

        let present = vec![entry("emmet", "Emmet", true)];
        state.retain_selection(&present);
        assert_eq!(state.selected.as_deref(), Some("emmet"));

        let removed = vec![entry("php", "PHP", true)];
        state.retain_selection(&removed);
        assert_eq!(state.selected, None);
    }
}
