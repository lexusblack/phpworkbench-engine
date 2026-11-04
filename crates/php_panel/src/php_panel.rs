use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;
use editor::{Editor, EditorElement, EditorEvent, EditorStyle, actions as editor_actions};
use file_icons::FileIcons;
use gpui::{
    Action, Anchor, AnyWindowHandle, App, AppContext, AsyncWindowContext, Bounds, ClickEvent,
    Context, DismissEvent, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement,
    Point, Rems, Render, SharedString, Size, StatefulInteractiveElement, Styled, Subscription,
    KeyBinding, TextStyle, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowOptions, actions,
    anchored,
    deferred, px, size,
};
use project::{Project, ProjectPath};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use settings::Settings as _;
use ui::{
    Button, Checkbox, ContextMenu, DockSide, IconButtonShape, Label, LabelSize, ListItem,
    PopoverMenu, Tooltip, ToggleState, prelude::*,
};
use util::ResultExt as _;

use workspace::dock::{DockPosition, PanelEvent};
use workspace::{OpenOptions, Panel, StatusItemView, Workspace};

mod table_view;

use table_view::{DatabaseTableView, TableTabTarget};

actions!(
    php_panel,
    [
        ToggleFocus,
        OpenDeploymentDialog,
        OpenDataSourcesDialog,
        OpenSshConfigsDialog,
        BrowseRemoteHost,
        SelectNextTab,
        SelectPrevTab,
        FocusNextDialogField,
        FocusPreviousDialogField
    ]
);

/// Adds a SQLite file picked in the project tree as a data source.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = php_panel)]
#[serde(deny_unknown_fields)]
pub struct AddAsDataSource {
    /// Absolute path of the SQLite file to add.
    #[serde(default)]
    pub abs_path: Option<String>,
}

/// Removes a data source from `.zed/databases.json` by name.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = php_panel)]
#[serde(deny_unknown_fields)]
pub struct DeleteDataSource {
    /// Name of the data source (connection) to remove.
    #[serde(default)]
    pub name: String,
}

/// Opens (adds if needed) a SQLite file as a data source and expands it in
/// the Databases tab, like double-clicking a SQLite file in PhpStorm.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = php_panel)]
#[serde(deny_unknown_fields)]
pub struct OpenSqliteDataSource {
    /// Absolute path of the SQLite file to open.
    #[serde(default)]
    pub abs_path: Option<String>,
}

/// Returns the name of an existing data source whose configured file path
/// matches `path` (worktree-relative, slash-normalized).
fn find_data_source_by_path(connections: &[DataSource], path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    connections.iter().find_map(|source| {
        if source.path.trim().replace('\\', "/") == path {
            Some(source.name.clone())
        } else {
            None
        }
    })
}

/// Ensures the SQLite file is configured as a data source and returns its name.
/// An existing connection with the same path is reused instead of duplicated.
fn ensure_sqlite_data_source(root: &Path, rel_path: &str) -> std::result::Result<String, String> {
    let sources = load_data_sources(root);
    if let Some(name) = find_data_source_by_path(&sources, rel_path) {
        return Ok(name);
    }
    upsert_sqlite_data_source(root, rel_path)
}

fn next_tab(active: PanelTab, offset: isize) -> PanelTab {
    let len = PanelTab::VISIBLE.len() as isize;
    let index = PanelTab::VISIBLE
        .iter()
        .position(|tab| *tab == active)
        .unwrap_or(0) as isize;
    PanelTab::VISIBLE[((index + offset).rem_euclid(len)) as usize]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PanelTab {
    Remote,
    Databases,
    Tests,
    Http,
    Collections,
}

impl PanelTab {
    const ALL: [PanelTab; 5] = [
        PanelTab::Remote,
        PanelTab::Databases,
        PanelTab::Tests,
        PanelTab::Http,
        PanelTab::Collections,
    ];

    /// Tabs shown in the panel tab bar; hidden tabs stay reachable in code
    /// but are not rendered or cycled through.
    const VISIBLE: [PanelTab; 3] = [PanelTab::Remote, PanelTab::Databases, PanelTab::Http];

    fn label(self) -> &'static str {
        match self {
            PanelTab::Remote => "Remote",
            PanelTab::Databases => "Databases",
            PanelTab::Tests => "Tests",
            PanelTab::Http => "HTTP",
            PanelTab::Collections => "Collections",
        }
    }
}

#[derive(Clone)]
struct RemoteServer {
    name: String,
    protocol: Option<String>,
    host: Option<String>,
    port: Option<String>,
    user: Option<String>,
    password: Option<String>,
    password_session: bool,
    ssh_config: Option<String>,
    remote_root: Option<String>,
    local_root: Option<String>,
    web_url: Option<String>,
    excluded: Vec<String>,
    upload_on_save: bool,
    use_rsync: bool,
    use_sudo: bool,
    visible: bool,
    passive_mode: bool,
    show_hidden: bool,
    list_compat: bool,
    use_mlsd: bool,
    max_connections: u32,
    keep_alive_enabled: bool,
    keep_alive_seconds: u32,
    encoding: String,
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SavePassword {
    Forever,
    Session,
}

impl SavePassword {
    fn label(self) -> &'static str {
        match self {
            SavePassword::Forever => "Forever",
            SavePassword::Session => "Session",
        }
    }

    fn next(self) -> Self {
        match self {
            SavePassword::Forever => SavePassword::Session,
            SavePassword::Session => SavePassword::Forever,
        }
    }

    fn hint(self) -> &'static str {
        match self {
            SavePassword::Forever => "Password saved permanently",
            SavePassword::Session => "Password saved for the current session",
        }
    }
}

const ADVANCED_DEFAULT_ENCODING: &str = "UTF-8";
const ADVANCED_DEFAULT_MAX_CONNECTIONS: u32 = 5;
const ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS: u32 = 300;

fn deploy_type_has_advanced(protocol: &str) -> bool {
    matches!(
        deploy_type_canonical(protocol),
        Some("ftp") | Some("ftps") | Some("webdav") | Some("webdavs")
    )
}

fn picker_query_matches(query: &str, name: &str) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&needle)
}

fn parse_local_dir_entries(items: Vec<(String, bool)>) -> Vec<RemoteEntry> {
    let mut entries: Vec<RemoteEntry> = items
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, is_directory)| RemoteEntry {
            path: name.clone(),
            name,
            is_directory,
            size: None,
        })
        .collect();
    entries.sort_by(|a, b| match (a.is_directory, b.is_directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    entries
}

const DEPLOY_TYPES: [&str; 7] = [
    "in-place", "local", "ftp", "ftps", "sftp", "webdav", "webdavs",
];

/// PhpStorm's Deployment > Type dropdown, in the exact menu order and wording.
const DEPLOY_TYPE_OPTIONS: [(&str, &str); 7] = [
    ("in-place", "In place"),
    ("local", "Local or mounted folder"),
    ("ftp", "FTP"),
    ("ftps", "FTPS"),
    ("sftp", "SFTP"),
    ("webdav", "WebDAV"),
    ("webdavs", "WebDAV over HTTPS"),
];

fn deploy_type_options() -> &'static [(&'static str, &'static str)] {
    &DEPLOY_TYPE_OPTIONS
}

fn deploy_type_canonical_from_label(label: &str) -> Option<&'static str> {
    DEPLOY_TYPE_OPTIONS
        .iter()
        .find(|(_, known)| *known == label)
        .map(|(protocol, _)| *protocol)
}

fn deploy_type_canonical(protocol: &str) -> Option<&'static str> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "local" => Some("local"),
        "in-place" | "inplace" | "in_place" | "in place" => Some("in-place"),
        "sftp" | "ssh" => Some("sftp"),
        "ftp" => Some("ftp"),
        "ftps" | "ftp-ssl" | "ftpes" => Some("ftps"),
        "webdav" | "dav" => Some("webdav"),
        "webdavs" | "davs" | "dav+" => Some("webdavs"),
        _ => None,
    }
}

fn deploy_type_badge_short(protocol: &str) -> &'static str {
    match deploy_type_canonical(protocol) {
        Some("local") => "local",
        Some("in-place") => "dl",
        Some("sftp") => "ssh",
        Some("ftp") | Some("ftps") => "ftp",
        Some("webdav") => "dav",
        Some("webdavs") => "dav+",
        _ => "?",
    }
}

fn deploy_type_badge_color(protocol: &str) -> Color {
    match deploy_type_canonical(protocol) {
        Some("local") => Color::Success,
        Some("in-place") => Color::Accent,
        Some("sftp") => Color::Info,
        Some("ftp") | Some("ftps") => Color::Warning,
        Some("webdav") | Some("webdavs") => Color::Created,
        _ => Color::Muted,
    }
}

fn deploy_type_display_name(protocol: &str) -> String {
    match deploy_type_canonical(protocol) {
        Some("local") => "Local or mounted folder".to_string(),
        Some("in-place") => "In place".to_string(),
        Some("sftp") => "SFTP".to_string(),
        Some("ftp") => "FTP".to_string(),
        Some("ftps") => "FTPS".to_string(),
        Some("webdav") => "WebDAV".to_string(),
        Some("webdavs") => "WebDAV over HTTPS".to_string(),
        _ => protocol.to_string(),
    }
}

fn deploy_default_port(protocol: &str) -> Option<&'static str> {
    match deploy_type_canonical(protocol) {
        Some("sftp") => Some("22"),
        Some("ftp") => Some("21"),
        Some("ftps") => Some("990"),
        Some("webdav") => Some("80"),
        Some("webdavs") => Some("443"),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeployField {
    SshConfig,
    HostPort,
    UserAnonymous,
    Password,
    Folder,
    RootPath,
    WebUrl,
    TestConnection,
    RsyncToggle,
    SudoToggle,
}

fn deploy_connection_fields(protocol: &str) -> &'static [DeployField] {
    match deploy_type_canonical(protocol) {
        Some("sftp") => &[
            DeployField::SshConfig,
            DeployField::TestConnection,
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::RsyncToggle,
            DeployField::SudoToggle,
        ],
        Some("ftp") | Some("ftps") => &[
            DeployField::HostPort,
            DeployField::UserAnonymous,
            DeployField::Password,
            DeployField::TestConnection,
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::RsyncToggle,
        ],
        Some("webdav") | Some("webdavs") => &[
            DeployField::HostPort,
            DeployField::UserAnonymous,
            DeployField::Password,
            DeployField::TestConnection,
            DeployField::RootPath,
            DeployField::WebUrl,
        ],
        Some("local") => &[DeployField::Folder, DeployField::WebUrl],
        Some("in-place") => &[DeployField::WebUrl],
        _ => &[DeployField::RootPath, DeployField::WebUrl],
    }
}

fn deploy_mapping_labels() -> [&'static str; 3] {
    ["Local path:", "Deployment path:", "Web path:"]
}

#[derive(Clone)]
struct RemoteEntry {
    name: String,
    path: String,
    is_directory: bool,
    size: Option<u64>,
}

enum RemoteStatus {
    Loading,
    Opened,
    Failed(String),
}

#[derive(Clone)]
struct DatabaseEntry {
    name: String,
    driver: Option<String>,
    target: Option<String>,
    user: Option<String>,
    host: Option<String>,
    root: PathBuf,
    read_only: bool,
}

/// Label shown on a table tab: the sqlite file name when known, else the
/// configured connection name.
fn database_connection_label(database: &DatabaseEntry) -> String {
    if database.driver.as_deref() == Some("sqlite")
        && let Some(target) = database.target.as_deref()
        && let Some(name) = Path::new(target).file_name()
    {
        return name.to_string_lossy().into_owned();
    }
    database.name.clone()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKind {
    Sql,
}

#[derive(Clone)]
struct FormState {
    kind: FormKind,
    fields: Vec<String>,
    active: usize,
    message: Option<String>,
    connection: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKeyOutcome {
    Continue,
    Save,
    Cancel,
}

impl FormState {
    fn sql(connection: String) -> Self {
        Self {
            kind: FormKind::Sql,
            fields: vec![String::new()],
            active: 0,
            message: None,
            connection: Some(connection),
        }
    }

    fn labels(&self) -> &'static [&'static str] {
        match self.kind {
            FormKind::Sql => &["sql"],
        }
    }

    fn placeholder(&self, index: usize) -> Option<&'static str> {
        match (self.kind, index) {
            (FormKind::Sql, 0) => Some("select * from users limit 20"),
            _ => None,
        }
    }

    fn handle_key(&mut self, key: &str, key_char: Option<&str>) -> FormKeyOutcome {
        match key {
            "escape" => return FormKeyOutcome::Cancel,
            "enter" => return FormKeyOutcome::Save,
            _ => {}
        }
        if self.fields.is_empty() {
            return FormKeyOutcome::Continue;
        }
        let field_count = self.fields.len();
        match key {
            "tab" | "down" => self.active = (self.active + 1) % field_count,
            "up" => self.active = (self.active + field_count - 1) % field_count,
            "backspace" => {
                if let Some(field) = self.fields.get_mut(self.active) {
                    field.pop();
                }
            }
            "space" => {
                if let Some(field) = self.fields.get_mut(self.active) {
                    field.push(' ');
                }
            }
            _ => {
                if let Some(character) = key_char {
                    if let Some(field) = self.fields.get_mut(self.active) {
                        field.push_str(character);
                    }
                }
            }
        }
        self.message = None;
        FormKeyOutcome::Continue
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeployTab {
    Connection,
    Mappings,
    Excluded,
}

impl DeployTab {
    fn label(self) -> &'static str {
        match self {
            DeployTab::Connection => "Connection",
            DeployTab::Mappings => "Mappings",
            DeployTab::Excluded => "Excluded Paths",
        }
    }
}

#[derive(Clone, Default)]
struct DialogBuffer {
    name: String,
    host: String,
    port: String,
    user: String,
    password: String,
    password_session: bool,
    remote_root: String,
    local_root: String,
    web_url: String,
    excluded: String,
    max_connections: String,
    keep_alive_seconds: String,
    encoding: String,
}

impl DialogBuffer {
    fn load(server: Option<&RemoteServer>) -> Self {
        match server {
            Some(server) => Self {
                name: server.name.clone(),
                host: server.host.clone().unwrap_or_default(),
                port: server.port.clone().unwrap_or_default(),
                user: server.user.clone().unwrap_or_default(),
                password: server.password.clone().unwrap_or_default(),
                password_session: server.password_session,
                remote_root: server.remote_root.clone().unwrap_or_default(),
                local_root: server.local_root.clone().unwrap_or_default(),
                web_url: server.web_url.clone().unwrap_or_default(),
                excluded: excluded_text(Some(server.excluded.as_slice())),
                max_connections: server.max_connections.to_string(),
                keep_alive_seconds: server.keep_alive_seconds.to_string(),
                encoding: server.encoding.clone(),
            },
            None => Self {
                password_session: false,
                max_connections: ADVANCED_DEFAULT_MAX_CONNECTIONS.to_string(),
                keep_alive_seconds: ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS.to_string(),
                encoding: ADVANCED_DEFAULT_ENCODING.to_string(),
                ..Self::default()
            },
        }
    }

    fn connection_fields(&self, protocol: &str) -> Vec<String> {
        vec![
            self.name.clone(),
            protocol.to_string(),
            self.host.clone(),
            self.port.clone(),
            self.user.clone(),
            self.remote_root.clone(),
        ]
    }

    fn mapping_fields(&self) -> Vec<String> {
        vec![
            self.local_root.clone(),
            self.remote_root.clone(),
            self.web_url.clone(),
        ]
    }

    fn apply(&mut self, server: &mut RemoteServer, protocol: &str) {
        apply_connection_fields(server, &self.connection_fields(protocol));
        apply_mapping_fields(server, &self.mapping_fields());
        apply_excluded_field(server, &[self.excluded.clone()]);
        let password = self.password.trim().to_string();
        server.password = if password.is_empty() {
            None
        } else {
            Some(password)
        };
        server.password_session = self.password_session;
        server.max_connections = self
            .max_connections
            .trim()
            .parse::<u32>()
            .unwrap_or(ADVANCED_DEFAULT_MAX_CONNECTIONS);
        server.keep_alive_seconds = self
            .keep_alive_seconds
            .trim()
            .parse::<u32>()
            .unwrap_or(ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS);
        let encoding = self.encoding.trim().to_string();
        server.encoding = if encoding.is_empty() {
            ADVANCED_DEFAULT_ENCODING.to_string()
        } else {
            encoding
        };
    }
}

fn dialog_add_server(servers: &mut Vec<RemoteServer>, root: &Path, next_number: &mut usize) {
    let label = *next_number;
    *next_number += 1;
    let mut blank = blank_server(root.to_path_buf());
    blank.name = format!("server{label}");
    servers.push(blank);
}

pub struct DeployDialogWindow(pub Option<AnyWindowHandle>);

impl gpui::Global for DeployDialogWindow {}
pub struct DeploymentDialog {
    root: PathBuf,
    servers: Vec<RemoteServer>,
    selected: Option<usize>,
    tab: DeployTab,
    protocol: String,
    next_number: usize,
    message: Option<(String, Color)>,
    panel: Option<WeakEntity<PhpPanel>>,
    ssh_config_name: String,
    ssh_config_missing: Option<String>,
    ssh_menu_open: bool,
    type_menu_open: bool,
    ssh_configs: Vec<SshEntry>,
    openssh_hosts: Vec<OpenSshHost>,
    advanced_open: bool,
    save_password: SavePassword,
    name_editor: Entity<Editor>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    user_editor: Entity<Editor>,
    password_editor: Entity<Editor>,
    remote_root_editor: Entity<Editor>,
    local_root_editor: Entity<Editor>,
    web_url_editor: Entity<Editor>,
    excluded_editor: Entity<Editor>,
    max_connections_editor: Entity<Editor>,
    keep_alive_seconds_editor: Entity<Editor>,
    encoding_editor: Entity<Editor>,
    validate_warnings: Vec<(String, Vec<String>)>,
    validate_seq: u64,
    autodetect_status: Option<(String, Color)>,
    autodetect_seq: u64,
    focus_handles: HashMap<DeployFocusSlot, FocusHandle>,
    focus_order: DialogFocusOrder,
    focus_tab: DeployTab,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for DeploymentDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl DeploymentDialog {
    fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut dialog = Self {
            root,
            servers: Vec::new(),
            selected: None,
            tab: DeployTab::Connection,
            protocol: "local".to_string(),
            next_number: 1,
            message: None,
            panel: None,
            ssh_config_name: String::new(),
            ssh_config_missing: None,
            ssh_menu_open: false,
            type_menu_open: false,
            ssh_configs: Vec::new(),
            openssh_hosts: Vec::new(),
            advanced_open: false,
            save_password: SavePassword::Forever,
            name_editor: cx.new(|cx| Editor::single_line(window, cx)),
            host_editor: cx.new(|cx| Editor::single_line(window, cx)),
            port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            user_editor: cx.new(|cx| Editor::single_line(window, cx)),
            password_editor: cx.new(|cx| Editor::single_line(window, cx)),
            remote_root_editor: cx.new(|cx| Editor::single_line(window, cx)),
            local_root_editor: cx.new(|cx| Editor::single_line(window, cx)),
            web_url_editor: cx.new(|cx| Editor::single_line(window, cx)),
            excluded_editor: cx.new(|cx| Editor::single_line(window, cx)),
            max_connections_editor: cx.new(|cx| Editor::single_line(window, cx)),
            keep_alive_seconds_editor: cx.new(|cx| Editor::single_line(window, cx)),
            encoding_editor: cx.new(|cx| Editor::single_line(window, cx)),
            validate_warnings: Vec::new(),
            validate_seq: 0,
            autodetect_status: None,
            autodetect_seq: 0,
            focus_handles: HashMap::new(),
            focus_order: DialogFocusOrder::new(),
            focus_tab: DeployTab::Connection,
            _subscriptions: Vec::new(),
        };
        let name_editor = dialog.name_editor.clone();
        dialog.bind_validation_editor(name_editor, cx, |server, text| server.name = text);
        let host_editor = dialog.host_editor.clone();
        dialog.bind_validation_editor(host_editor, cx, |server, text| {
            server.host = if text.is_empty() { None } else { Some(text) };
        });
        let port_editor = dialog.port_editor.clone();
        dialog.bind_validation_editor(port_editor, cx, |server, text| {
            server.port = if text.is_empty() { None } else { Some(text) };
        });
        let user_editor = dialog.user_editor.clone();
        dialog.bind_validation_editor(user_editor, cx, |server, text| {
            server.user = if text.is_empty() { None } else { Some(text) };
        });
        let remote_root_editor = dialog.remote_root_editor.clone();
        dialog.bind_validation_editor(remote_root_editor, cx, |server, text| {
            server.remote_root = if text.is_empty() { None } else { Some(text) };
        });
        let local_root_editor = dialog.local_root_editor.clone();
        dialog.bind_validation_editor(local_root_editor, cx, |server, text| {
            server.local_root = if text.is_empty() { None } else { Some(text) };
        });
        let web_url_editor = dialog.web_url_editor.clone();
        dialog.bind_validation_editor(web_url_editor, cx, |server, text| {
            server.web_url = if text.is_empty() { None } else { Some(text) };
        });
        let mut last_bounds: Option<(f32, f32)> = None;
        dialog
            ._subscriptions
            .push(cx.observe_window_bounds(window, move |_, window, _| {
                let bounds = window.bounds();
                let current = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                if last_bounds != Some(current) {
                    last_bounds = Some(current);
                    save_dialog_size("deployment", bounds);
                }
            }));
        dialog.seed_from_disk(window, cx);
        dialog
    }

    fn focus_next_field(
        &mut self,
        _: &FocusNextDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.next(window, cx);
        cx.notify();
    }

    fn focus_previous_field(
        &mut self,
        _: &FocusPreviousDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.prev(window, cx);
        cx.notify();
    }

    fn deploy_focus_handle(&mut self, slot: DeployFocusSlot, cx: &App) -> FocusHandle {
        match slot {
            DeployFocusSlot::Host => self.host_editor.focus_handle(cx),
            DeployFocusSlot::Port => self.port_editor.focus_handle(cx),
            DeployFocusSlot::User => self.user_editor.focus_handle(cx),
            DeployFocusSlot::Password => self.password_editor.focus_handle(cx),
            DeployFocusSlot::LocalRoot | DeployFocusSlot::MappingLocal => {
                self.local_root_editor.focus_handle(cx)
            }
            DeployFocusSlot::RemoteRoot | DeployFocusSlot::MappingRemote => {
                self.remote_root_editor.focus_handle(cx)
            }
            DeployFocusSlot::WebUrl | DeployFocusSlot::MappingWeb => {
                self.web_url_editor.focus_handle(cx)
            }
            DeployFocusSlot::ExcludedPaths => self.excluded_editor.focus_handle(cx),
            other => dialog_focus_handle(&mut self.focus_handles, other, cx),
        }
    }

    fn bind_validation_editor<F>(
        &mut self,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
        update: F,
    ) where
        F: Fn(&mut RemoteServer, String) + 'static,
    {
        let subscription = cx.subscribe(&editor, move |this, editor, event, cx| {
            if !matches!(event, EditorEvent::BufferEdited) {
                return;
            }
            let text = editor.read(cx).text(cx);
            if let Some(index) = this.selected {
                if let Some(server) = this.servers.get_mut(index) {
                    update(server, text);
                }
            }
            this.update_next_number();
            this.schedule_validate_soon(cx);
            cx.notify();
        });
        self._subscriptions.push(subscription);
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers = load_deploy_servers(&self.root);
        self.ssh_configs = load_ssh_configs(&self.root);
        let project_root = self.root.to_string_lossy().to_string();
        for server in self.servers.iter_mut() {
            let fill = match server.local_root.as_deref() {
                None => true,
                Some(value) => value.trim().is_empty() || value.trim() == ".",
            };
            if fill {
                server.local_root = Some(project_root.clone());
            }
            if server.remote_root.as_deref().map(str::trim).unwrap_or("").is_empty() {
                server.remote_root = Some("/".to_string());
            }
        }
        if self.servers.is_empty() {
            dialog_add_server(&mut self.servers, &self.root, &mut self.next_number);
        }
        let names = self
            .servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let config = read_json(&self.root.join(".zed/deploy.json"));
        let configured_active = config
            .as_ref()
            .and_then(|config| config.get("active"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let configured_default = config
            .as_ref()
            .and_then(|config| config.get("default"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.selected = resolve_active_server(configured_active, configured_default, &names)
            .and_then(|name| self.servers.iter().position(|server| server.name == name))
            .or(if self.servers.is_empty() {
                None
            } else {
                Some(0)
            });
        if cx.has_global::<DeployShared>() {
            self.panel = cx.global::<DeployShared>().panel.clone();
        }
        self.update_next_number();
        if let Some(server) = self.selected.and_then(|index| self.servers.get(index)) {
            self.protocol = deploy_type_canonical(server.protocol.as_deref().unwrap_or("local"))
                .unwrap_or("local")
                .to_string();
        }
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn resume_with_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.root = root;
        self.tab = DeployTab::Connection;
        self.message = None;
        self.seed_from_disk(window, cx);
    }

    fn update_next_number(&mut self) {
        self.next_number = dialog_next_number(&self.servers);
    }

    fn load_editors(&mut self, window: &mut Window, cx: &mut App) {
        let buffer = DialogBuffer::load(self.selected.and_then(|index| self.servers.get(index)));
        self.protocol = self
            .selected
            .and_then(|index| self.servers.get(index))
            .and_then(|server| server.protocol.clone())
            .filter(|protocol| !protocol.is_empty())
            .unwrap_or_else(|| "local".to_string());
        let mut set = |editor: &Entity<Editor>, value: String| {
            editor.update(cx, |editor, cx| editor.set_text(value, window, cx));
        };
        set(&self.name_editor, buffer.name.clone());
        set(&self.host_editor, buffer.host.clone());
        let port = if buffer.port.is_empty() {
            deploy_default_port(&self.protocol)
                .unwrap_or_default()
                .to_string()
        } else {
            buffer.port.clone()
        };
        set(&self.port_editor, port);
        set(&self.user_editor, buffer.user.clone());
        set(&self.password_editor, buffer.password.clone());
        self.save_password = if buffer.password_session {
            SavePassword::Session
        } else {
            SavePassword::Forever
        };
        self.ssh_config_name = self
            .selected
            .and_then(|index| self.servers.get(index))
            .and_then(|server| server.ssh_config.clone())
            .unwrap_or_default();
        self.ssh_config_missing = None;
        if let Some(alias) = self
            .ssh_config_name
            .strip_prefix("openssh:")
            .map(str::trim)
            .map(str::to_string)
        {
            let hosts = load_openssh_hosts();
            if !hosts.iter().any(|host| host.alias == alias) {
                self.ssh_config_missing = Some(openssh_missing_alias_message(&alias));
            }
        }
        self.ssh_menu_open = false;
        self.type_menu_open = false;
        set(&self.remote_root_editor, buffer.remote_root.clone());
        set(&self.local_root_editor, buffer.local_root.clone());
        set(&self.web_url_editor, buffer.web_url.clone());
        set(&self.excluded_editor, buffer.excluded.clone());
        set(&self.max_connections_editor, buffer.max_connections.clone());
        set(
            &self.keep_alive_seconds_editor,
            buffer.keep_alive_seconds.clone(),
        );
        set(&self.encoding_editor, buffer.encoding.clone());
    }

    fn buffer(&self, cx: &App) -> DialogBuffer {
        let text = |editor: &Entity<Editor>| editor.read(cx).text(cx);
        DialogBuffer {
            name: text(&self.name_editor),
            host: text(&self.host_editor),
            port: text(&self.port_editor),
            user: text(&self.user_editor),
            password: text(&self.password_editor),
            password_session: self.save_password == SavePassword::Session,
            remote_root: text(&self.remote_root_editor),
            local_root: text(&self.local_root_editor),
            web_url: text(&self.web_url_editor),
            excluded: text(&self.excluded_editor),
            max_connections: text(&self.max_connections_editor),
            keep_alive_seconds: text(&self.keep_alive_seconds_editor),
            encoding: text(&self.encoding_editor),
        }
    }

    fn apply_editors(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let protocol = self.protocol.clone();
        let mut buffer = self.buffer(cx);
        if let Some(server) = self.servers.get_mut(index) {
            buffer.apply(server, &protocol);
        }
        self.update_next_number();
    }

    fn select_server(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(index) {
            return;
        }
        self.apply_editors(cx);
        self.selected = Some(index);
        self.tab = DeployTab::Connection;
        self.message = None;
        self.autodetect_status = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn select_tab(&mut self, tab: DeployTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.apply_editors(cx);
        self.tab = tab;
        self.message = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn add_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        dialog_add_server(&mut self.servers, &self.root, &mut self.next_number);
        self.selected = Some(self.servers.len() - 1);
        self.tab = DeployTab::Connection;
        self.message = None;
        self.autodetect_status = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn remove_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.message = Some(("no server selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.servers.remove(index);
        self.selected = if self.servers.is_empty() {
            None
        } else {
            Some(index.min(self.servers.len() - 1))
        };
        self.tab = DeployTab::Connection;
        self.message = None;
        self.autodetect_status = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn toggle_type_menu(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.ssh_menu_open = false;
        self.type_menu_open = !self.type_menu_open;
        cx.notify();
    }

    fn close_type_menu(&mut self, cx: &mut Context<Self>) {
        self.type_menu_open = false;
        cx.notify();
    }

    fn select_deploy_type(&mut self, protocol: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.type_menu_open = false;
        let canonical = deploy_type_canonical(protocol).unwrap_or("local");
        self.protocol = canonical.to_string();
        let default_port = deploy_default_port(canonical)
            .unwrap_or_default()
            .to_string();
        if let Some(index) = self.selected {
            if let Some(server) = self.servers.get_mut(index) {
                server.protocol = Some(canonical.to_string());
                server.port = if default_port.is_empty() {
                    None
                } else {
                    Some(default_port.clone())
                };
            }
        }
        self.port_editor.update(cx, |editor, cx| {
            editor.set_text(default_port, window, cx);
        });
        self.message = None;
        self.autodetect_status = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn toggle_visible(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            server.visible = !server.visible;
        }
        self.schedule_validate(cx);
        cx.notify();
    }

    fn toggle_rsync(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            server.use_rsync = !server.use_rsync;
        }
        cx.notify();
    }

    fn toggle_sudo(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            server.use_sudo = !server.use_sudo;
        }
        cx.notify();
    }

    fn duplicate_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            self.message = Some(("no server selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        let Some(mut copy) = self.servers.get(index).cloned() else {
            return;
        };
        let base = copy
            .name
            .trim_end_matches(|ch: char| ch.is_ascii_digit())
            .to_string();
        let (name, next_number) = deploy_copy_name(&self.servers, &base, self.next_number);
        copy.name = name;
        self.next_number = next_number;
        self.servers.insert(index + 1, copy);
        self.selected = Some(index + 1);
        self.tab = DeployTab::Connection;
        self.message = None;
        self.autodetect_status = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn toggle_advanced(&mut self, cx: &mut Context<Self>) {
        self.advanced_open = !self.advanced_open;
        cx.notify();
    }

    fn toggle_advanced_flag(
        &mut self,
        pick: impl Fn(&mut RemoteServer) -> &mut bool,
        cx: &mut Context<Self>,
    ) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            let flag = pick(server);
            *flag = !*flag;
        }
        cx.notify();
    }

    fn cycle_save_password(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let next = self.save_password.next();
        self.save_password = next;
        let Some(index) = self.selected else {
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            server.password_session = next == SavePassword::Session;
        }
        cx.notify();
    }

    fn reset_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_password = SavePassword::Forever;
        self.password_editor.update(cx, |editor, cx| {
            editor.set_text(String::new(), window, cx);
        });
        let Some(index) = self.selected else {
            cx.notify();
            return;
        };
        if let Some(server) = self.servers.get_mut(index) {
            server.password = None;
            server.password_session = false;
        }
        cx.notify();
    }

    fn open_root_picker(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(selected) = self.selected else {
            return;
        };
        let Some(server) = self.servers.get(selected).cloned() else {
            return;
        };
        let configs = load_ssh_configs(&self.root);
        let entries: Vec<Value> = self
            .servers
            .iter()
            .map(deploy_server_entry)
            .map(|mut entry| {
                let _ = resolve_ssh_into(&mut entry, &configs);
                entry
            })
            .collect();
        let content = deploy_temp_config_content(&entries, &server.name);
        let picker_path = self.root.join(".zed/deploy.picker.json");
        if let Err(error) = std::fs::create_dir_all(
            picker_path.parent().expect("picker config parent"),
        )
        .and_then(|_| {
            std::fs::write(
                &picker_path,
                serde_json::to_string(&content).expect("serialize picker config"),
            )
        }) {
            self.message = Some((format!("cannot write picker config: {error}"), Color::Error));
            cx.notify();
            return;
        }
        let local_base = match deploy_type_canonical(&server.protocol.clone().unwrap_or_default()) {
            Some("local") => Some(match server.local_root.as_deref() {
                Some(local) if !local.trim().is_empty() => self.root.join(local.trim()),
                _ => self.root.clone(),
            }),
            _ => None,
        };
        let host_display = server
            .host
            .clone()
            .unwrap_or_else(|| local_base_display(&local_base, &self.root));
        let title = format!("Choose Root Path on {} ({})", server.name, host_display);
        open_root_picker_window(
            self.root.clone(),
            server.name.clone(),
            title,
            host_display,
            local_base,
            cx,
        );
    }

    fn apply_picked_root(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.remote_root_editor.update(cx, |editor, cx| {
            editor.set_text(path.clone(), window, cx);
        });
        if let Some(index) = self.selected {
            if let Some(server) = self.servers.get_mut(index) {
                server.remote_root = Some(path);
            }
        }
        self.schedule_validate(cx);
        cx.notify();
    }

    fn render_advanced_rows(
        &self,
        selected: Option<&RemoteServer>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let passive = selected.is_some_and(|server| server.passive_mode);
        let show_hidden = selected.is_some_and(|server| server.show_hidden);
        let list_compat = selected.is_some_and(|server| server.list_compat);
        let use_mlsd = selected.is_some_and(|server| server.use_mlsd);
        let keep_alive_enabled = selected.is_some_and(|server| server.keep_alive_enabled);
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            dialog_toggle_row("deploy-advanced-passive", passive, "Passive mode", cx)
                .w_full()
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.toggle_advanced_flag(|server| &mut server.passive_mode, cx);
                }))
                .into_any_element(),
        );
        rows.push(
            dialog_toggle_row(
                "deploy-advanced-hidden",
                show_hidden,
                "Show and process hidden files",
                cx,
            )
            .w_full()
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_advanced_flag(|server| &mut server.show_hidden, cx);
            }))
            .into_any_element(),
        );
        rows.push(dialog_row(
            "Use LIST command:",
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    dialog_toggle_with(
                        "deploy-advanced-list-compat",
                        list_compat,
                        "Compatible with old version...",
                        None,
                        cx,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_advanced_flag(|server| &mut server.list_compat, cx);
                    }))
                    .into_any_element(),
                )
                .child(
                    dialog_toggle_with(
                        "deploy-advanced-mlsd",
                        use_mlsd,
                        "Instead of MLSD",
                        None,
                        cx,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_advanced_flag(|server| &mut server.use_mlsd, cx);
                    }))
                    .into_any_element(),
                ),
        ));
        rows.push(dialog_row(
            "Number of connections:",
            dialog_editor(
                "deploy-advanced-connections-editor",
                &self.max_connections_editor,
                Some(px(60.)),
                window,
                cx,
            ),
        ));
        rows.push(
            dialog_toggle_row_with(
                "deploy-advanced-keep-alive",
                keep_alive_enabled,
                "Send keep-alive messages each",
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(dialog_editor(
                        "deploy-advanced-keep-alive-editor",
                        &self.keep_alive_seconds_editor,
                        Some(px(55.)),
                        window,
                        cx,
                    ))
                    .child(
                        Label::new("seconds")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    ),
                cx,
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_advanced_flag(|server| &mut server.keep_alive_enabled, cx);
            }))
            .into_any_element(),
        );
        rows.push(dialog_row(
            "Encoding for client-server communication:",
            dialog_editor(
                "deploy-advanced-encoding-editor",
                &self.encoding_editor,
                Some(px(120.)),
                window,
                cx,
            ),
        ));
        rows
    }

    fn toggle_anonymous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        let anonymous = self
            .servers
            .get(index)
            .is_some_and(|server| server.user.as_deref() == Some("anonymous"));
        if let Some(server) = self.servers.get_mut(index) {
            if anonymous {
                server.user = None;
            } else {
                server.user = Some("anonymous".to_string());
                server.password = None;
            }
        }
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn reload_ssh_configs(&mut self, cx: &mut Context<Self>) {
        self.ssh_configs = load_ssh_configs(&self.root);
        cx.notify();
    }

    fn toggle_ssh_menu(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.reload_ssh_configs(cx);
        self.openssh_hosts = load_openssh_hosts();
        self.ssh_menu_open = !self.ssh_menu_open;
        cx.notify();
    }

    fn close_ssh_menu(&mut self, cx: &mut Context<Self>) {
        self.ssh_menu_open = false;
        cx.notify();
    }

    fn select_ssh_config(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.ssh_menu_open = false;
        let Some(config) = self.ssh_configs.get(index).cloned() else {
            return;
        };
        let Some(selected) = self.selected else {
            return;
        };
        let Some(server) = self.servers.get_mut(selected) else {
            return;
        };
        server.ssh_config = Some(config.name.clone());
        server.user = if config.user.trim().is_empty() {
            None
        } else {
            Some(config.user.trim().to_string())
        };
        server.host = if config.host.trim().is_empty() {
            None
        } else {
            Some(config.host.trim().to_string())
        };
        server.port = Some(ssh_entry_port(&config).to_string());
        self.ssh_config_name = config.name.clone();
        self.ssh_config_missing = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn select_openssh_config(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.ssh_menu_open = false;
        let Some(host) = self.openssh_hosts.get(index).cloned() else {
            return;
        };
        let Some(selected) = self.selected else {
            return;
        };
        let Some(server) = self.servers.get_mut(selected) else {
            return;
        };
        server.ssh_config = Some(format!("openssh:{}", host.alias));
        server.user = host
            .user
            .clone()
            .map(|user| user.trim().to_string())
            .filter(|user| !user.is_empty());
        server.host = if host.host_name.trim().is_empty() {
            None
        } else {
            Some(host.host_name.trim().to_string())
        };
        server.port = Some(
            host.port
                .as_deref()
                .map(str::trim)
                .filter(|port| !port.is_empty())
                .unwrap_or("22")
                .to_string(),
        );
        self.ssh_config_name = format!("openssh:{}", host.alias);
        self.ssh_config_missing = None;
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn open_ssh_configs(&mut self, cx: &mut Context<Self>) {
        self.close_ssh_menu(cx);
        open_ssh_configs_window(self.root.clone(), cx);
    }

    fn run_autodetect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        self.apply_editors(cx);
        let Some(server) = self.servers.get(selected).cloned() else {
            return;
        };
        self.autodetect_seq += 1;
        let seq = self.autodetect_seq;
        self.autodetect_status = Some(("detecting...".to_string(), Color::Muted));
        cx.notify();

        let root = self.root.clone();
        let name = server.name.clone();
        let mut entry = deploy_server_entry(&server);
        let configs = load_ssh_configs(&self.root);
        let _ = resolve_ssh_into(&mut entry, &configs);
        let content = deploy_temp_config_content(&[entry], &name);
        let autodetect_path = root.join(".zed/deploy.autodetect.json");
        if std::fs::write(
            &autodetect_path,
            serde_json::to_string(&content).expect("serialize autodetect config"),
        )
        .is_err()
        {
            self.autodetect_status =
                Some(("cannot write autodetect config".to_string(), Color::Error));
            cx.notify();
            return;
        }
        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-deploy");
            command
                .arg("autodetect")
                .arg("--config")
                .arg(autodetect_path)
                .arg("--name")
                .arg(&name)
                .arg("--json")
                .current_dir(&root);
            command.output().await
        });

        cx.spawn_in(window, async move |weak, cx| {
            let output = task.await;
            weak.update_in(cx, |dialog, window, cx| {
                if dialog.autodetect_seq != seq {
                    return;
                }
                let found = output
                    .ok()
                    .filter(|output| output.status.success())
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|value| parse_autodetect_root(&value));
                dialog.autodetect_status = Some(match found {
                    Some(found) => {
                        dialog.remote_root_editor.update(cx, |editor, cx| {
                            editor.set_text(found.clone(), window, cx);
                        });
                        if let Some(index) = dialog.selected {
                            if let Some(server) = dialog.servers.get_mut(index) {
                                server.remote_root = Some(found.clone());
                            }
                        }
                        (format!("root found: {found}"), Color::Success)
                    }
                    None => ("no root found".to_string(), Color::Warning),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn schedule_validate_soon(&mut self, cx: &mut Context<Self>) {
        self.validate_seq += 1;
        let seq = self.validate_seq;
        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(220))
                .await;
            weak.update(cx, |dialog, cx| {
                if dialog.validate_seq == seq {
                    dialog.schedule_validate(cx);
                }
            })
            .log_err();
        })
        .detach();
    }

    fn schedule_validate(&mut self, cx: &mut Context<Self>) {
        self.validate_seq += 1;
        let seq = self.validate_seq;
        let Some(selected) = self.selected else {
            self.validate_warnings = Vec::new();
            return;
        };
        let Some(active) = self.servers.get(selected).cloned() else {
            self.validate_warnings = Vec::new();
            return;
        };
        let configs = load_ssh_configs(&self.root);
        let mut ssh_warnings = Vec::new();
        let entries = self
            .servers
            .iter()
            .map(deploy_server_entry)
            .map(|mut entry| {
                let name = entry
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Err(message) = resolve_ssh_into(&mut entry, &configs) {
                    ssh_warnings.push((name, vec![message]));
                }
                entry
            })
            .collect::<Vec<_>>();
        let content = deploy_temp_config_content(&entries, &active.name);
        let validate_path = self.root.join(".zed/deploy.validate.json");
        let written =
            std::fs::create_dir_all(validate_path.parent().expect("validate config parent"))
                .and_then(|_| {
                    std::fs::write(
                        &validate_path,
                        serde_json::to_string(&content).expect("serialize validate config"),
                    )
                });
        self.validate_warnings = Vec::new();
        if written.is_err() {
            return;
        }
        cx.notify();

        let root = self.root.clone();
        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-deploy");
            command
                .arg("validate")
                .arg("--config")
                .arg(&validate_path)
                .arg("--json")
                .current_dir(&root);
            command.output().await
        });

        cx.spawn(async move |_, cx| {
            let output = task.await;
            weak.update(cx, |dialog, cx| {
                if dialog.validate_seq != seq {
                    return;
                }
                dialog.validate_warnings = output
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .map(|value| parse_validate_output(&value))
                    .unwrap_or_default();
                dialog.validate_warnings.extend(ssh_warnings);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            self.message = Some(("no server selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.apply_editors(cx);
        let Some(server) = self.servers.get(selected).cloned() else {
            return;
        };
        let mut entry = deploy_server_entry(&server);
        let configs = load_ssh_configs(&self.root);
        if let Err(message) = resolve_ssh_into(&mut entry, &configs) {
            self.message = Some((message, Color::Error));
            cx.notify();
            return;
        }
        let content = deploy_temp_config_content(&[entry], &server.name);
        let test_path = self.root.join(".zed/deploy.test.json");
        if let Err(error) = std::fs::create_dir_all(test_path.parent().expect("test config parent"))
            .and_then(|_| {
                std::fs::write(
                    &test_path,
                    serde_json::to_string(&content).expect("serialize test config"),
                )
            })
        {
            self.message = Some((format!("cannot write test config: {error}"), Color::Error));
            cx.notify();
            return;
        }
        self.message = Some(("testing...".to_string(), Color::Muted));
        cx.notify();

        let root = self.root.clone();
        let name = server.name.clone();
        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-deploy");
            command
                .arg("ls")
                .arg("--config")
                .arg(&test_path)
                .arg("--server")
                .arg(&name)
                .arg("--path")
                .arg(".")
                .arg("--json")
                .current_dir(&root);
            command_result(command.output().await, "connection failed")
        });

        cx.spawn(async move |_, cx| {
            let result = task.await;
            weak.update(cx, |dialog, cx| {
                dialog.message = Some(match result {
                    Ok(()) => ("connection ok".to_string(), Color::Success),
                    Err(message) => (format!("connection failed - {message}"), Color::Error),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        if self.servers.is_empty() {
            self.message = Some(("no servers to save".to_string(), Color::Error));
            cx.notify();
            return;
        }
        if let Some(problem) = validate_staged_servers(&self.servers) {
            self.message = Some((problem, Color::Error));
            cx.notify();
            return;
        }
        let names = self
            .servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let configured = read_json(&self.root.join(".zed/deploy.json")).and_then(|config| {
            config
                .get("active")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let active = configured
            .filter(|name| names.iter().any(|known| known == name))
            .or_else(|| names.first().cloned());
        let servers = self.servers.clone();
        let root = self.root.clone();
        if let Err(message) = save_deploy_config(&root, &servers, active.as_deref()) {
            self.message = Some((message, Color::Error));
            cx.notify();
            return;
        }
        self.refresh_shared(cx);
        if let Some(panel) = self.panel.clone() {
            panel
                .update(cx, |panel, cx| {
                    panel.reload_remote(cx);
                    panel.saved_message = Some("deployment updated".to_string());
                })
                .log_err();
        }
        self.close(window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.has_global::<DeployDialogWindow>() {
            cx.global_mut::<DeployDialogWindow>().0 = None;
        }
        window.remove_window();
        cx.notify();
    }

    fn refresh_shared(&mut self, cx: &mut App) {
        let names = self
            .servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let active = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.name.clone())
            .or_else(|| names.first().cloned());
        let panel = self.panel.clone();
        cx.set_global(DeployShared {
            servers: names,
            active,
            panel,
            root: Some(self.root.clone()),
        });
    }
}

pub(crate) fn dialog_text_style(cx: &App) -> TextStyle {
    let settings = theme_settings::ThemeSettings::get_global(cx);
    let font = settings.ui_font.clone();
    TextStyle {
        color: cx.theme().colors().text,
        font_family: font.family.clone(),
        font_features: font.features.clone(),
        font_fallbacks: font.fallbacks.clone(),
        font_size: rems(0.825).into(),
        font_weight: font.weight,
        line_height: relative(1.3),
        ..TextStyle::default()
    }
}

fn dialog_editor_element(editor: &Entity<Editor>, cx: &App) -> AnyElement {
    let style = EditorStyle {
        background: cx.theme().colors().editor_background,
        local_player: cx.theme().players().local(),
        text: dialog_text_style(cx),
        ..EditorStyle::default()
    };
    EditorElement::new(editor, style).into_any_element()
}

/// Single text size for the whole panel UI: matches the core Zed UI scale
/// (`text_ui` = 0.825rem ≈ 13.2px), so panel text and app chrome agree.
pub(crate) const PANEL_TEXT_REMS: Rems = Rems(0.825);

const DIALOG_LABEL_WIDTH: Pixels = px(148.);
const DIALOG_ROW_HEIGHT: Pixels = px(29.);
const DIALOG_CONTROL_HEIGHT: Pixels = px(26.);
const DIALOG_CONTROL_INDENT: Pixels = px(164.);

fn dialog_label(label: impl Into<SharedString>) -> Label {
    Label::new(label)
        .size(LabelSize::Custom(PANEL_TEXT_REMS))
        .color(Color::Muted)
}

fn dialog_row(label: &'static str, control: impl IntoElement) -> AnyElement {
    h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .min_h(DIALOG_ROW_HEIGHT)
        .child(
            div()
                .flex_none()
                .w(DIALOG_LABEL_WIDTH)
                .child(dialog_label(label)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h(DIALOG_CONTROL_HEIGHT)
                .flex()
                .items_center()
                .gap_2()
                .child(control),
        )
        .into_any_element()
}

fn dialog_editor(
    id: impl Into<ElementId>,
    editor: &Entity<Editor>,
    width: Option<Pixels>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let focused = editor.read(cx).focus_handle(cx).is_focused(window);
    let mut box_div = div()
        .id(id)
        .h(DIALOG_CONTROL_HEIGHT)
        .flex()
        .items_center()
        .bg(colors.editor_background)
        .border_1()
        .border_color(if focused {
            colors.border_focused
        } else {
            colors.border_variant
        })
        .rounded_md()
        .px_2()
        .cursor_text()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .child(dialog_editor_element(editor, cx)),
        );
    if let Some(width) = width {
        box_div = box_div.flex_none().w(width);
    } else {
        box_div = box_div.flex_1().min_w_0();
    }
    box_div.into_any_element()
}

fn dialog_static(text: impl Into<SharedString>, color: Color) -> AnyElement {
    h_flex()
        .flex_none()
        .px_1()
        .py_0p5()
        .child(
            Label::new(text)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(color)
                .truncate(),
        )
        .into_any_element()
}

fn dialog_section_header(text: &'static str) -> AnyElement {
    h_flex()
        .w_full()
        .flex_none()
        .px_1()
        .py_2()
        .child(
            Label::new(text.to_uppercase())
                .size(LabelSize::Custom(Rems(0.6875)))
                .color(Color::Muted),
        )
        .into_any_element()
}

fn section_header(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .flex_none()
        .py_1()
        .child(
            Label::new(text.into().to_uppercase())
                .size(LabelSize::Custom(Rems(0.6875)))
                .color(Color::Muted),
        )
        .into_any_element()
}

fn dialog_toggle_row(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    dialog_toggle_with(id, checked, label, None, cx)
}

fn dialog_toggle_row_with(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    trailing: impl IntoElement,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    dialog_toggle_with(id, checked, label, Some(trailing.into_any_element()), cx)
}

fn dialog_toggle_with(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let element_id = id.into();
    let id_shared: SharedString = format!("{:?}", element_id.clone()).into();
    h_flex()
        .id(element_id)
        .flex_none()
        .items_center()
        .gap_1()
        .h(DIALOG_CONTROL_HEIGHT)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().colors().element_hover))
        .child(
            div()
                .id(SharedString::from(format!("{id_shared}-box")))
                .flex_none()
                .size(px(14.))
                .rounded_md()
                .border_1()
                .border_color(if checked {
                    cx.theme().colors().border_focused
                } else {
                    cx.theme().colors().border_variant
                })
                .when(checked, |this| {
                    this.bg(cx.theme().colors().element_active)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::Check)
                                .size(IconSize::Small)
                                .color(Color::Accent),
                        )
                }),
        )
        .child(
            Label::new(label)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(if checked {
                    Color::Default
                } else {
                    Color::Muted
                }),
        )
        .when_some(trailing, |this, trailing| this.child(trailing))
}

#[derive(Clone, Copy, PartialEq)]
enum DialogButtonVariant {
    Primary,
    Secondary,
    Link,
}

fn dialog_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    variant: DialogButtonVariant,
) -> Button {
    let mut button = Button::new(id, label);
    match variant {
        DialogButtonVariant::Primary => {
            button = button.style(ButtonStyle::Filled);
        }
        DialogButtonVariant::Secondary => {
            button = button.style(ButtonStyle::Outlined);
        }
        DialogButtonVariant::Link => {
            button = button.style(ButtonStyle::Subtle).color(Color::Accent);
        }
    }
    button
        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
        .size(ButtonSize::Medium)
        .truncate(true)
}

fn dialog_badge(chip: impl Into<SharedString>, color: Color, cx: &App) -> gpui::AnyElement {
    let chip: SharedString = chip.into();
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .bg(cx.theme().colors().element_hover)
        .child(
            Label::new(chip)
                .size(LabelSize::Custom(Rems(0.625)))
                .color(color),
        )
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Dialog keyboard focus traversal
// ---------------------------------------------------------------------------

/// Tab / Shift+Tab traversal shared by the configuration dialogs.
///
/// The slot list is rebuilt in visual order on every render, so it always
/// matches the active tab and the controls that are actually on screen.
/// `active` remembers where traversal last landed, giving Tab a sensible
/// starting point even when no dialog field currently holds focus.
struct DialogFocusOrder {
    slots: Vec<FocusHandle>,
    cursor: FocusCursor,
}

impl DialogFocusOrder {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            cursor: FocusCursor::default(),
        }
    }

    /// Replace the slot list. Passing `reset` (used when the active tab
    /// changes) restarts traversal at the first slot of the new sub-order.
    fn set_slots(&mut self, slots: Vec<FocusHandle>, reset: bool) {
        self.slots = slots;
        self.cursor.rebase(self.slots.len(), reset);
    }

    /// Index of the slot that currently owns keyboard focus, if any.
    fn focused_index(&self, window: &Window) -> Option<usize> {
        self.slots
            .iter()
            .position(|handle| handle.is_focused(window))
    }

    fn next(&mut self, window: &mut Window, cx: &mut App) {
        self.move_by(true, window, cx);
    }

    fn prev(&mut self, window: &mut Window, cx: &mut App) {
        self.move_by(false, window, cx);
    }

    fn focus_index(&mut self, index: usize, window: &mut Window, cx: &mut App) {
        let Some(index) = self.cursor.focus(self.slots.len(), index) else {
            return;
        };
        window.focus(&self.slots[index], cx);
    }

    fn move_by(&mut self, forward: bool, window: &mut Window, cx: &mut App) {
        let target = match self.focused_index(window) {
            // Focus is already inside the order: step from it.
            Some(index) => {
                self.cursor.focus(self.slots.len(), index);
                if forward {
                    self.cursor.next(self.slots.len())
                } else {
                    self.cursor.prev(self.slots.len())
                }
            }
            // Focus started outside the order (e.g. the server-name editor,
            // or a freshly opened window): enter at the remembered anchor.
            None => self.cursor.focus(self.slots.len(), self.cursor.active),
        };
        if let Some(index) = target {
            self.focus_index(index, window, cx);
        }
    }
}

/// Pure index bookkeeping behind [`DialogFocusOrder`].
///
/// Kept free of `Window`/`FocusHandle` so the wrap-around, clamping and
/// tab-switch reset rules can be tested without an app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FocusCursor {
    active: usize,
}

impl FocusCursor {
    /// Move forward one slot, wrapping past the end.
    fn next(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        self.active = (self.active.min(len - 1) + 1) % len;
        Some(self.active)
    }

    /// Move backward one slot, wrapping before the start.
    fn prev(&mut self, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let active = self.active.min(len - 1);
        self.active = (active + len - 1) % len;
        Some(self.active)
    }

    /// Jump to an absolute index, clamped into range. Returns the index
    /// actually focused.
    fn focus(&mut self, len: usize, wanted: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        self.active = wanted.min(len - 1);
        Some(self.active)
    }

    /// Adopt a new slot list. `reset` restarts at the first slot, which is
    /// what a tab switch needs because every tab owns its own sub-order.
    fn rebase(&mut self, len: usize, reset: bool) -> Option<usize> {
        if len == 0 {
            self.active = 0;
            return None;
        }
        self.active = if reset { 0 } else { self.active.min(len - 1) };
        Some(self.active)
    }
}

/// Focus handles for controls that have no backing `Editor` entity.
/// Created once per slot and reused across renders so focus survives.
fn dialog_focus_handle<K: Clone + Eq + std::hash::Hash>(
    handles: &mut HashMap<K, FocusHandle>,
    key: K,
    cx: &App,
) -> FocusHandle {
    handles.entry(key).or_insert_with(|| cx.focus_handle()).clone()
}

fn dialog_slot<K: Copy + Eq + std::hash::Hash>(
    handles: &HashMap<K, FocusHandle>,
    key: K,
) -> FocusHandle {
    handles
        .get(&key)
        .cloned()
        .expect("dialog focus slot was registered for this render")
}

/// Register Tab / Shift+Tab / Escape for one dialog key context.
///
/// The `<context> > Editor` variant is required because the focused
/// single-line editor pushes `Editor` onto the key context path, and the
/// deepest matching binding wins. Bindings registered here run after the
/// default keymap has loaded, so they take precedence at equal depth.
///
/// Escape is bound to `editor::Cancel` in both contexts: editors propagate it
/// when they have nothing to cancel, and the dialog root's `Cancel` handler
/// closes the window. Without this, Escape would stop working as soon as Tab
/// moved focus off an editor onto a tab or button.
fn register_dialog_key_bindings(cx: &mut App, context: &str) {
    let editor_context = format!("{context} > Editor");
    cx.bind_keys([
        KeyBinding::new("tab", FocusNextDialogField, Some(context)),
        KeyBinding::new("shift-tab", FocusPreviousDialogField, Some(context)),
        KeyBinding::new("tab", FocusNextDialogField, Some(&editor_context)),
        KeyBinding::new(
            "shift-tab",
            FocusPreviousDialogField,
            Some(&editor_context),
        ),
        KeyBinding::new("escape", editor_actions::Cancel, Some(context)),
        KeyBinding::new("escape", editor_actions::Cancel, Some(&editor_context)),
    ]);
}

fn dialog_activation_key(event: &KeyDownEvent) -> bool {
    !event.keystroke.modifiers.modified()
        && matches!(event.keystroke.key.as_str(), "enter" | "space")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DeployFocusSlot {
    ConnectionTab,
    MappingsTab,
    ExcludedTab,
    Visibility,
    ServerType,
    SshConfig,
    SshBrowse,
    TestConnection,
    Host,
    Port,
    User,
    Anonymous,
    Password,
    PasswordReset,
    RemoteRoot,
    RemoteRootBrowse,
    Autodetect,
    WebUrl,
    Rsync,
    RsyncSettings,
    Sudo,
    LocalRoot,
    LocalRootBrowse,
    Advanced,
    MappingLocal,
    MappingRemote,
    MappingWeb,
    ExcludedPaths,
    Cancel,
    Ok,
}

/// Visual-order tab stops for the deployment dialog.
fn deploy_focus_slots(protocol: &str, tab: DeployTab) -> Vec<DeployFocusSlot> {
    let mut slots = vec![
        DeployFocusSlot::ConnectionTab,
        DeployFocusSlot::MappingsTab,
        DeployFocusSlot::ExcludedTab,
    ];
    match tab {
        DeployTab::Connection => {
            slots.push(DeployFocusSlot::Visibility);
            slots.push(DeployFocusSlot::ServerType);
            for field in deploy_connection_fields(protocol) {
                match field {
                    DeployField::SshConfig => {
                        slots.push(DeployFocusSlot::SshConfig);
                        slots.push(DeployFocusSlot::SshBrowse);
                    }
                    DeployField::HostPort => {
                        slots.push(DeployFocusSlot::Host);
                        slots.push(DeployFocusSlot::Port);
                    }
                    DeployField::UserAnonymous => {
                        slots.push(DeployFocusSlot::User);
                        slots.push(DeployFocusSlot::Anonymous);
                    }
                    DeployField::Password => {
                        slots.push(DeployFocusSlot::Password);
                        slots.push(DeployFocusSlot::PasswordReset);
                    }
                    DeployField::Folder => {
                        slots.push(DeployFocusSlot::LocalRoot);
                        slots.push(DeployFocusSlot::LocalRootBrowse);
                    }
                    DeployField::RootPath => {
                        slots.push(DeployFocusSlot::RemoteRoot);
                        slots.push(DeployFocusSlot::RemoteRootBrowse);
                        slots.push(DeployFocusSlot::Autodetect);
                    }
                    DeployField::WebUrl => slots.push(DeployFocusSlot::WebUrl),
                    DeployField::TestConnection => slots.push(DeployFocusSlot::TestConnection),
                    DeployField::RsyncToggle => {
                        slots.push(DeployFocusSlot::Rsync);
                        slots.push(DeployFocusSlot::RsyncSettings);
                    }
                    DeployField::SudoToggle => slots.push(DeployFocusSlot::Sudo),
                }
            }
            if deploy_type_has_advanced(protocol) {
                slots.push(DeployFocusSlot::Advanced);
            }
        }
        DeployTab::Mappings => {
            slots.push(DeployFocusSlot::MappingLocal);
            slots.push(DeployFocusSlot::MappingRemote);
            slots.push(DeployFocusSlot::MappingWeb);
        }
        DeployTab::Excluded => slots.push(DeployFocusSlot::ExcludedPaths),
    }
    slots.push(DeployFocusSlot::Cancel);
    slots.push(DeployFocusSlot::Ok);
    slots
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DataSourceFocusSlot {
    GeneralTab,
    OptionsTab,
    SshTab,
    SchemasTab,
    AdvancedTab,
    Name,
    Driver,
    Host,
    Port,
    User,
    Password,
    Database,
    Url,
    ReadOnly,
    SshToggle,
    SshConfig,
    SshEdit,
    SshLocalPort,
    SchemasAll,
    Schema(usize),
    AdvancedKey(usize),
    AdvancedValue(usize),
    AdvancedAdd,
    AdvancedRemove,
    TestConnection,
    Cancel,
    Apply,
    Ok,
}

/// Visual-order tab stops for the data sources dialog.
fn data_source_focus_slots(
    driver: &str,
    tab: DataSourceTab,
    ssh_enabled: bool,
    // `None` while the schema list is idle, loading or failed: those states
    // render no schema rows at all.
    schema_rows: Option<usize>,
    advanced_row_count: usize,
) -> Vec<DataSourceFocusSlot> {
    let mut slots = vec![
        DataSourceFocusSlot::GeneralTab,
        DataSourceFocusSlot::OptionsTab,
        DataSourceFocusSlot::SshTab,
        DataSourceFocusSlot::SchemasTab,
        DataSourceFocusSlot::AdvancedTab,
        DataSourceFocusSlot::Name,
    ];
    match tab {
        DataSourceTab::General => {
            // The driver picker only exists on the General tab.
            slots.push(DataSourceFocusSlot::Driver);
            for field in data_source_general_fields(driver) {
                match field {
                    DsGeneralField::Host => slots.push(DataSourceFocusSlot::Host),
                    DsGeneralField::Port => slots.push(DataSourceFocusSlot::Port),
                    DsGeneralField::User => slots.push(DataSourceFocusSlot::User),
                    DsGeneralField::Password => slots.push(DataSourceFocusSlot::Password),
                    DsGeneralField::Database => slots.push(DataSourceFocusSlot::Database),
                }
            }
            if driver != "sqlite" {
                slots.push(DataSourceFocusSlot::Url);
            }
        }
        DataSourceTab::Options => slots.push(DataSourceFocusSlot::ReadOnly),
        DataSourceTab::Ssh => {
            slots.push(DataSourceFocusSlot::SshToggle);
            if ssh_enabled {
                slots.push(DataSourceFocusSlot::SshConfig);
                slots.push(DataSourceFocusSlot::SshEdit);
                slots.push(DataSourceFocusSlot::SshLocalPort);
            }
        }
        DataSourceTab::Schemas => {
            if let Some(schema_count) = schema_rows {
                slots.push(DataSourceFocusSlot::SchemasAll);
                for index in 0..schema_count {
                    slots.push(DataSourceFocusSlot::Schema(index));
                }
            }
        }
        DataSourceTab::Advanced => {
            for index in 0..advanced_row_count {
                slots.push(DataSourceFocusSlot::AdvancedKey(index));
                slots.push(DataSourceFocusSlot::AdvancedValue(index));
            }
            slots.push(DataSourceFocusSlot::AdvancedAdd);
            slots.push(DataSourceFocusSlot::AdvancedRemove);
        }
    }
    slots.push(DataSourceFocusSlot::TestConnection);
    slots.push(DataSourceFocusSlot::Cancel);
    slots.push(DataSourceFocusSlot::Apply);
    slots.push(DataSourceFocusSlot::Ok);
    slots
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SshFocusSlot {
    Name,
    Host,
    Port,
    User,
    AuthType,
    Password,
    SavePassword,
    ResetPassword,
    KeyPath,
    Alias,
    KeepAlive,
    KeepAliveSeconds,
    Strict,
    TestConnection,
    Cancel,
    Apply,
    Ok,
}

/// Visual-order tab stops for the SSH configurations dialog.
fn ssh_focus_slots(auth: SshAuthMode, keep_alive_enabled: bool) -> Vec<SshFocusSlot> {
    let mut slots = vec![
        SshFocusSlot::Name,
        SshFocusSlot::Host,
        SshFocusSlot::Port,
        SshFocusSlot::User,
        SshFocusSlot::AuthType,
    ];
    match auth {
        SshAuthMode::Password => {
            slots.push(SshFocusSlot::Password);
            slots.push(SshFocusSlot::SavePassword);
            slots.push(SshFocusSlot::ResetPassword);
        }
        SshAuthMode::Key => slots.push(SshFocusSlot::KeyPath),
        SshAuthMode::Agent => {}
        SshAuthMode::Config => {
            slots.push(SshFocusSlot::Alias);
            slots.push(SshFocusSlot::KeyPath);
        }
    }
    slots.push(SshFocusSlot::KeepAlive);
    if keep_alive_enabled {
        slots.push(SshFocusSlot::KeepAliveSeconds);
    }
    slots.push(SshFocusSlot::Strict);
    slots.push(SshFocusSlot::TestConnection);
    slots.push(SshFocusSlot::Cancel);
    slots.push(SshFocusSlot::Apply);
    slots.push(SshFocusSlot::Ok);
    slots
}

impl Render for DeploymentDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let protocol = self.protocol.clone();
        let message = self.message.clone();

        // Rebuild the Tab order for the active tab / protocol before any
        // control asks for its focus handle.
        let deploy_slots = deploy_focus_slots(&protocol, self.tab);
        let mut focus: HashMap<DeployFocusSlot, FocusHandle> = HashMap::new();
        for slot in &deploy_slots {
            let handle = self.deploy_focus_handle(*slot, cx);
            focus.insert(*slot, handle);
        }
        let order = deploy_slots
            .iter()
            .map(|slot| dialog_slot(&focus, *slot))
            .collect::<Vec<_>>();
        let tab_changed = self.focus_tab != self.tab;
        self.focus_tab = self.tab;
        self.focus_order.set_slots(order, tab_changed);

        let mut server_rows = Vec::new();
        for (index, server) in self.servers.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            let badge = deploy_type_badge_short(server.protocol.as_deref().unwrap_or("server"));
            let name_element: AnyElement = if is_selected {
                dialog_editor("deploy-name-editor", &self.name_editor, None, window, cx)
            } else {
                Label::new(server.name.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(if !server.visible {
                        Color::Muted
                    } else {
                        Color::Default
                    })
                    .truncate()
                    .into_any_element()
            };
            server_rows.push(
                h_flex()
                    .id(("deploy-server-row", index))
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(colors.element_selected))
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(driver_badge(
                        badge,
                        deploy_type_badge_color(server.protocol.as_deref().unwrap_or("server")),
                        cx,
                    ))
                    .child(name_element)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_server(index, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if server_rows.is_empty() {
            server_rows.push(dialog_static("no servers", Color::Muted));
        }

        let mut tab_rows = Vec::new();
        for tab in [
            DeployTab::Connection,
            DeployTab::Mappings,
            DeployTab::Excluded,
        ] {
            let selected = tab == self.tab;
            let tab_slot = match tab {
                DeployTab::Connection => DeployFocusSlot::ConnectionTab,
                DeployTab::Mappings => DeployFocusSlot::MappingsTab,
                DeployTab::Excluded => DeployFocusSlot::ExcludedTab,
            };
            let tab_focus = dialog_slot(&focus, tab_slot);
            tab_rows.push(
                h_flex()
                    .id(("deploy-tab", tab as usize))
                    .flex_none()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .track_focus(&tab_focus)
                    .focus(|style| style.bg(colors.element_hover))
                    .when(selected, |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .when(tab_focus.is_focused(window), |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if dialog_activation_key(event) {
                            cx.stop_propagation();
                            this.select_tab(tab, window, cx);
                        }
                    }))
                    .child(
                        Label::new(tab.label())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if selected {
                                Color::Default
                            } else {
                                Color::Muted
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(tab, window, cx);
                    }))
                    .into_any_element(),
            );
        }

        let type_focus = dialog_slot(&focus, DeployFocusSlot::ServerType);
        let type_picker = h_flex()
            .id("deploy-type-picker")
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .px_1()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .track_focus(&type_focus)
            .focus(|style| style.bg(colors.element_hover))
            .hover(|style| style.bg(colors.element_hover))
            .child(dialog_badge(
                deploy_type_badge_short(&protocol),
                deploy_type_badge_color(&protocol),
                cx,
            ))
            .child(
                Label::new(deploy_type_display_name(&protocol))
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .truncate(),
            )
            .child(div().flex_1().min_w_0())
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_type_menu(cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.toggle_type_menu(cx);
                }
            }))
            .into_any_element();

        let visible_state = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.visible)
            .unwrap_or(true);

        let rsync_state = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.use_rsync)
            .unwrap_or(false);
        let sudo_state = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.use_sudo)
            .unwrap_or(false);
        let anonymous_state = self.user_editor.read(cx).text(cx).trim().eq("anonymous");

        let local_folder_empty = deploy_type_canonical(&protocol) == Some("local")
            && self.local_root_editor.read(cx).text(cx).trim().is_empty();

        let mut connection_rows: Vec<AnyElement> = Vec::new();
        for field in deploy_connection_fields(&protocol) {
            connection_rows.push(match field {
                DeployField::SshConfig => {
                    let mut rows: Vec<AnyElement> = Vec::new();
                    let config_label = if self.ssh_config_name.is_empty() {
                        "<select configuration>".to_string()
                    } else {
                        ssh_config_display_name(&self.ssh_config_name)
                    };
                    rows.push(
                        dialog_row(
                            "SSH configuration:",
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(
                                    dialog_button(
                                        "dp-ssh-config",
                                        config_label,
                                        if self.ssh_config_name.is_empty() {
                                            DialogButtonVariant::Link
                                        } else {
                                            DialogButtonVariant::Primary
                                        },
                                    )
                                    .track_focus(&dialog_slot(&focus, DeployFocusSlot::SshConfig))
                                    .end_icon(
                                        Icon::new(IconName::ChevronDown)
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.toggle_ssh_menu(cx);
                                    })),
                                )
                                .child(
                                    IconButton::new("dp-ssh-edit", IconName::Ellipsis)
                                        .shape(IconButtonShape::Square)
                                        .track_focus(&dialog_slot(&focus, DeployFocusSlot::SshBrowse))
                                        .icon_size(IconSize::Small)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text("Edit SSH configurations"))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, _window, cx| {
                                                this.open_ssh_configs(cx);
                                            },
                                        )),
                                ),
                        ),
                    );
                    if self.ssh_menu_open {
                        let selected = self.ssh_config_name.clone();
                        for (index, config) in self.ssh_configs.iter().enumerate() {
                            let is_active = selected == config.name;
                            rows.push(
                                h_flex()
                                    .id(("dp-ssh-config-row", index))
                                    .w_full()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(is_active, |this| this.bg(colors.element_selected))
                                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                                    .child(
                                        Label::new(config.name.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(ssh_user_host_text(config))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.select_ssh_config(index, window, cx);
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                        rows.push(
                            h_flex()
                                .id("dp-ssh-config-add")
                                .w_full()
                                .flex_none()
                                .items_center()
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .child(
                                    Label::new("Add new configuration...")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.open_ssh_configs(cx);
                                }))
                                .into_any_element(),
                        );
                        rows.push(dialog_section_header("From ~/.ssh/config"));
                        if self.openssh_hosts.is_empty() {
                            rows.push(dialog_static("no host aliases found", Color::Muted));
                        }
                        for (index, host) in self.openssh_hosts.iter().enumerate() {
                            let is_active =
                                self.ssh_config_name == format!("openssh:{}", host.alias);
                            rows.push(
                                h_flex()
                                    .id(("dp-openssh-row", index))
                                    .w_full()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(is_active, |this| this.bg(colors.element_selected))
                                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                                    .child(dialog_badge("ssh", Color::Accent, cx))
                                    .child(
                                        Label::new(host.alias.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(openssh_user_host_text(host))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.select_openssh_config(index, window, cx);
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                    }
                    if let Some(hint) = self.ssh_config_missing.clone() {
                        rows.push(dialog_static(hint, Color::Warning));
                    }
                    h_flex()
                        .w_full()
                        .flex_none()
                        .flex_col()
                        .gap_0p5()
                        .children(rows)
                        .into_any_element()
                }
                DeployField::HostPort => dialog_row(
                    "Host:",
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(dialog_editor(
                            "deploy-host-editor",
                            &self.host_editor,
                            None,
                            window,
                            cx,
                        ))
                        .child(dialog_label("Port:"))
                        .child(dialog_editor(
                            "deploy-port-editor",
                            &self.port_editor,
                            Some(px(64.)),
                            window,
                            cx,
                        )),
                ),
                DeployField::UserAnonymous => dialog_row(
                    "Username:",
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(dialog_editor(
                            "deploy-user-editor",
                            &self.user_editor,
                            None,
                            window,
                            cx,
                        ))
                        .child(
                            dialog_toggle_row(
                                "deploy-anon",
                                anonymous_state,
                                "Login as anonymous",
                                cx,
                            )
                            .flex_none()
                            .track_focus(&dialog_slot(&focus, DeployFocusSlot::Anonymous))
                            .focus(|style| style.bg(colors.element_hover))
                            .on_key_down(cx.listener(
                                |this, event: &KeyDownEvent, window, cx| {
                                    if dialog_activation_key(event) {
                                        cx.stop_propagation();
                                        this.toggle_anonymous(window, cx);
                                    }
                                },
                            ))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.toggle_anonymous(window, cx);
                            })),
                        ),
                ),
                DeployField::Password => h_flex()
                    .w_full()
                    .flex_none()
                    .flex_col()
                    .gap_0p5()
                    .child(dialog_row(
                        "Password:",
                        dialog_editor(
                            "deploy-password-editor",
                            &self.password_editor,
                            None,
                            window,
                            cx,
                        ),
                    ))
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .pl(DIALOG_CONTROL_INDENT)
                            .child(dialog_static(self.save_password.hint(), Color::Muted))
                            .child(
                                dialog_button(
                                    "deploy-reset-password",
                                    "Reset",
                                    DialogButtonVariant::Link,
                                )
                                .track_focus(&dialog_slot(&focus, DeployFocusSlot::PasswordReset))
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.reset_password(window, cx);
                                })),
                            ),
                    )
                    .into_any_element(),
                DeployField::Folder => dialog_row(
                    "Folder:",
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(dialog_editor(
                            "deploy-local-root-editor",
                            &self.local_root_editor,
                            None,
                            window,
                            cx,
                        ))
                        .child(
                            IconButton::new("dp-local-browse", IconName::Folder)
                                .shape(IconButtonShape::Square)
                                .track_focus(&dialog_slot(&focus, DeployFocusSlot::LocalRootBrowse))
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Choose folder"))
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.open_root_picker(cx);
                                })),
                        ),
                ),
                DeployField::RootPath => h_flex()
                    .w_full()
                    .flex_none()
                    .flex_col()
                    .gap_0p5()
                    .child(dialog_row(
                        "Root path:",
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(dialog_editor(
                                "deploy-root-editor",
                                &self.remote_root_editor,
                                None,
                                window,
                                cx,
                            ))
                            .child(
                                IconButton::new("dp-root-browse", IconName::Folder)
                                    .shape(IconButtonShape::Square)
                                    .track_focus(&dialog_slot(&focus, DeployFocusSlot::RemoteRootBrowse))
                                    .icon_size(IconSize::Small)
                                    .icon_color(Color::Muted)
                                    .tooltip(Tooltip::text("Choose root path"))
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _window, cx| {
                                            this.open_root_picker(cx);
                                        },
                                    )),
                            )
                            .child(
                                IconButton::new("dp-root-help", IconName::CircleHelp)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::Small)
                                    .icon_color(Color::Muted)
                                    .tooltip(Tooltip::text(
                                        "The deployment root is the server path that mirrors the local project root",
                                    )),
                            )
                            .child(
                                dialog_button(
                                    "deploy-autodetect",
                                    "Autodetect",
                                    DialogButtonVariant::Secondary,
                                )
                                .track_focus(&dialog_slot(&focus, DeployFocusSlot::Autodetect))
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.run_autodetect(window, cx);
                                    },
                                )),
                            ),
                    ))
                    .children(self.autodetect_status.as_ref().map(|(text, color)| {
                        dialog_static(text.clone(), *color)
                    }))
                    .into_any_element(),
                DeployField::WebUrl => dialog_row(
                    "Web server URL:",
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .child(dialog_editor(
                            "deploy-url-editor",
                            &self.web_url_editor,
                            None,
                            window,
                            cx,
                        ))
                        .child(
                            Icon::new(IconName::Public)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        ),
                ),
                DeployField::TestConnection => {
                    let test_button = div()
                        .flex_none()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if dialog_activation_key(event) {
                                cx.stop_propagation();
                                this.test_connection(cx);
                            }
                        }))
                        .child(
                            dialog_button(
                                "deploy-test",
                                "Test Connection",
                                DialogButtonVariant::Link,
                            )
                            .track_focus(&dialog_slot(&focus, DeployFocusSlot::TestConnection))
                            .disabled(local_folder_empty)
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.test_connection(cx);
                            })),
                        );
                    match message.as_ref() {
                        Some((text, color)) => h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .px_1()
                            .py_1()
                            .child(test_button)
                            .child(
                                Label::new(text.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(*color)
                                    .flex_1()
                                    .truncate(),
                            )
                            .into_any_element(),
                        None => h_flex()
                            .flex_none()
                            .items_center()
                            .px_1()
                            .py_1()
                            .child(test_button)
                            .into_any_element(),
                    }
                }
                DeployField::RsyncToggle => h_flex()
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(
                        dialog_toggle_row(
                            "deploy-rsync",
                            rsync_state,
                            "Use rsync for download/upload/sync",
                            cx,
                        )
                        .flex_none()
                        .track_focus(&dialog_slot(&focus, DeployFocusSlot::Rsync))
                        .focus(|style| style.bg(colors.element_hover))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if dialog_activation_key(event) {
                                cx.stop_propagation();
                                this.toggle_rsync(cx);
                            }
                        }))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_rsync(cx);
                        })),
                    )
                    .child(
                        Button::new("deploy-rsync-settings", "Rsync Settings")
                            .track_focus(&dialog_slot(&focus, DeployFocusSlot::RsyncSettings))
                            .style(ButtonStyle::Subtle)
                            .color(Color::Accent)
                            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .tooltip(Tooltip::text("rsync uses ssh defaults")),
                    )
                    .into_any_element(),
                DeployField::SudoToggle => dialog_toggle_row(
                    "deploy-sudo",
                    sudo_state,
                    "Use sudo to run SFTP server",
                    cx,
                )
                .w_full()
                .track_focus(&dialog_slot(&focus, DeployFocusSlot::Sudo))
                .focus(|style| style.bg(colors.element_hover))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    if dialog_activation_key(event) {
                        cx.stop_propagation();
                        this.toggle_sudo(cx);
                    }
                }))
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.toggle_sudo(cx);
                }))
                .into_any_element(),
            });
        }

        let selected_name = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.name.clone());
        let selected_warnings: Vec<String> = self
            .validate_warnings
            .iter()
            .filter(|(name, _)| name.is_empty() || Some(name) == selected_name.as_ref())
            .flat_map(|(_, warnings)| warnings.iter().cloned())
            .collect();
        let warning_lines = selected_warnings
            .iter()
            .map(|warning| {
                h_flex()
                    .flex_none()
                    .px_1()
                    .py_0p5()
                    .child(
                        Label::new(format!("⚠ {warning}"))
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Warning)
                            .truncate(),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let tab_content: AnyElement = match self.tab {
            DeployTab::Connection => v_flex()
                .id("deploy-tab-connection")
                .w_full()
                .children({
                    let mut rows: Vec<AnyElement> = Vec::new();
                    rows.push(
                        dialog_toggle_row(
                            "deploy-visible",
                            visible_state,
                            "Visible only for this project",
                            cx,
                        )
                        .w_full()
                        .track_focus(&dialog_slot(&focus, DeployFocusSlot::Visibility))
                        .focus(|style| style.bg(colors.element_hover))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if dialog_activation_key(event) {
                                cx.stop_propagation();
                                this.toggle_visible(cx);
                            }
                        }))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_visible(cx);
                        }))
                        .into_any_element(),
                    );
                    rows.push(dialog_row("Type:", type_picker));
                    if self.type_menu_open {
                        let mut menu_rows = Vec::new();
                        for (menu_index, (canonical, label)) in
                            deploy_type_options().iter().enumerate()
                        {
                            let is_current = *canonical == protocol;
                            menu_rows.push(
                                h_flex()
                                    .id(("deploy-type-row", menu_index))
                                    .w_full()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(is_current, |this| this.bg(colors.element_selected))
                                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                                    .child(dialog_badge(
                                        deploy_type_badge_short(canonical),
                                        deploy_type_badge_color(canonical),
                                        cx,
                                    ))
                                    .child(
                                        Label::new(*label)
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(if is_current {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.select_deploy_type(canonical, window, cx);
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                        rows.push(
                            v_flex()
                                .id("deploy-type-menu")
                                .w_full()
                                .flex_none()
                                .gap_0p5()
                                .px_1()
                                .py_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.elevated_surface_background)
                                .shadow_sm()
                                .children(menu_rows)
                                .into_any_element(),
                        );
                    }
                    rows.extend(connection_rows);
                    rows.extend(warning_lines);
                    if deploy_type_has_advanced(&self.protocol) {
                        rows.push(
                            h_flex()
                                .id("deploy-advanced-toggle")
                                .w_full()
                                .flex_none()
                                .items_center()
                                .gap_1()
                                .px_1()
                                .py_0p5()
                                .min_h(px(24.))
                                .rounded_sm()
                                .cursor_pointer()
                                .track_focus(&dialog_slot(&focus, DeployFocusSlot::Advanced))
                                .focus(|style| style.bg(colors.element_hover))
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .on_key_down(cx.listener(
                                    |this, event: &KeyDownEvent, _window, cx| {
                                        if dialog_activation_key(event) {
                                            cx.stop_propagation();
                                            this.toggle_advanced(cx);
                                        }
                                    },
                                ))
                                .child(
                                    Label::new(if self.advanced_open {
                                        "▾ Advanced"
                                    } else {
                                        "▸ Advanced"
                                    })
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Default),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.toggle_advanced(cx);
                                }))
                                .into_any_element(),
                        );
                        if self.advanced_open {
                            let selected = self
                                .selected
                                .and_then(|index| self.servers.get(index))
                                .cloned();
                            rows.extend(self.render_advanced_rows(selected.as_ref(), window, cx));
                        }
                    }
                    rows
                })
                .into_any_element(),
            DeployTab::Mappings => v_flex()
                .id("deploy-tab-mappings")
                .w_full()
                .children({
                    let [local_label, deploy_label, web_label] = deploy_mapping_labels();
                    vec![
                        dialog_row(
                            local_label,
                            dialog_editor(
                                "deploy-local-root-editor",
                                &self.local_root_editor,
                                None,
                                window,
                                cx,
                            ),
                        ),
                        dialog_row(
                            deploy_label,
                            dialog_editor(
                                "deploy-deploy-path-editor",
                                &self.remote_root_editor,
                                None,
                                window,
                                cx,
                            ),
                        ),
                        dialog_row(
                            web_label,
                            dialog_editor(
                                "deploy-web-path-editor",
                                &self.web_url_editor,
                                None,
                                window,
                                cx,
                            ),
                        ),
                    ]
                })
                .into_any_element(),
            DeployTab::Excluded => v_flex()
                .id("deploy-tab-excluded")
                .w_full()
                .gap_1()
                .child(dialog_static(
                    "excluded paths, comma separated",
                    Color::Muted,
                ))
                .child(dialog_editor(
                    "deploy-excluded-editor",
                    &self.excluded_editor,
                    None,
                    window,
                    cx,
                ))
                .into_any_element(),
        };

        let message_line: Option<AnyElement> = message.as_ref().map(|(text, color)| {
            h_flex()
                .flex_none()
                .px_3()
                .py_1()
                .child(
                    Label::new(text.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(*color)
                        .truncate(),
                )
                .into_any_element()
        });

        v_flex()
            .id("deploy-dialog")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .key_context("DeploymentDialog")
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
            }))
            .on_action(cx.listener(Self::focus_next_field))
            .on_action(cx.listener(Self::focus_previous_field))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" && (this.type_menu_open || this.ssh_menu_open) {
                    this.type_menu_open = false;
                    this.ssh_menu_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(
                        Label::new("Deployment")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Default),
                    )
                    .child(
                        IconButton::new("deploy-dialog-close", IconName::Close)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.cancel(window, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        v_flex()
                            .w(px(195.))
                            .flex_none()
                            .h_full()
                            .border_r_1()
                            .border_color(colors.border)
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_0p5()
                                    .px_1()
                                    .py_1()
                                    .child(
                                        IconButton::new("deploy-add", IconName::Plus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Add server"))
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.add_server(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("deploy-remove", IconName::SquareMinus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Remove server"))
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.remove_server(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("deploy-copy", IconName::Copy)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Duplicate server"))
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.duplicate_server(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("deploy-check", IconName::Check)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Validate configuration"))
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _window, cx| {
                                                    this.schedule_validate(cx);
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id("deploy-server-list")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .children(server_rows),
                            )
                            .child(
                                v_flex()
                                    .id("deploy-problems")
                                    .flex_none()
                                    .max_h(px(120.))
                                    .border_t_1()
                                    .border_color(colors.border)
                                    .px_1()
                                    .py_1()
                                    .gap_0p5()
                                    .child(
                                        Label::new("Problems")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    )
                                    .children(selected_warnings.iter().map(|warning| {
                                        Label::new(format!("⚠ {warning}"))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Warning)
                                            .into_any_element()
                                    })),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .px_2()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .children(tab_rows),
                            )
                            .child(
                                div()
                                    .id("deploy-tab-content")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_2()
                                    .child(tab_content),
                            ),
                    ),
            )
            .children(message_line)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        IconButton::new("deploy-help", IconName::CircleHelp)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Deployment help")),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.cancel(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "deploy-cancel",
                                            "Cancel",
                                            DialogButtonVariant::Secondary,
                                        )
                                        .track_focus(&dialog_slot(&focus, DeployFocusSlot::Cancel))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.cancel(window, cx);
                                            },
                                        )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.confirm(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "deploy-ok",
                                            "OK",
                                            DialogButtonVariant::Primary,
                                        )
                                        .track_focus(&dialog_slot(&focus, DeployFocusSlot::Ok))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.confirm(window, cx);
                                            },
                                        )),
                                    ),
                            ),
                    ),
            )
    }
}

fn load_deploy_servers(root: &Path) -> Vec<RemoteServer> {
    let Some(config) = read_json(&root.join(".zed/deploy.json")) else {
        return Vec::new();
    };
    let Some(servers) = config.get("servers").and_then(Value::as_array) else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|server| parse_deploy_server(server, root))
        .collect()
}

pub fn open_deployment_window(root: PathBuf, cx: &mut App) {
    register_dialog_key_bindings(cx, "DeploymentDialog");
    let config = read_json(&root.join(".zed/deploy.json"));
    let names = load_deploy_servers(&root)
        .iter()
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    let configured_active = config
        .as_ref()
        .and_then(|config| config.get("active"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let configured_default = config
        .as_ref()
        .and_then(|config| config.get("default"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let panel = if cx.has_global::<DeployShared>() {
        cx.global::<DeployShared>().panel.clone()
    } else {
        None
    };
    cx.set_global(DeployShared {
        servers: names.clone(),
        active: resolve_active_server(configured_active, configured_default, &names),
        panel,
        root: Some(root.clone()),
    });

    if !cx.has_global::<DeployDialogWindow>() {
        cx.set_global(DeployDialogWindow(None));
    }
    let existing = cx.global::<DeployDialogWindow>().0;
    if let Some(handle) = existing.and_then(|any| any.downcast::<DeploymentDialog>()) {
        let resumed = handle.update(cx, |dialog, window, cx| {
            dialog.resume_with_root(root.clone(), window, cx);
            window.activate_window();
            cx.notify();
        });
        if resumed.is_ok() {
            return;
        }
        cx.global_mut::<DeployDialogWindow>().0 = None;
    }

    let window_bounds = dialog_bounds("deployment", size(px(660.), px(470.)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("Deployment")),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Deployment");
                let view = cx.new(|cx| DeploymentDialog::new(root.clone(), window, cx));
                window.activate_window();
                view.focus_handle(cx).focus(window, cx);
                view
            },
        )
        .log_err();
    if let Some(handle) = handle {
        cx.global_mut::<DeployDialogWindow>().0 = Some(handle.into());
    }
}

fn deploy_copy_name(servers: &[RemoteServer], base: &str, start: usize) -> (String, usize) {
    let mut number = start.max(1);
    loop {
        let candidate = format!("{base}{number}");
        if !servers.iter().any(|server| server.name == candidate) {
            return (candidate, number + 1);
        }
        number += 1;
    }
}

fn dialog_next_number(servers: &[RemoteServer]) -> usize {
    let mut next = 1;
    for server in servers {
        if let Some(suffix) = server.name.strip_prefix("server") {
            if let Ok(number) = suffix.parse::<usize>() {
                next = next.max(number + 1);
            }
        }
    }
    next
}

pub struct DeployShared {
    pub servers: Vec<String>,
    pub active: Option<String>,
    pub panel: Option<WeakEntity<PhpPanel>>,
    pub root: Option<PathBuf>,
}

impl gpui::Global for DeployShared {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DataSourceTab {
    General,
    Options,
    Ssh,
    Schemas,
    Advanced,
}

impl DataSourceTab {
    const ALL: [DataSourceTab; 5] = [
        DataSourceTab::General,
        DataSourceTab::Options,
        DataSourceTab::Ssh,
        DataSourceTab::Schemas,
        DataSourceTab::Advanced,
    ];

    fn label(self) -> &'static str {
        match self {
            DataSourceTab::General => "General",
            DataSourceTab::Options => "Options",
            DataSourceTab::Ssh => "SSH,SSL",
            DataSourceTab::Schemas => "Schemas",
            DataSourceTab::Advanced => "Advanced",
        }
    }
}

#[derive(Clone)]
struct DataSource {
    name: String,
    driver: String,
    path: String,
    host: String,
    port: String,
    user: String,
    password: String,
    database: String,
    url: String,
    read_only: bool,
    ssh_enabled: bool,
    ssh_name: String,
    ssh_config: Option<String>,
    ssh_user: String,
    ssh_host: String,
    ssh_port: String,
    ssh_local_port: String,
    excluded_schemas: Vec<String>,
    options: Vec<(String, String)>,
    root: PathBuf,
}

impl DataSource {
    fn blank(root: PathBuf) -> Self {
        Self {
            name: String::new(),
            driver: "sqlite".to_string(),
            path: String::new(),
            host: String::new(),
            port: String::new(),
            user: String::new(),
            password: String::new(),
            database: String::new(),
            url: String::new(),
            read_only: false,
            ssh_enabled: false,
            ssh_name: String::new(),
            ssh_config: None,
            ssh_user: String::new(),
            ssh_host: String::new(),
            ssh_port: String::new(),
            ssh_local_port: String::new(),
            excluded_schemas: Vec::new(),
            options: Vec::new(),
            root,
        }
    }

    fn is_sqlite(&self) -> bool {
        self.driver == "sqlite"
    }
}

const DATA_SOURCE_MANAGED_KEYS: [&str; 13] = [
    "name",
    "driver",
    "path",
    "url",
    "host",
    "port",
    "user",
    "database",
    "password",
    "read_only",
    "ssh_tunnel",
    "excluded_schemas",
    "options",
];

fn parse_data_source(value: &Value, root: &Path) -> Option<DataSource> {
    let name = string_field(value, "name")?;
    let mut source = DataSource::blank(root.to_path_buf());
    source.name = name;
    if let Some(driver) = string_field(value, "driver") {
        source.driver = driver.to_ascii_lowercase();
    }
    source.path = string_field(value, "path").unwrap_or_default();
    source.user = string_field(value, "user").unwrap_or_default();
    source.database = string_field(value, "database").unwrap_or_default();
    source.url = string_field(value, "url").unwrap_or_default();
    source.password = string_field(value, "password").unwrap_or_default();
    source.host = string_field(value, "host").unwrap_or_default();
    source.port = match value.get("port") {
        Some(Value::Number(port)) => port.to_string(),
        Some(Value::String(port)) => port.clone(),
        _ => String::new(),
    };
    source.read_only = value
        .get("read_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(tunnel) = value.get("ssh_tunnel") {
        source.ssh_enabled = tunnel
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        source.ssh_name = string_field(tunnel, "name").unwrap_or_default();
        source.ssh_host = string_field(tunnel, "host").unwrap_or_default();
        source.ssh_user = string_field(tunnel, "user").unwrap_or_default();
        source.ssh_port = match tunnel.get("port") {
            Some(Value::Number(port)) => port.to_string(),
            Some(Value::String(port)) => port.clone(),
            _ => String::new(),
        };
        source.ssh_local_port = match tunnel.get("local_port") {
            Some(Value::Number(port)) => port.to_string(),
            Some(Value::String(port)) => port.clone(),
            _ => String::new(),
        };
    }
    source.excluded_schemas = parse_excluded_schemas(value.get("excluded_schemas"));
    let mut options = value
        .get("options")
        .and_then(Value::as_object)
        .map(options_to_rows)
        .unwrap_or_default();
    // php-db rejects unknown top-level/tunnel fields but keeps and ignores
    // unknown `options` entries, so the OpenSSH selection marker lives there.
    if let Some(position) = options.iter().position(|(key, _)| key == "ssh_config") {
        let (_, marker) = options.remove(position);
        let marker = marker.trim();
        if marker.starts_with("openssh:") && marker.len() > "openssh:".len() {
            source.ssh_config = Some(marker.to_string());
        }
    }
    source.options = options;
    Some(source)
}

fn data_source_entry(source: &DataSource) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "name".to_string(),
        Value::String(source.name.trim().to_string()),
    );
    entry.insert("driver".to_string(), Value::String(source.driver.clone()));
    if source.is_sqlite() {
        if !source.path.trim().is_empty() {
            entry.insert(
                "path".to_string(),
                Value::String(source.path.trim().to_string()),
            );
        }
    } else {
        let url = source.url.trim().to_string();
        if !url.is_empty() {
            entry.insert("url".to_string(), Value::String(url));
        } else {
            if !source.host.trim().is_empty() {
                entry.insert(
                    "host".to_string(),
                    Value::String(source.host.trim().to_string()),
                );
            }
            let port = source.port.trim().to_string();
            if !port.is_empty() {
                match port.parse::<u16>() {
                    Ok(number) => entry.insert("port".to_string(), Value::from(number)),
                    Err(_) => entry.insert("port".to_string(), Value::String(port)),
                };
            }
            if !source.user.trim().is_empty() {
                entry.insert(
                    "user".to_string(),
                    Value::String(source.user.trim().to_string()),
                );
            }
            if !source.database.trim().is_empty() {
                entry.insert(
                    "database".to_string(),
                    Value::String(source.database.trim().to_string()),
                );
            }
        }
    }
    if !source.password.is_empty() {
        entry.insert(
            "password".to_string(),
            Value::String(source.password.clone()),
        );
    }
    if source.read_only {
        entry.insert("read_only".to_string(), Value::Bool(true));
    }
    if source.ssh_enabled {
        let mut tunnel = serde_json::Map::new();
        tunnel.insert("enabled".to_string(), Value::Bool(true));
        if !source.ssh_local_port.trim().is_empty() {
            let local_port = source.ssh_local_port.trim().to_string();
            match local_port.parse::<u16>() {
                Ok(number) => tunnel.insert("local_port".to_string(), Value::from(number)),
                Err(_) => tunnel.insert("local_port".to_string(), Value::String(local_port)),
            };
        }
        entry.insert("ssh_tunnel".to_string(), Value::Object(tunnel));
    }
    if !source.excluded_schemas.is_empty() {
        entry.insert(
            "excluded_schemas".to_string(),
            Value::Array(
                source
                    .excluded_schemas
                    .iter()
                    .map(|name| Value::String(name.clone()))
                    .collect(),
            ),
        );
    }
    let mut options = rows_to_options(&source.options);
    if let Some(marker) = source
        .ssh_config
        .as_deref()
        .map(str::trim)
        .filter(|marker| !marker.is_empty())
    {
        options.insert("ssh_config".to_string(), Value::String(marker.to_string()));
    }
    if !options.is_empty() {
        entry.insert("options".to_string(), Value::Object(options));
    }
    Value::Object(entry)
}

fn data_source_test_config(source: &DataSource) -> Value {
    let mut entry = match data_source_entry(source) {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    entry.insert("name".to_string(), Value::String("_test".to_string()));
    // inject the staged config NAME so resolve_ssh_tunnel_into can find it
    if source.ssh_enabled && !source.ssh_name.trim().is_empty() {
        if let Some(tunnel) = entry.get_mut("ssh_tunnel").and_then(Value::as_object_mut) {
            tunnel.insert(
                "name".to_string(),
                Value::String(source.ssh_name.trim().to_string()),
            );
        }
    }
    serde_json::json!({ "connections": [Value::Object(entry)] })
}

fn save_data_sources(root: &Path, sources: &[DataSource]) -> std::result::Result<(), String> {
    for source in sources {
        if source.name.trim().is_empty() {
            return Err("data source name is required".to_string());
        }
    }
    let mut seen = HashSet::new();
    for source in sources {
        let name = source.name.trim().to_string();
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate data source name: {name}"));
        }
    }
    let path = root.join(".zed/databases.json");
    let mut config = load_config_object(&path)?;
    let existing: Vec<Value> = match config.remove("connections") {
        Some(Value::Array(connections)) => connections,
        Some(_) => return Err("connections in databases.json is not an array".to_string()),
        None => Vec::new(),
    };
    let ssh_configs = load_ssh_configs(root);
    let mut connections = Vec::new();
    for source in sources {
        let staged = match data_source_entry(source) {
            Value::Object(fields) => fields,
            _ => serde_json::Map::new(),
        };
        let mut merged = existing
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(source.name.trim()))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, value) in &staged {
            merged.insert(key.clone(), value.clone());
        }
        for key in DATA_SOURCE_MANAGED_KEYS {
            if !staged.contains_key(key) {
                merged.remove(key);
            }
        }
        let mut merged = Value::Object(merged);
        if source.ssh_enabled && !source.ssh_name.trim().is_empty() {
            if let Some(tunnel) = merged.get_mut("ssh_tunnel").and_then(Value::as_object_mut) {
                tunnel.insert(
                    "name".to_string(),
                    Value::String(source.ssh_name.trim().to_string()),
                );
            }
        }
        if let Err(message) = resolve_ssh_tunnel_into(&mut merged, &ssh_configs) {
            return Err(format!("{}: {}", source.name.trim(), message));
        }
        let merged = match merged {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        connections.push(Value::Object(merged));
    }
    config.insert("connections".to_string(), Value::Array(connections));
    write_config_object(&path, &config)
}

/// Removes the connection named `name` from a `connections` JSON array,
/// preserving every other entry verbatim. Returns whether anything was removed.
fn remove_connection(connections: &mut Vec<Value>, name: &str) -> bool {
    let name = name.trim();
    let before = connections.len();
    connections.retain(|entry| entry.get("name").and_then(Value::as_str) != Some(name));
    connections.len() != before
}

/// Deletes a data source from `.zed/databases.json`, keeping all other
/// connections and top-level config keys untouched.
fn delete_data_source(root: &Path, name: &str) -> std::result::Result<bool, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("data source name is required".to_string());
    }
    let path = root.join(".zed/databases.json");
    let mut config = load_config_object(&path)?;
    let mut connections = match config.remove("connections") {
        Some(Value::Array(connections)) => connections,
        Some(_) => return Err("connections in databases.json is not an array".to_string()),
        None => Vec::new(),
    };
    let removed = remove_connection(&mut connections, name);
    config.insert("connections".to_string(), Value::Array(connections));
    write_config_object(&path, &config)?;
    Ok(removed)
}

fn load_data_sources(root: &Path) -> Vec<DataSource> {
    read_json(&root.join(".zed/databases.json"))
        .and_then(|config| config.get("connections").and_then(Value::as_array).cloned())
        .map(|connections| {
            connections
                .iter()
                .filter_map(|connection| parse_data_source(connection, root))
                .collect()
        })
        .unwrap_or_default()
}

fn mask_password(password: &str) -> &'static str {
    if password.is_empty() {
        ""
    } else {
        "•••••"
    }
}

fn driver_support_hint(driver: &str) -> &'static str {
    match driver {
        "postgres" => "supports 9.x – 17",
        "mysql" => "supports 5.1 – 8.4",
        "mariadb" => "supports 10.x – 11.x",
        "sqlite" => "supports SQLite 3.x",
        "sqlserver" => "supports 2012 – 2022",
        "oracle" => "supports 12c – 23ai",
        "mongodb" => "supports 4.4 – 8.0",
        "redis" => "supports 5.0 – 7.4",
        _ => "",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SshAuthMode {
    Password,
    Key,
    Agent,
    Config,
}

impl SshAuthMode {
    fn label(self) -> &'static str {
        match self {
            SshAuthMode::Password => "Password",
            SshAuthMode::Key => "Key pair",
            SshAuthMode::Agent => "OpenSSH config and authentication agent",
            SshAuthMode::Config => "OpenSSH config file (host alias)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            SshAuthMode::Password => "password",
            SshAuthMode::Key => "key",
            SshAuthMode::Agent => "agent",
            SshAuthMode::Config => "config",
        }
    }

    fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "key" | "key pair" => SshAuthMode::Key,
            "agent" | "openssh" | "openssh config and authentication agent" => SshAuthMode::Agent,
            "config"
            | "alias"
            | "host alias"
            | "openssh config file"
            | "openssh config file (host alias)" => SshAuthMode::Config,
            _ => SshAuthMode::Password,
        }
    }
}

/// Authentication modes in the exact order the dropdown lists them.
const SSH_AUTH_OPTIONS: [SshAuthMode; 4] = [
    SshAuthMode::Password,
    SshAuthMode::Key,
    SshAuthMode::Agent,
    SshAuthMode::Config,
];

/// A concrete `Host <alias>` entry parsed from an OpenSSH client config.
///
/// Wildcard (`*`, `?`) and negated (`!`) patterns are skipped: only aliases
/// the user can type verbatim are surfaced. `host_name` defaults to the alias
/// (PhpStorm-like) when the block has no `HostName` directive.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenSshHost {
    alias: String,
    host_name: String,
    user: Option<String>,
    port: Option<String>,
    identity_file: Option<String>,
    proxy_jump: Option<String>,
    proxy_command: Option<String>,
}

/// Environment override used by tests and e2e runs. When unset the parser
/// reads `$HOME/.ssh/config`.
const OPENSSH_CONFIG_ENV: &str = "PHPWORKBENCH_SSH_CONFIG";

/// Nested `Include` directives are followed at most this many levels deep.
const OPENSSH_INCLUDE_MAX_DEPTH: usize = 2;

fn openssh_config_path() -> Option<PathBuf> {
    if let Ok(value) = std::env::var(OPENSSH_CONFIG_ENV) {
        let value = value.trim();
        if !value.is_empty() {
            return Some(PathBuf::from(value));
        }
    }
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty())?;
    Some(PathBuf::from(home).join(".ssh").join("config"))
}

fn openssh_config_display_path() -> String {
    let Some(path) = openssh_config_path() else {
        return "~/.ssh/config".to_string();
    };
    if let Ok(home) = std::env::var("HOME") {
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = rest.to_string_lossy();
            return format!("~/{}", rest.trim_start_matches('/'));
        }
    }
    path.to_string_lossy().to_string()
}

fn expand_home_path(value: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return value.to_string();
    };
    if value == "~" {
        return home.to_string();
    }
    match value.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None => value.to_string(),
    }
}

/// Splits an OpenSSH directive value into whitespace-separated tokens while
/// honouring single/double quotes (`IdentityFile "~/.ssh/my key"`).
fn tokenize_openssh_value(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for character in value.chars() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                } else {
                    current.push(character);
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    quote = Some(character);
                    has_token = true;
                } else if character.is_whitespace() {
                    if has_token || !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                        has_token = false;
                    }
                } else {
                    current.push(character);
                    has_token = true;
                }
            }
        }
    }
    if has_token || !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn openssh_is_concrete_alias(pattern: &str) -> bool {
    let pattern = pattern.trim();
    !pattern.is_empty()
        && !pattern.starts_with('!')
        && !pattern.contains('*')
        && !pattern.contains('?')
}

/// Simple `*`/`?` matcher used for `Include` globs. No character classes or
/// brace expansion: the goal is to find sibling config fragments, not to
/// reimplement `glob(3)`.
fn openssh_wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut star_match) = (None, 0usize);
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            star_match = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            star_match += 1;
            ti = star_match;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

/// Resolves an `Include` directive relative to the including file's directory.
/// Only regular files one directory level deep are considered (no recursion
/// into subdirectories) and the returned list is sorted for determinism.
fn openssh_include_paths(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let path = Path::new(pattern);
    let file_pattern = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(pattern)
        .to_string();
    let has_glob = file_pattern.contains('*') || file_pattern.contains('?');
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            if parent.is_absolute() {
                parent.to_path_buf()
            } else {
                base.join(parent)
            }
        }
        _ => base.to_path_buf(),
    };

    if !has_glob {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            base.join(path)
        };
        return if candidate.is_file() {
            vec![candidate]
        } else {
            Vec::new()
        };
    }

    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut matches: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_file())
                .unwrap_or(false)
        })
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| openssh_wildcard_match(&file_pattern, name))
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect();
    matches.sort();
    matches
}

/// Pure `parse_openssh_config(text)` entry point. `Include` directives are
/// ignored here because there is no base directory to resolve them against;
/// use [`load_openssh_hosts_from`] for file-backed parsing with includes.
fn parse_openssh_config(text: &str) -> Vec<OpenSshHost> {
    parse_openssh_config_scoped(text, None, 0)
}

fn parse_openssh_config_scoped(text: &str, base: Option<&Path>, depth: usize) -> Vec<OpenSshHost> {
    let mut hosts: Vec<OpenSshHost> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut active: Vec<usize> = Vec::new();
    let home = std::env::var("HOME").ok();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = match line.find(char::is_whitespace) {
            Some(index) => (&line[..index], line[index..].trim()),
            None => (line, ""),
        };
        // Accept `Key = Value` as a convenience; ssh_config itself uses
        // whitespace separators, sshd_config also accepts `=`.
        let rest = rest.strip_prefix('=').map(str::trim_start).unwrap_or(rest);
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                active.clear();
                for pattern in tokenize_openssh_value(rest) {
                    if !openssh_is_concrete_alias(&pattern) {
                        continue;
                    }
                    // OpenSSH applies the first matching value, so the first
                    // block for an alias wins and later blocks are ignored.
                    if seen.insert(pattern.clone()) {
                        hosts.push(OpenSshHost {
                            alias: pattern.clone(),
                            host_name: pattern,
                            user: None,
                            port: None,
                            identity_file: None,
                            proxy_jump: None,
                            proxy_command: None,
                        });
                        active.push(hosts.len() - 1);
                    }
                }
            }
            "hostname" | "user" | "port" | "identityfile" | "proxyjump" | "proxycommand" => {
                if active.is_empty() {
                    continue;
                }
                let Some(value) = tokenize_openssh_value(rest).into_iter().next() else {
                    continue;
                };
                // ProxyCommand is a shell command, not a single token; keep the
                // rest of the line (minus one matching outer quote pair).
                let proxy_command = {
                    let raw = rest.trim();
                    raw.strip_prefix('"')
                        .and_then(|inner| inner.strip_suffix('"'))
                        .or_else(|| {
                            raw.strip_prefix('\'')
                                .and_then(|inner| inner.strip_suffix('\''))
                        })
                        .unwrap_or(raw)
                        .to_string()
                };
                for index in &active {
                    let host = &mut hosts[*index];
                    match keyword.to_ascii_lowercase().as_str() {
                        "hostname" => host.host_name = value.clone(),
                        "user" => host.user = Some(value.clone()),
                        "port" => host.port = Some(value.clone()),
                        "identityfile" => {
                            host.identity_file = Some(expand_home_path(&value, home.as_deref()));
                        }
                        "proxyjump" => host.proxy_jump = Some(value.clone()),
                        "proxycommand" => host.proxy_command = Some(proxy_command.clone()),
                        _ => {}
                    }
                }
            }
            "include" => {
                let Some(base) = base else {
                    continue;
                };
                if depth >= OPENSSH_INCLUDE_MAX_DEPTH {
                    continue;
                }
                for pattern in tokenize_openssh_value(rest) {
                    let expanded = expand_home_path(&pattern, home.as_deref());
                    for path in openssh_include_paths(base, &expanded) {
                        let Ok(included) = std::fs::read_to_string(&path) else {
                            continue;
                        };
                        let nested =
                            parse_openssh_config_scoped(&included, path.parent(), depth + 1);
                        for host in nested {
                            if seen.insert(host.alias.clone()) {
                                hosts.push(host);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    hosts
}

fn load_openssh_hosts_from(path: &Path) -> Vec<OpenSshHost> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_openssh_config_scoped(&text, path.parent(), 0)
}

fn load_openssh_hosts() -> Vec<OpenSshHost> {
    openssh_config_path()
        .map(|path| load_openssh_hosts_from(&path))
        .unwrap_or_default()
}

fn openssh_missing_alias_message(alias: &str) -> String {
    format!("alias {alias} not found in ssh config")
}

fn openssh_user_host_text(host: &OpenSshHost) -> String {
    let mut text = String::new();
    if let Some(user) = host
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
    {
        text.push_str(user);
        text.push('@');
    }
    text.push_str(host.host_name.trim());
    if let Some(port) = host
        .port
        .as_deref()
        .map(str::trim)
        .filter(|port| !port.is_empty())
    {
        text.push(':');
        text.push_str(port);
    }
    text
}

/// The selector stores the raw marker (`openssh:<alias>`) so it can be
/// resolved back after a restart; the UI only ever shows the alias.
fn ssh_config_display_name(raw: &str) -> String {
    raw.strip_prefix("openssh:")
        .map(str::trim)
        .unwrap_or(raw)
        .to_string()
}

/// Resolves a staged `.zed/ssh.json` entry that uses the `config` auth mode to
/// a concrete entry the ssh probe / engine helpers can use.
fn resolve_openssh_entry(
    config: &SshEntry,
    hosts: &[OpenSshHost],
) -> std::result::Result<SshEntry, String> {
    let alias = config
        .ssh_alias
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .ok_or_else(|| "ssh config alias is required".to_string())?;
    let host = hosts
        .iter()
        .find(|host| host.alias == alias)
        .ok_or_else(|| openssh_missing_alias_message(alias))?;
    let mut resolved = config.clone();
    resolved.host = host.host_name.clone();
    resolved.user = host.user.clone().unwrap_or_default();
    resolved.port = host.port.clone().unwrap_or_else(|| "22".to_string());
    resolved.key_path = host.identity_file.clone().unwrap_or_default();
    resolved.auth = if host
        .identity_file
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .is_some()
    {
        SshAuthMode::Key
    } else {
        SshAuthMode::Agent
    };
    Ok(resolved)
}

#[derive(Clone, Debug, PartialEq)]
struct SshEntry {
    name: String,
    user: String,
    host: String,
    port: String,
    auth: SshAuthMode,
    password: String,
    password_session: bool,
    key_path: String,
    ssh_alias: Option<String>,
    keep_alive: u16,
    strict_host_key: bool,
}

impl SshEntry {
    fn blank() -> Self {
        Self {
            name: String::new(),
            user: String::new(),
            host: String::new(),
            port: "22".to_string(),
            auth: SshAuthMode::Password,
            password: String::new(),
            password_session: false,
            key_path: String::new(),
            ssh_alias: None,
            keep_alive: 15,
            strict_host_key: false,
        }
    }
}

/// Engines (php-db / php-deploy / ssh) only understand concrete auth modes.
/// A `config` entry is resolved from its alias before reaching them, so map it
/// to `key` when an IdentityFile was found and `agent` otherwise.
fn ssh_effective_auth(config: &SshEntry) -> SshAuthMode {
    match config.auth {
        SshAuthMode::Config => {
            if config.key_path.trim().is_empty() {
                SshAuthMode::Agent
            } else {
                SshAuthMode::Key
            }
        }
        other => other,
    }
}

fn ssh_entry_port(config: &SshEntry) -> u16 {
    config.port.trim().parse::<u16>().unwrap_or(22)
}

fn ssh_user_host_text(config: &SshEntry) -> String {
    let mut text = String::new();
    if !config.user.trim().is_empty() {
        text.push_str(config.user.trim());
        text.push('@');
    }
    text.push_str(config.host.trim());
    if !config.port.trim().is_empty() {
        text.push_str(":");
        text.push_str(config.port.trim());
    }
    text
}

fn parse_ssh_entry(value: &Value) -> SshEntry {
    let mut entry = SshEntry::blank();
    entry.name = string_field(value, "name").unwrap_or_default();
    entry.user = string_field(value, "user").unwrap_or_default();
    entry.host = string_field(value, "host").unwrap_or_default();
    entry.port = match value.get("port") {
        Some(Value::Number(port)) => port.to_string(),
        Some(Value::String(port)) => port.clone(),
        _ => String::new(),
    };
    entry.auth = value
        .get("auth")
        .and_then(Value::as_str)
        .map(SshAuthMode::parse)
        .unwrap_or(SshAuthMode::Password);
    entry.password = string_field(value, "password").unwrap_or_default();
    entry.key_path = string_field(value, "key_path").unwrap_or_default();
    entry.ssh_alias = string_field(value, "ssh_alias")
        .map(|alias| alias.trim().to_string())
        .filter(|alias| !alias.is_empty());
    entry.keep_alive = match value.get("keep_alive") {
        Some(Value::Number(seconds)) => seconds
            .as_u64()
            .map(|seconds| seconds.min(u16::MAX as u64) as u16)
            .unwrap_or(15),
        _ => 15,
    };
    entry.strict_host_key = value
        .get("strict_host_key")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    entry
}

fn ssh_store_entry(config: &SshEntry) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "name".to_string(),
        Value::String(config.name.trim().to_string()),
    );
    entry.insert(
        "user".to_string(),
        Value::String(config.user.trim().to_string()),
    );
    entry.insert(
        "host".to_string(),
        Value::String(config.host.trim().to_string()),
    );
    entry.insert("port".to_string(), Value::from(ssh_entry_port(config)));
    entry.insert(
        "auth".to_string(),
        Value::String(config.auth.key().to_string()),
    );
    if !config.password.is_empty() && !config.password_session {
        entry.insert(
            "password".to_string(),
            Value::String(config.password.clone()),
        );
    }
    if !config.key_path.trim().is_empty() {
        entry.insert(
            "key_path".to_string(),
            Value::String(config.key_path.trim().to_string()),
        );
    }
    if let Some(alias) = config
        .ssh_alias
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
    {
        entry.insert("ssh_alias".to_string(), Value::String(alias.to_string()));
    }
    entry.insert("keep_alive".to_string(), Value::from(config.keep_alive));
    entry.insert(
        "strict_host_key".to_string(),
        Value::Bool(config.strict_host_key),
    );
    Value::Object(entry)
}

fn load_ssh_configs(root: &Path) -> Vec<SshEntry> {
    read_json(&root.join(".zed/ssh.json"))
        .and_then(|config| config.get("configs").and_then(Value::as_array).cloned())
        .map(|configs| {
            configs
                .iter()
                .filter(|entry| {
                    entry
                        .get("name")
                        .and_then(Value::as_str)
                        .map(|name| !name.trim().is_empty())
                        .unwrap_or(false)
                })
                .map(parse_ssh_entry)
                .collect()
        })
        .unwrap_or_default()
}

fn save_ssh_configs(root: &Path, configs: &[SshEntry]) -> std::result::Result<(), String> {
    for config in configs {
        if config.name.trim().is_empty() {
            return Err("configuration name is required".to_string());
        }
        if !config
            .port
            .trim()
            .chars()
            .all(|character| character.is_ascii_digit())
            || config.port.trim().parse::<u16>().is_err()
        {
            return Err(format!("{}: wrong port number", config.name.trim()));
        }
        if matches!(config.auth, SshAuthMode::Key) && config.key_path.trim().is_empty() {
            return Err(format!("{}: key path is required", config.name.trim()));
        }
        if matches!(config.auth, SshAuthMode::Config)
            && config
                .ssh_alias
                .as_deref()
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .is_none()
        {
            return Err(format!(
                "{}: ssh config alias is required",
                config.name.trim()
            ));
        }
    }
    let mut seen = HashSet::new();
    for config in configs {
        let name = config.name.trim().to_string();
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate configuration name: {name}"));
        }
    }
    write_config_object(
        &root.join(".zed/ssh.json"),
        &serde_json::json!({
            "configs": configs
                .iter()
                .map(ssh_store_entry)
                .collect::<Vec<_>>()
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    )
}

fn unique_ssh_config_name(base: &str, configs: &[SshEntry]) -> String {
    let mut name = base.to_string();
    let mut index = 0usize;
    while configs.iter().any(|config| config.name.trim() == name) {
        index += 1;
        name = format!("{base} ({index})");
    }
    name
}

fn ssh_apply_auth_to_tunnel(tunnel: &mut serde_json::Map<String, Value>, config: &SshEntry) {
    tunnel.insert(
        "auth".to_string(),
        Value::String(ssh_effective_auth(config).key().to_string()),
    );
    if !config.password.is_empty() {
        tunnel.insert(
            "password".to_string(),
            Value::String(config.password.clone()),
        );
    }
    if !config.key_path.trim().is_empty() {
        tunnel.insert(
            "key_path".to_string(),
            Value::String(config.key_path.trim().to_string()),
        );
    }
    tunnel.insert(
        "strict_host_key".to_string(),
        Value::Bool(config.strict_host_key),
    );
    tunnel.insert("keep_alive".to_string(), Value::from(config.keep_alive));
}

fn ssh_apply_auth_to_server(server: &mut serde_json::Map<String, Value>, config: &SshEntry) {
    server.insert(
        "ssh_auth".to_string(),
        Value::String(ssh_effective_auth(config).key().to_string()),
    );
    if !config.password.is_empty() {
        server.insert(
            "ssh_password".to_string(),
            Value::String(config.password.clone()),
        );
    }
    if !config.key_path.trim().is_empty() {
        server.insert(
            "ssh_key_path".to_string(),
            Value::String(config.key_path.trim().to_string()),
        );
    }
    server.insert(
        "ssh_strict_host_key".to_string(),
        Value::Bool(config.strict_host_key),
    );
    server.insert("ssh_keep_alive".to_string(), Value::from(config.keep_alive));
}

fn openssh_auth(host: &OpenSshHost) -> (&'static str, Option<&str>) {
    match host
        .identity_file
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        Some(key) => ("key", Some(key)),
        None => ("agent", None),
    }
}

fn insert_openssh_endpoint(map: &mut serde_json::Map<String, Value>, host: &OpenSshHost) {
    map.insert(
        "host".to_string(),
        Value::String(host.host_name.trim().to_string()),
    );
    if let Some(user) = host
        .user
        .as_deref()
        .map(str::trim)
        .filter(|user| !user.is_empty())
    {
        map.insert("user".to_string(), Value::String(user.to_string()));
    }
    let port = host
        .port
        .as_deref()
        .and_then(|port| port.trim().parse::<u16>().ok())
        .unwrap_or(22);
    map.insert("port".to_string(), Value::from(port));
}

fn apply_openssh_auth_to_server(server: &mut serde_json::Map<String, Value>, host: &OpenSshHost) {
    let (auth, key_path) = openssh_auth(host);
    server.insert("ssh_auth".to_string(), Value::String(auth.to_string()));
    if let Some(key_path) = key_path {
        server.insert(
            "ssh_key_path".to_string(),
            Value::String(key_path.to_string()),
        );
    }
    server.insert("ssh_strict_host_key".to_string(), Value::Bool(false));
    server.insert("ssh_keep_alive".to_string(), Value::from(15));
}

fn apply_openssh_auth_to_tunnel(tunnel: &mut serde_json::Map<String, Value>, host: &OpenSshHost) {
    let (auth, key_path) = openssh_auth(host);
    tunnel.insert("auth".to_string(), Value::String(auth.to_string()));
    if let Some(key_path) = key_path {
        tunnel.insert("key_path".to_string(), Value::String(key_path.to_string()));
    }
    tunnel.insert("strict_host_key".to_string(), Value::Bool(false));
    tunnel.insert("keep_alive".to_string(), Value::from(15));
}

fn resolve_ssh_into(server: &mut Value, configs: &[SshEntry]) -> std::result::Result<(), String> {
    resolve_ssh_into_with(server, configs, &load_openssh_hosts())
}

fn resolve_ssh_into_with(
    server: &mut Value,
    configs: &[SshEntry],
    openssh: &[OpenSshHost],
) -> std::result::Result<(), String> {
    let Some(name) = string_field(server, "ssh_config").filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    if let Some(alias) = name.strip_prefix("openssh:").map(str::trim) {
        let host = openssh
            .iter()
            .find(|host| host.alias == alias)
            .ok_or_else(|| openssh_missing_alias_message(alias))?;
        let Some(map) = server.as_object_mut() else {
            return Ok(());
        };
        insert_openssh_endpoint(map, host);
        apply_openssh_auth_to_server(map, host);
        return Ok(());
    }
    let Some(config) = configs.iter().find(|config| config.name == name) else {
        return Err(format!("ssh configuration '{name}' not found"));
    };
    let Some(map) = server.as_object_mut() else {
        return Ok(());
    };
    map.insert(
        "host".to_string(),
        Value::String(config.host.trim().to_string()),
    );
    map.insert(
        "user".to_string(),
        Value::String(config.user.trim().to_string()),
    );
    map.insert("port".to_string(), Value::from(ssh_entry_port(config)));
    ssh_apply_auth_to_server(map, config);
    Ok(())
}

fn resolve_ssh_tunnel_into(
    connection: &mut Value,
    configs: &[SshEntry],
) -> std::result::Result<(), String> {
    resolve_ssh_tunnel_into_with(connection, configs, &load_openssh_hosts())
}

fn resolve_ssh_tunnel_into_with(
    connection: &mut Value,
    configs: &[SshEntry],
    openssh: &[OpenSshHost],
) -> std::result::Result<(), String> {
    let Some(Value::Object(tunnel)) = connection.get("ssh_tunnel") else {
        return Ok(());
    };
    let enabled = tunnel
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !enabled {
        return Ok(());
    }
    let name = tunnel
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    // A data source cannot persist an arbitrary top-level `ssh_config` key
    // because php-db denies unknown fields, so the OpenSSH marker rides in the
    // tolerated `options` driver map (php-db keeps and ignores unknown keys).
    let openssh_marker = connection
        .get("options")
        .and_then(Value::as_object)
        .and_then(|options| options.get("ssh_config"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|marker| !marker.is_empty())
        .map(str::to_string);
    let Some(tunnel) = connection
        .get_mut("ssh_tunnel")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    if let Some(marker) = openssh_marker {
        let Some(alias) = marker.strip_prefix("openssh:").map(str::trim) else {
            return Ok(());
        };
        let host = openssh
            .iter()
            .find(|host| host.alias == alias)
            .ok_or_else(|| openssh_missing_alias_message(alias))?;
        tunnel.remove("name");
        insert_openssh_endpoint(tunnel, host);
        apply_openssh_auth_to_tunnel(tunnel, host);
        return Ok(());
    }
    let Some(name) = name else {
        return Ok(());
    };
    let Some(config) = configs.iter().find(|config| config.name == name) else {
        return Err(format!("ssh configuration '{name}' not found"));
    };
    // the marker is transient: php-db denies unknown fields, so strip it now
    // that the resolved host/user/port/auth keys carry the connection data
    tunnel.remove("name");
    tunnel.insert(
        "host".to_string(),
        Value::String(config.host.trim().to_string()),
    );
    tunnel.insert(
        "user".to_string(),
        Value::String(config.user.trim().to_string()),
    );
    tunnel.insert("port".to_string(), Value::from(ssh_entry_port(config)));
    ssh_apply_auth_to_tunnel(tunnel, config);
    Ok(())
}

fn options_to_rows(options: &serde_json::Map<String, Value>) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = options
        .iter()
        .map(|(key, value)| match value {
            Value::String(text) => (key.clone(), text.clone()),
            other => (
                key.clone(),
                serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
            ),
        })
        .collect();
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    rows
}

fn rows_to_options(rows: &[(String, String)]) -> serde_json::Map<String, Value> {
    let mut options = BTreeMap::new();
    for (key, value) in rows {
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        options.insert(key, Value::String(value.trim().to_string()));
    }
    options.into_iter().collect()
}

fn parse_excluded_schemas(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn schema_checked(excluded: &[String], name: &str) -> bool {
    !excluded.iter().any(|known| known == name)
}

fn data_source_problems(source: &DataSource) -> Vec<String> {
    let mut problems = Vec::new();
    if source.is_sqlite() {
        if source.path.trim().is_empty() {
            problems.push("file path is required".to_string());
        }
    } else if source.url.trim().is_empty() {
        if source.host.trim().is_empty() {
            problems.push("host is required".to_string());
        }
        if source.port.trim().is_empty() {
            problems.push("port is required".to_string());
        }
        if source.user.trim().is_empty() {
            problems.push("user is required".to_string());
        }
        if source.database.trim().is_empty() {
            problems.push("database is required".to_string());
        }
    }
    if source.name.trim().is_empty() {
        problems.push("name is required".to_string());
    }
    if source.ssh_enabled && source.ssh_host.trim().is_empty() {
        problems.push("ssh host is required".to_string());
    }
    problems
}

fn ssh_config_problem(source: &DataSource, configs: &[SshEntry]) -> Option<String> {
    if !source.ssh_enabled {
        return None;
    }
    if let Some(alias) = source
        .ssh_config
        .as_deref()
        .and_then(|marker| marker.strip_prefix("openssh:"))
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
    {
        let hosts = load_openssh_hosts();
        if hosts.iter().any(|host| host.alias == alias) {
            return None;
        }
        return Some(openssh_missing_alias_message(alias));
    }
    let name = source.ssh_name.trim();
    if name.is_empty() {
        return None;
    }
    if configs.iter().any(|config| config.name == name) {
        return None;
    }
    Some(format!("ssh configuration '{name}' not found"))
}

fn next_data_source_name(base: &str, sources: &[DataSource]) -> String {
    let mut name = base.to_string();
    let mut index = 0usize;
    while sources.iter().any(|source| source.name == name) {
        index += 1;
        name = format!("{base} ({index})");
    }
    name
}

/// A data source to create when the dialog is opened from the Create Data
/// Source menu. Purely a seed for the editor; nothing is written to disk.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DataSourceSeed {
    driver: &'static str,
    path: Option<String>,
}

impl DataSourceSeed {
    fn driver(driver: &'static str) -> Self {
        Self { driver, path: None }
    }

    fn demo(demo: DemoDatabase) -> Self {
        Self {
            driver: demo.driver(),
            path: match demo {
                DemoDatabase::Sqlite => Some("demo.sqlite".to_string()),
                DemoDatabase::Postgres | DemoDatabase::MySql => None,
            },
        }
    }
}

/// PhpStorm-style numbered names derived from the driver identifier.
fn create_data_source_name(driver: &str, sources: &[DataSource]) -> String {
    let mut index = 1usize;
    loop {
        let candidate = format!("{driver}{index}");
        if !sources.iter().any(|source| source.name == candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn seeded_data_source(
    driver: &str,
    path: Option<&str>,
    root: &Path,
    sources: &[DataSource],
) -> DataSource {
    let mut source = DataSource::blank(root.to_path_buf());
    source.driver = driver.to_string();
    source.name = create_data_source_name(driver, sources);
    if let Some(default_port) = driver_default_port(driver) {
        source.port = default_port.to_string();
    }
    if driver == "sqlite" {
        source.path = path.unwrap_or_default().to_string();
    }
    source
}

fn unconfigured_data_source(root: &Path, sources: &[DataSource]) -> DataSource {
    let mut source = DataSource::blank(root.to_path_buf());
    source.driver.clear();
    source.name = next_data_source_name("datasource", sources);
    source
}

/// An empty project should still open with one selectable data source so the
/// driver picker has a target (mirrors the deployment dialog's blank server).
fn seed_data_sources_if_empty(
    sources: &mut Vec<DataSource>,
    selected: &mut Option<usize>,
    root: &Path,
) {
    if sources.is_empty() {
        let source = unconfigured_data_source(root, sources);
        sources.push(source);
    }
    if selected.map_or(true, |index| index >= sources.len()) {
        *selected = if sources.is_empty() { None } else { Some(0) };
    }
}

fn resolve_selected_source(selected: Option<usize>, sources: &[DataSource]) -> Option<usize> {
    match selected {
        Some(index) if index < sources.len() => Some(index),
        _ if sources.is_empty() => None,
        _ => Some(0),
    }
}

fn driver_picker_rows(current: &str) -> Vec<(&'static str, bool)> {
    DATABASE_DRIVERS
        .iter()
        .map(|driver| (*driver, *driver == current))
        .collect()
}

pub fn is_sqlite_file_name(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("sqlite" | "db" | "sqlite3")
    )
}

fn sqlite_data_source_from_file(name: &str, rel_path: &str, root: &Path) -> DataSource {
    let mut source = DataSource::blank(root.to_path_buf());
    source.name = name.to_string();
    source.driver = "sqlite".to_string();
    source.path = rel_path.to_string();
    source
}

fn upsert_sqlite_data_source(root: &Path, rel_path: &str) -> std::result::Result<String, String> {
    let rel_path = rel_path.trim().replace('\\', "/");
    if rel_path.is_empty() {
        return Err("sqlite file path is empty".to_string());
    }
    let file_name = Path::new(&rel_path)
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .ok_or_else(|| "sqlite file name could not be derived".to_string())?
        .to_string();
    let mut sources = load_data_sources(root);
    let name = next_data_source_name(&file_name, &sources);
    sources.push(sqlite_data_source_from_file(&name, &rel_path, root));
    save_data_sources(root, &sources)?;
    Ok(name)
}

fn open_sqlite_data_source(
    workspace: &Workspace,
    abs_path: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let project = workspace.project().read(cx);
    let mut target = None;
    for worktree in project.visible_worktrees(cx) {
        let worktree = worktree.read(cx);
        let worktree_root = worktree.abs_path();
        let Ok(relative) = Path::new(abs_path).strip_prefix(&*worktree_root) else {
            continue;
        };
        if !relative.as_os_str().is_empty() {
            target = Some((worktree_root.to_path_buf(), relative.to_path_buf()));
            break;
        }
    }
    let Some((root, relative)) = target else {
        if cx.has_global::<DeployShared>() {
            if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                panel
                    .update(cx, |panel, cx| {
                        panel.saved_message =
                            Some(format!("failed: '{abs_path}' is not in any project folder"));
                    })
                    .log_err();
            }
        }
        return;
    };
    log::info!(
        "phpwb-action: sqlite target resolved: root={:?} rel={:?}",
        root.display(),
        relative.to_string_lossy()
    );
    let rel_path = relative.to_string_lossy().replace('\\', "/");
    match ensure_sqlite_data_source(&root, &rel_path) {
        Ok(name) => {
            if cx.has_global::<DeployShared>() {
                if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                    panel
                        .update(cx, |panel, cx| {
                            panel.refresh(cx);
                            panel.select_tab(PanelTab::Databases, cx);
                            panel.saved_message = None;
                            panel.expand_database_connection(&name, root.clone(), cx);
                        })
                        .log_err();
                }
            }
        }
        Err(message) => {
            if cx.has_global::<DeployShared>() {
                if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                    panel
                        .update(cx, |panel, cx| {
                            panel.saved_message = Some(format!("failed: {message}"));
                            cx.notify();
                        })
                        .log_err();
                }
            }
        }
    }
    let _ = window;
}

fn add_data_source_from_sqlite_file(
    workspace: &Workspace,
    abs_path: &str,
    cx: &App,
) -> std::result::Result<String, String> {
    let project = workspace.project().read(cx);
    let mut target = None;
    for worktree in project.visible_worktrees(cx) {
        let worktree = worktree.read(cx);
        let worktree_root = worktree.abs_path();
        let Ok(relative) = Path::new(abs_path).strip_prefix(&*worktree_root) else {
            continue;
        };
        if !relative.as_os_str().is_empty() {
            target = Some((worktree_root.to_path_buf(), relative.to_path_buf()));
            break;
        }
    }
    let Some((root, relative)) = target else {
        return Err(format!(
            "'{abs_path}' is not in any project folder — data source not added"
        ));
    };
    upsert_sqlite_data_source(&root, &relative.to_string_lossy())
}

enum SchemaListState {
    Idle,
    Loading,
    Loaded(Vec<String>),
    Failed(String),
}

pub struct DataSourcesDialogWindow(pub Option<AnyWindowHandle>);

impl gpui::Global for DataSourcesDialogWindow {}

pub struct DataSourcesDialog {
    root: PathBuf,
    sources: Vec<DataSource>,
    selected: Option<usize>,
    tab: DataSourceTab,
    message: Option<(String, Color)>,
    test_message: Option<(String, Color)>,
    schemas_state: SchemaListState,
    panel: Option<WeakEntity<PhpPanel>>,
    name_editor: Entity<Editor>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    user_editor: Entity<Editor>,
    password_editor: Entity<Editor>,
    database_editor: Entity<Editor>,
    url_editor: Entity<Editor>,
    ssh_local_port_editor: Entity<Editor>,
    ssh_menu_open: bool,
    driver_menu_open: bool,
    ssh_configs: Vec<SshEntry>,
    openssh_hosts: Vec<OpenSshHost>,
    ssh_config_missing: Option<String>,
    advanced_rows: Vec<(Entity<Editor>, Entity<Editor>)>,
    focus_handles: HashMap<DataSourceFocusSlot, FocusHandle>,
    focus_order: DialogFocusOrder,
    focus_tab: DataSourceTab,
    _bounds_subscription: Option<Subscription>,
}

impl Focusable for DataSourcesDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl DataSourcesDialog {
    fn new(
        root: PathBuf,
        seed: Option<DataSourceSeed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self {
            root,
            sources: Vec::new(),
            selected: None,
            tab: DataSourceTab::General,
            message: None,
            test_message: None,
            schemas_state: SchemaListState::Idle,
            panel: None,
            name_editor: cx.new(|cx| Editor::single_line(window, cx)),
            host_editor: cx.new(|cx| Editor::single_line(window, cx)),
            port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            user_editor: cx.new(|cx| Editor::single_line(window, cx)),
            password_editor: cx.new(|cx| Editor::single_line(window, cx)),
            database_editor: cx.new(|cx| Editor::single_line(window, cx)),
            url_editor: cx.new(|cx| Editor::single_line(window, cx)),
            ssh_local_port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            ssh_menu_open: false,
            driver_menu_open: false,
            ssh_configs: Vec::new(),
            openssh_hosts: Vec::new(),
            ssh_config_missing: None,
            advanced_rows: Vec::new(),
            focus_handles: HashMap::new(),
            focus_order: DialogFocusOrder::new(),
            focus_tab: DataSourceTab::General,
            _bounds_subscription: None,
        };
        let mut last_bounds: Option<(f32, f32)> = None;
        dialog._bounds_subscription =
            Some(cx.observe_window_bounds(window, move |_, window, _| {
                let bounds = window.bounds();
                let current = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                if last_bounds != Some(current) {
                    last_bounds = Some(current);
                    save_dialog_size("data_sources", bounds);
                }
            }));
        dialog.seed_from_disk(window, cx);
        dialog.apply_open_seed(seed, window, cx);
        dialog
    }

    fn focus_next_field(
        &mut self,
        _: &FocusNextDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.next(window, cx);
        cx.notify();
    }

    fn focus_previous_field(
        &mut self,
        _: &FocusPreviousDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.prev(window, cx);
        cx.notify();
    }

    fn data_source_focus_handle(
        &mut self,
        slot: DataSourceFocusSlot,
        cx: &App,
    ) -> FocusHandle {
        match slot {
            DataSourceFocusSlot::Name => self.name_editor.focus_handle(cx),
            DataSourceFocusSlot::Host => self.host_editor.focus_handle(cx),
            DataSourceFocusSlot::Port => self.port_editor.focus_handle(cx),
            DataSourceFocusSlot::User => self.user_editor.focus_handle(cx),
            DataSourceFocusSlot::Password => self.password_editor.focus_handle(cx),
            DataSourceFocusSlot::Database => self.database_editor.focus_handle(cx),
            DataSourceFocusSlot::Url => self.url_editor.focus_handle(cx),
            DataSourceFocusSlot::SshLocalPort => self.ssh_local_port_editor.focus_handle(cx),
            DataSourceFocusSlot::AdvancedKey(index) => self
                .advanced_rows
                .get(index)
                .expect("advanced row was rendered")
                .0
                .focus_handle(cx),
            DataSourceFocusSlot::AdvancedValue(index) => self
                .advanced_rows
                .get(index)
                .expect("advanced row was rendered")
                .1
                .focus_handle(cx),
            other => dialog_focus_handle(&mut self.focus_handles, other, cx),
        }
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sources = load_data_sources(&self.root);
        self.ssh_configs = load_ssh_configs(&self.root);
        self.openssh_hosts = load_openssh_hosts();
        self.restore_ssh_config_names();
        self.ssh_menu_open = false;
        seed_data_sources_if_empty(&mut self.sources, &mut self.selected, &self.root);
        if cx.has_global::<DeployShared>() {
            self.panel = cx.global::<DeployShared>().panel.clone();
        }
        self.tab = DataSourceTab::General;
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn resume_with_root(
        &mut self,
        root: PathBuf,
        seed: Option<DataSourceSeed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.root = root;
        self.seed_from_disk(window, cx);
        self.apply_open_seed(seed, window, cx);
    }

    /// Applies a Create Data Source seed: picks the `General` tab, creates the
    /// prefilled source, and focuses its name editor. Reuses the lone blank
    /// placeholder so an empty project does not accumulate stray rows.
    fn apply_open_seed(
        &mut self,
        seed: Option<DataSourceSeed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(seed) = seed else {
            return;
        };
        self.apply_editors(cx);
        let source = seeded_data_source(
            seed.driver,
            seed.path.as_deref(),
            &self.root,
            &self.sources,
        );
        let reuse_placeholder = self.sources.len() == 1
            && self.sources[0].driver.is_empty()
            && self.sources[0].name == "datasource"
            && self.sources[0].path.is_empty()
            && self.sources[0].host.is_empty()
            && self.sources[0].user.is_empty();
        if reuse_placeholder {
            self.sources[0] = source;
            self.selected = Some(0);
        } else {
            self.sources.push(source);
            self.selected = Some(self.sources.len() - 1);
        }
        self.tab = DataSourceTab::General;
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        let handle = self.name_editor.focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn load_fields(&mut self, window: &mut Window, cx: &mut App) {
        let source = self
            .selected
            .and_then(|index| self.sources.get(index))
            .cloned()
            .unwrap_or_else(|| DataSource::blank(self.root.clone()));
        let mut set = |editor: &Entity<Editor>, value: String| {
            editor.update(cx, |editor, cx| editor.set_text(value, window, cx));
        };
        set(&self.name_editor, source.name.clone());
        set(&self.host_editor, source.host.clone());
        set(&self.port_editor, source.port.clone());
        set(&self.user_editor, source.user.clone());
        set(&self.password_editor, String::new());
        set(&self.database_editor, source.database.clone());
        set(&self.url_editor, source.url.clone());
        self.ssh_menu_open = false;
        self.driver_menu_open = false;
        self.ssh_configs = load_ssh_configs(&self.root.clone());
        self.openssh_hosts = load_openssh_hosts();
        self.ssh_config_missing = source
            .ssh_config
            .as_deref()
            .and_then(|marker| marker.strip_prefix("openssh:"))
            .map(str::trim)
            .filter(|alias| !alias.is_empty())
            .filter(|alias| !self.openssh_hosts.iter().any(|host| host.alias == *alias))
            .map(openssh_missing_alias_message);
        set(&self.ssh_local_port_editor, source.ssh_local_port.clone());
        self.ssh_local_port_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text("<Dynamic>", window, cx);
        });
        self.advanced_rows = source
            .options
            .iter()
            .map(|(key, value)| {
                let key_editor = cx.new(|cx| Editor::single_line(window, cx));
                key_editor.update(cx, |editor, cx| editor.set_text(key.clone(), window, cx));
                let value_editor = cx.new(|cx| Editor::single_line(window, cx));
                value_editor.update(cx, |editor, cx| editor.set_text(value.clone(), window, cx));
                (key_editor, value_editor)
            })
            .collect();
    }

    fn apply_editors(&mut self, cx: &mut App) {
        let Some(index) = self.selected else {
            return;
        };
        let text = |editor: &Entity<Editor>| editor.read(cx).text(cx);
        let mut advanced = Vec::new();
        for (key_editor, value_editor) in &self.advanced_rows {
            advanced.push((text(key_editor), text(value_editor)));
        }
        if let Some(source) = self.sources.get_mut(index) {
            source.name = text(&self.name_editor);
            source.host = text(&self.host_editor);
            source.port = text(&self.port_editor);
            source.user = text(&self.user_editor);
            let typed_password = text(&self.password_editor);
            if !typed_password.is_empty() {
                source.password = typed_password;
            }
            source.database = text(&self.database_editor);
            source.url = text(&self.url_editor);
            source.ssh_local_port = text(&self.ssh_local_port_editor);
            source.options = rows_to_options(&advanced)
                .into_iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(text) => text,
                        other => serde_json::to_string(&other).unwrap_or_default(),
                    };
                    (key, value)
                })
                .collect();
        }
    }

    fn toggle_driver_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = resolve_selected_source(self.selected, &self.sources) else {
            return;
        };
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.tab = DataSourceTab::General;
            self.load_fields(window, cx);
        }
        self.driver_menu_open = !self.driver_menu_open;
        cx.notify();
    }

    fn select_driver(&mut self, driver: String, window: &mut Window, cx: &mut Context<Self>) {
        self.driver_menu_open = false;
        let Some(index) = resolve_selected_source(self.selected, &self.sources) else {
            return;
        };
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.load_fields(window, cx);
        }
        self.apply_editors(cx);
        if let Some(source) = self.sources.get_mut(index) {
            if source.driver != driver {
                source.driver = driver;
                if source.port.trim().is_empty() {
                    if let Some(default_port) = driver_default_port(&source.driver) {
                        source.port = default_port.to_string();
                    }
                }
            }
        }
        self.message = None;
        self.test_message = None;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn select_source(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(index) {
            return;
        }
        self.apply_editors(cx);
        self.selected = Some(index);
        self.tab = DataSourceTab::General;
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn select_tab(&mut self, tab: DataSourceTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.apply_editors(cx);
        self.tab = tab;
        self.message = None;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn add_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let mut blank = DataSource::blank(self.root.clone());
        blank.name = next_data_source_name("datasource", &self.sources);
        self.sources.push(blank);
        self.selected = Some(self.sources.len() - 1);
        self.tab = DataSourceTab::General;
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn remove_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.message = Some(("no data source selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.sources.remove(index);
        self.selected = if self.sources.is_empty() {
            None
        } else {
            Some(index.min(self.sources.len() - 1))
        };
        self.tab = DataSourceTab::General;
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn copy_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.message = Some(("no data source selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.apply_editors(cx);
        let mut copy = self.sources[index].clone();
        copy.name = next_data_source_name(&format!("{} copy", copy.name), &self.sources);
        self.sources.push(copy);
        self.selected = Some(self.sources.len() - 1);
        self.message = None;
        self.test_message = None;
        self.schemas_state = SchemaListState::Idle;
        self.load_fields(window, cx);
        cx.notify();
    }

    fn add_advanced_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let key_editor = cx.new(|cx| Editor::single_line(window, cx));
        let value_editor = cx.new(|cx| Editor::single_line(window, cx));
        self.advanced_rows.push((key_editor, value_editor));
        cx.notify();
    }

    fn remove_advanced_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.advanced_rows.pop();
        cx.notify();
    }

    fn toggle_read_only(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        source.read_only = !source.read_only;
        cx.notify();
    }

    fn toggle_ssh(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        source.ssh_enabled = !source.ssh_enabled;
        cx.notify();
    }

    fn toggle_ssh_menu(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.ssh_configs = load_ssh_configs(&self.root);
        self.openssh_hosts = load_openssh_hosts();
        self.ssh_menu_open = !self.ssh_menu_open;
        cx.notify();
    }

    fn select_ssh_config(&mut self, index: usize, cx: &mut Context<Self>) {
        self.ssh_menu_open = false;
        let Some(config) = self.ssh_configs.get(index).cloned() else {
            return;
        };
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        source.ssh_name = config.name.clone();
        source.ssh_config = None;
        source.ssh_user = config.user.trim().to_string();
        source.ssh_host = config.host.trim().to_string();
        source.ssh_port = ssh_entry_port(&config).to_string();
        self.ssh_config_missing = None;
        cx.notify();
    }

    fn select_openssh_config(&mut self, index: usize, cx: &mut Context<Self>) {
        self.ssh_menu_open = false;
        let Some(host) = self.openssh_hosts.get(index).cloned() else {
            return;
        };
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        source.ssh_name = String::new();
        source.ssh_config = Some(format!("openssh:{}", host.alias));
        source.ssh_user = host.user.clone().unwrap_or_default();
        source.ssh_host = host.host_name.clone();
        source.ssh_port = host
            .port
            .clone()
            .filter(|port| !port.trim().is_empty())
            .unwrap_or_else(|| "22".to_string());
        self.ssh_config_missing = None;
        cx.notify();
    }

    // databases.json cannot carry the config NAME (php-db denies unknown
    // tunnel fields), so reconstruct the selection by matching the resolved
    // host/port/user against the ssh store on load.
    fn restore_ssh_config_names(&mut self) {
        let configs = self.ssh_configs.clone();
        if configs.is_empty() {
            return;
        }
        for source in &mut self.sources {
            if !source.ssh_enabled
                || !source.ssh_name.trim().is_empty()
                || source.ssh_config.is_some()
            {
                continue;
            }
            let host = source.ssh_host.trim();
            let user = source.ssh_user.trim();
            let port = source.ssh_port.trim();
            if host.is_empty() {
                continue;
            }
            for config in &configs {
                let config_port = ssh_entry_port(config).to_string();
                let matches_port = port.is_empty() || port == config_port;
                let matches_user =
                    user.is_empty() || user == config.user.trim() || config.user.trim().is_empty();
                if host == config.host.trim() && matches_port && matches_user {
                    source.ssh_name = config.name.clone();
                    break;
                }
            }
        }
    }

    fn open_ssh_configs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.ssh_menu_open = false;
        open_ssh_configs_window(self.root.clone(), cx);
    }

    fn toggle_all_schemas(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        source.excluded_schemas.clear();
        cx.notify();
    }

    fn toggle_schema(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(source) = self.selected.and_then(|index| self.sources.get_mut(index)) else {
            return;
        };
        if let Some(position) = source
            .excluded_schemas
            .iter()
            .position(|known| known == &name)
        {
            source.excluded_schemas.remove(position);
        } else {
            source.excluded_schemas.push(name);
        }
        cx.notify();
    }

    fn refresh_schemas(&mut self, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        let Some(index) = self.selected else {
            return;
        };
        let Some(source) = self.sources.get(index) else {
            return;
        };
        let name = source.name.trim().to_string();
        if name.is_empty() {
            self.schemas_state = SchemaListState::Failed("data source name is empty".to_string());
            cx.notify();
            return;
        }
        self.schemas_state = SchemaListState::Loading;
        cx.notify();

        let config_path = self.root.join(".zed/databases.json");
        let root = self.root.clone();
        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("databases")
                .arg(&name)
                .arg("--config")
                .arg(&config_path)
                .arg("--json")
                .current_dir(&root)
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    parse_database_names(&String::from_utf8_lossy(&output.stdout))
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "databases command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.schemas_state = match result {
                    Ok(names) => SchemaListState::Loaded(names),
                    Err(message) => SchemaListState::Failed(message),
                };
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn test_connection(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            self.test_message = Some(("no data source selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.apply_editors(cx);
        let Some(source) = self.sources.get(selected).cloned() else {
            return;
        };
        let mut content = data_source_test_config(&source);
        let configs = load_ssh_configs(&self.root);
        if let Some(connections) = content.get_mut("connections").and_then(Value::as_array_mut) {
            for connection in connections.iter_mut() {
                if let Err(message) = resolve_ssh_tunnel_into(connection, &configs) {
                    self.test_message = Some((message, Color::Error));
                    cx.notify();
                    return;
                }
            }
        }
        let test_path = self.root.join(".zed/.datasources.test.json");
        if let Err(error) = std::fs::create_dir_all(test_path.parent().expect("test config parent"))
            .and_then(|_| {
                std::fs::write(
                    &test_path,
                    serde_json::to_string(&content).expect("serialize test config"),
                )
            })
        {
            self.test_message = Some((format!("cannot write test config: {error}"), Color::Error));
            cx.notify();
            return;
        }
        self.test_message = Some(("testing...".to_string(), Color::Muted));
        cx.notify();

        let root = self.root.clone();
        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-db");
            command
                .arg("schema")
                .arg("_test")
                .arg("--config")
                .arg(&test_path)
                .arg("--json")
                .current_dir(&root);
            command.output().await.map_err(|error| error.to_string())
        });

        cx.spawn(async move |_, cx| {
            let result = task.await;
            weak.update(cx, |dialog, cx| {
                dialog.test_message = Some(match result {
                    Ok(output) if output.status.success() => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let driver = serde_json::from_str::<Value>(stdout.trim())
                            .ok()
                            .and_then(|value| string_field(&value, "driver"))
                            .map(|driver| format!(" · {}", driver_display_name(&driver)))
                            .unwrap_or_default();
                        (format!("ok{driver}"), Color::Success)
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let message = first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "connection failed".to_string());
                        (message, Color::Error)
                    }
                    Err(error) => (error, Color::Error),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) -> std::result::Result<(), String> {
        self.apply_editors(cx);
        for source in &self.sources {
            let problems = data_source_problems(source);
            if let Some(problem) = problems.first() {
                return Err(format!("{}: {}", source.name.trim(), problem));
            }
        }
        save_data_sources(&self.root, &self.sources)?;
        if let Some(panel) = self.panel.clone() {
            panel
                .update(cx, |panel, cx| {
                    panel.refresh(cx);
                    panel.saved_message = Some("data sources updated".to_string());
                })
                .log_err();
        }
        Ok(())
    }

    fn apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match self.save(cx) {
            Ok(()) => {
                self.message = Some(("applied".to_string(), Color::Success));
            }
            Err(message) => {
                self.message = Some((message, Color::Error));
            }
        }
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(message) = self.save(cx) {
            self.message = Some((message, Color::Error));
            cx.notify();
            return;
        }
        self.close(window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.has_global::<DataSourcesDialogWindow>() {
            cx.global_mut::<DataSourcesDialogWindow>().0 = None;
        }
        window.remove_window();
        cx.notify();
    }
}

pub struct SshConfigDialogWindow(pub Option<AnyWindowHandle>);

impl gpui::Global for SshConfigDialogWindow {}

pub struct SshConfigsDialog {
    root: PathBuf,
    configs: Vec<SshEntry>,
    selected: Option<usize>,
    message: Option<(String, Color)>,
    test_message: Option<(String, Color)>,
    panel: Option<WeakEntity<PhpPanel>>,
    auth: SshAuthMode,
    auth_menu_open: bool,
    save_password: SavePassword,
    keep_alive_enabled: bool,
    strict: bool,
    ssh_alias: String,
    ssh_alias_menu_open: bool,
    ssh_alias_missing: bool,
    openssh_hosts: Vec<OpenSshHost>,
    name_editor: Entity<Editor>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    user_editor: Entity<Editor>,
    password_editor: Entity<Editor>,
    key_path_editor: Entity<Editor>,
    keep_alive_editor: Entity<Editor>,
    focus_handles: HashMap<SshFocusSlot, FocusHandle>,
    focus_order: DialogFocusOrder,
    focus_auth: SshAuthMode,
    _bounds_subscription: Option<Subscription>,
}

impl Focusable for SshConfigsDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl SshConfigsDialog {
    fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let password_editor = cx.new(|cx| Editor::single_line(window, cx));
        password_editor.update(cx, |editor, cx| editor.set_masked(true, cx));
        let mut dialog = Self {
            root,
            configs: Vec::new(),
            selected: None,
            message: None,
            test_message: None,
            panel: None,
            auth: SshAuthMode::Password,
            auth_menu_open: false,
            save_password: SavePassword::Forever,
            keep_alive_enabled: true,
            strict: false,
            ssh_alias: String::new(),
            ssh_alias_menu_open: false,
            ssh_alias_missing: false,
            openssh_hosts: Vec::new(),
            name_editor: cx.new(|cx| Editor::single_line(window, cx)),
            host_editor: cx.new(|cx| Editor::single_line(window, cx)),
            port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            user_editor: cx.new(|cx| Editor::single_line(window, cx)),
            password_editor,
            key_path_editor: cx.new(|cx| Editor::single_line(window, cx)),
            keep_alive_editor: cx.new(|cx| Editor::single_line(window, cx)),
            focus_handles: HashMap::new(),
            focus_order: DialogFocusOrder::new(),
            focus_auth: SshAuthMode::Password,
            _bounds_subscription: None,
        };
        let mut last_bounds: Option<(f32, f32)> = None;
        dialog._bounds_subscription =
            Some(cx.observe_window_bounds(window, move |_, window, _| {
                let bounds = window.bounds();
                let current = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                if last_bounds != Some(current) {
                    last_bounds = Some(current);
                    save_dialog_size("ssh", bounds);
                }
            }));
        dialog.seed_from_disk(window, cx);
        dialog
    }

    fn focus_next_field(
        &mut self,
        _: &FocusNextDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.next(window, cx);
        cx.notify();
    }

    fn focus_previous_field(
        &mut self,
        _: &FocusPreviousDialogField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_order.prev(window, cx);
        cx.notify();
    }

    fn ssh_focus_handle(&mut self, slot: SshFocusSlot, cx: &App) -> FocusHandle {
        match slot {
            SshFocusSlot::Name => self.name_editor.focus_handle(cx),
            SshFocusSlot::Host => self.host_editor.focus_handle(cx),
            SshFocusSlot::Port => self.port_editor.focus_handle(cx),
            SshFocusSlot::User => self.user_editor.focus_handle(cx),
            SshFocusSlot::Password => self.password_editor.focus_handle(cx),
            SshFocusSlot::KeyPath => self.key_path_editor.focus_handle(cx),
            SshFocusSlot::KeepAliveSeconds => self.keep_alive_editor.focus_handle(cx),
            other => dialog_focus_handle(&mut self.focus_handles, other, cx),
        }
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.configs = load_ssh_configs(&self.root);
        self.selected = if self.configs.is_empty() {
            None
        } else {
            Some(0)
        };
        if cx.has_global::<DeployShared>() {
            self.panel = cx.global::<DeployShared>().panel.clone();
        }
        self.message = None;
        self.test_message = None;
        self.load_editors(window, cx);
        cx.notify();
    }

    fn resume_with_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.root = root;
        self.seed_from_disk(window, cx);
    }

    fn load_editors(&mut self, window: &mut Window, cx: &mut App) {
        let config = self
            .selected
            .and_then(|index| self.configs.get(index))
            .cloned()
            .unwrap_or_else(SshEntry::blank);
        self.auth = config.auth;
        self.auth_menu_open = false;
        self.save_password = if config.password_session {
            SavePassword::Session
        } else {
            SavePassword::Forever
        };
        self.keep_alive_enabled = config.keep_alive > 0;
        self.strict = config.strict_host_key;
        self.ssh_alias = config.ssh_alias.clone().unwrap_or_default();
        self.ssh_alias_menu_open = false;
        self.ssh_alias_missing = false;
        self.openssh_hosts = load_openssh_hosts();
        let mut host = config.host.clone();
        let mut user = config.user.clone();
        let mut port = config.port.clone();
        let mut key_path = config.key_path.clone();
        if self.auth == SshAuthMode::Config && !self.ssh_alias.trim().is_empty() {
            let alias = self.ssh_alias.trim();
            match self.openssh_hosts.iter().find(|host| host.alias == alias) {
                Some(entry) => {
                    host = entry.host_name.clone();
                    user = entry.user.clone().unwrap_or_default();
                    port = entry.port.clone().unwrap_or_else(|| "22".to_string());
                    key_path = entry.identity_file.clone().unwrap_or_default();
                }
                None => self.ssh_alias_missing = true,
            }
        }
        let mut set = |editor: &Entity<Editor>, value: String| {
            editor.update(cx, |editor, cx| editor.set_text(value, window, cx));
        };
        set(&self.name_editor, config.name.clone());
        set(&self.host_editor, host);
        set(&self.port_editor, port);
        set(&self.user_editor, user);
        set(&self.password_editor, config.password.clone());
        set(&self.key_path_editor, key_path);
        set(
            &self.keep_alive_editor,
            if config.keep_alive > 0 {
                config.keep_alive.to_string()
            } else {
                "15".to_string()
            },
        );
    }

    fn stage_current(&mut self, cx: &mut Context<Self>) {
        let text = |editor: &Entity<Editor>| editor.read(cx).text(cx);
        let Some(index) = self.selected else {
            return;
        };
        let Some(config) = self.configs.get_mut(index) else {
            return;
        };
        config.name = text(&self.name_editor);
        config.host = text(&self.host_editor);
        config.port = text(&self.port_editor);
        config.user = text(&self.user_editor);
        config.password = text(&self.password_editor);
        config.key_path = text(&self.key_path_editor);
        config.auth = self.auth;
        config.ssh_alias = if self.ssh_alias.trim().is_empty() {
            None
        } else {
            Some(self.ssh_alias.trim().to_string())
        };
        config.password_session = self.save_password == SavePassword::Session;
        config.strict_host_key = self.strict;
        config.keep_alive = if self.keep_alive_enabled {
            text(&self.keep_alive_editor)
                .trim()
                .parse::<u16>()
                .unwrap_or(15)
        } else {
            0
        };
    }

    fn select_config(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected == Some(index) {
            return;
        }
        self.stage_current(cx);
        self.selected = Some(index);
        self.test_message = None;
        self.message = None;
        self.load_editors(window, cx);
        cx.notify();
    }

    fn add_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage_current(cx);
        let name = unique_ssh_config_name("config", &self.configs);
        let mut config = SshEntry::blank();
        config.name = name;
        self.configs.push(config);
        self.selected = Some(self.configs.len() - 1);
        self.test_message = None;
        self.message = None;
        self.load_editors(window, cx);
        cx.notify();
    }

    fn remove_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            self.message = Some(("no configuration selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        self.configs.remove(index);
        self.selected = if self.configs.is_empty() {
            None
        } else {
            Some(index.min(self.configs.len() - 1))
        };
        self.test_message = None;
        self.message = None;
        self.load_editors(window, cx);
        cx.notify();
    }

    fn toggle_auth_menu(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.ssh_alias_menu_open = false;
        self.auth_menu_open = !self.auth_menu_open;
        cx.notify();
    }

    fn close_auth_menu(&mut self, cx: &mut Context<Self>) {
        if self.auth_menu_open {
            self.auth_menu_open = false;
            cx.notify();
        }
    }

    fn select_auth_mode(&mut self, mode: SshAuthMode, cx: &mut Context<Self>) {
        self.auth_menu_open = false;
        self.ssh_alias_menu_open = false;
        if self.auth != mode {
            self.stage_current(cx);
            self.auth = mode;
            if mode == SshAuthMode::Config {
                self.openssh_hosts = load_openssh_hosts();
            }
        }
        cx.notify();
    }

    fn toggle_ssh_alias_menu(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.openssh_hosts = load_openssh_hosts();
        self.ssh_alias_menu_open = !self.ssh_alias_menu_open;
        cx.notify();
    }

    fn select_ssh_alias(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_alias_menu_open = false;
        let Some(host) = self.openssh_hosts.get(index).cloned() else {
            cx.notify();
            return;
        };
        self.ssh_alias = host.alias.clone();
        self.ssh_alias_missing = false;
        let mut set = |editor: &Entity<Editor>, value: String| {
            editor.update(cx, |editor, cx| editor.set_text(value, window, cx));
        };
        set(&self.host_editor, host.host_name.clone());
        set(&self.user_editor, host.user.clone().unwrap_or_default());
        set(
            &self.port_editor,
            host.port.clone().unwrap_or_else(|| "22".to_string()),
        );
        set(
            &self.key_path_editor,
            host.identity_file.clone().unwrap_or_default(),
        );
        self.stage_current(cx);
        cx.notify();
    }

    fn cycle_save_password(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.save_password = self.save_password.next();
        cx.notify();
    }

    fn reset_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_password = SavePassword::Forever;
        self.password_editor.update(cx, |editor, cx| {
            editor.set_text(String::new(), window, cx);
        });
        let Some(index) = self.selected else {
            cx.notify();
            return;
        };
        if let Some(config) = self.configs.get_mut(index) {
            config.password = String::new();
            config.password_session = false;
        }
        cx.notify();
    }

    fn toggle_keep_alive(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.keep_alive_enabled = !self.keep_alive_enabled;
        cx.notify();
    }

    fn toggle_strict(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.strict = !self.strict;
        cx.notify();
    }

    fn test_connection(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        let Some(index) = self.selected else {
            self.test_message = Some(("no configuration selected".to_string(), Color::Error));
            cx.notify();
            return;
        };
        let Some(config) = self.configs.get(index).cloned() else {
            return;
        };
        let config = if config.auth == SshAuthMode::Config {
            match resolve_openssh_entry(&config, &load_openssh_hosts()) {
                Ok(resolved) => resolved,
                Err(message) => {
                    self.test_message = Some((message, Color::Warning));
                    cx.notify();
                    return;
                }
            }
        } else {
            config
        };
        let mut command = match ssh_probe_command(&config) {
            Ok(command) => command,
            Err(message) => {
                self.test_message = Some((message, Color::Warning));
                cx.notify();
                return;
            }
        };
        self.test_message = Some(("testing...".to_string(), Color::Muted));
        cx.notify();

        let weak = cx.weak_entity();
        let task = cx.background_spawn(async move { command.output().await });
        cx.spawn(async move |_, cx| {
            let output = task.await;
            weak.update(cx, |dialog, cx| {
                dialog.test_message = Some(match output {
                    Ok(output) if output.status.success() => {
                        ("connection ok".to_string(), Color::Success)
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let message = first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "connection failed".to_string());
                        (message, Color::Error)
                    }
                    Err(error) => (error.to_string(), Color::Error),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) -> std::result::Result<(), String> {
        self.stage_current(cx);
        save_ssh_configs(&self.root, &self.configs)?;
        if let Some(panel) = self.panel.clone() {
            panel
                .update(cx, |panel, cx| {
                    panel.saved_message = Some("ssh configurations updated".to_string());
                })
                .log_err();
        }
        Ok(())
    }

    fn apply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        match self.save(cx) {
            Ok(()) => {
                self.message = Some(("applied".to_string(), Color::Success));
            }
            Err(message) => {
                self.message = Some((message, Color::Error));
            }
        }
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(message) = self.save(cx) {
            self.message = Some((message, Color::Error));
            cx.notify();
            return;
        }
        self.close(window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.has_global::<SshConfigDialogWindow>() {
            cx.global_mut::<SshConfigDialogWindow>().0 = None;
        }
        window.remove_window();
        cx.notify();
    }
}

impl Render for SshConfigsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let message = self.message.clone();
        let test_message = self.test_message.clone();

        // Rebuild the Tab order for the current authentication mode.
        let ssh_slots = ssh_focus_slots(self.auth, self.keep_alive_enabled);
        let mut focus: HashMap<SshFocusSlot, FocusHandle> = HashMap::new();
        for slot in &ssh_slots {
            let handle = self.ssh_focus_handle(*slot, cx);
            focus.insert(*slot, handle);
        }
        let order = ssh_slots
            .iter()
            .map(|slot| dialog_slot(&focus, *slot))
            .collect::<Vec<_>>();
        let auth_changed = self.focus_auth != self.auth;
        self.focus_auth = self.auth;
        self.focus_order.set_slots(order, auth_changed);

        let mut config_rows = Vec::new();
        for (index, config) in self.configs.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            config_rows.push(
                h_flex()
                    .id(("ssh-config-row", index))
                    .w_full()
                    .flex_none()
                    .h(px(26.))
                    .px_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_selected, |this| {
                        this.border_l_2().border_color(colors.border_focused)
                    })
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        Label::new(config.name.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if is_selected {
                                Color::Accent
                            } else {
                                Color::Default
                            })
                            .truncate(),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_config(index, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if config_rows.is_empty() {
            config_rows.push(dialog_static("no configurations", Color::Muted));
        }

        let auth_picker = h_flex()
            .id("ssh-auth-type-picker")
            .flex_1()
            .min_w_0()
            .h(DIALOG_CONTROL_HEIGHT)
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .cursor_pointer()
            .hover(|style| style.bg(colors.element_hover))
            .track_focus(&dialog_slot(&focus, SshFocusSlot::AuthType))
            .focus(|style| style.border_color(colors.border_focused))
            .child(
                Label::new(self.auth.label())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .truncate(),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.toggle_auth_menu(cx);
                }
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_auth_menu(cx);
            }));

        let mut auth_menu: Vec<AnyElement> = Vec::new();
        if self.auth_menu_open {
            let mut menu_rows: Vec<AnyElement> = Vec::new();
            for mode in SSH_AUTH_OPTIONS {
                let is_current = mode == self.auth;
                menu_rows.push(
                    h_flex()
                        .id(format!("ssh-auth-row-{}", mode.key()))
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .when(is_current, |this| this.bg(colors.element_selected))
                        .hover(|style| style.bg(colors.element_hover))
                        .child(
                            Label::new(mode.label())
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(if is_current {
                                    Color::Accent
                                } else {
                                    Color::Default
                                })
                                .truncate(),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.select_auth_mode(mode, cx);
                        }))
                        .into_any_element(),
                );
            }
            auth_menu.push(
                v_flex()
                    .id("ssh-auth-type-menu")
                    .w_full()
                    .flex_none()
                    .gap_0p5()
                    .px_1()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.elevated_surface_background)
                    .shadow_sm()
                    .children(menu_rows)
                    .into_any_element(),
            );
        }

        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(dialog_row(
            "Name:",
            dialog_editor("ssh-name-editor", &self.name_editor, None, window, cx),
        ));
        rows.push(dialog_row(
            "Host:",
            h_flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap_2()
                .child(dialog_editor(
                    "ssh-host-editor",
                    &self.host_editor,
                    None,
                    window,
                    cx,
                ))
                .child(div().flex_none().w(px(35.)).child(dialog_label("Port:")))
                .child(dialog_editor(
                    "ssh-port-editor",
                    &self.port_editor,
                    Some(px(60.)),
                    window,
                    cx,
                )),
        ));
        rows.push(dialog_row(
            "Username:",
            dialog_editor("ssh-user-editor", &self.user_editor, None, window, cx),
        ));
        rows.push(
            v_flex()
                .w_full()
                .flex_none()
                .child(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .min_h(DIALOG_ROW_HEIGHT)
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .w(DIALOG_LABEL_WIDTH)
                                .child(dialog_label("Authentication type:")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(DIALOG_CONTROL_HEIGHT)
                                .flex()
                                .items_center()
                                .child(auth_picker),
                        ),
                )
                .children(auth_menu)
                .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.close_auth_menu(cx);
                }))
                .into_any_element(),
        );
        match self.auth {
            SshAuthMode::Password => {
                rows.push(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .flex_col()
                        .gap_0p5()
                        .child(dialog_row(
                            "Password:",
                            dialog_editor(
                                "ssh-password-editor",
                                &self.password_editor,
                                None,
                                window,
                                cx,
                            ),
                        ))
                        .child(
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .pl(DIALOG_CONTROL_INDENT)
                                .child(
                                    dialog_button(
                                        "ssh-save-password",
                                        format!("Save: {}", self.save_password.label()),
                                        DialogButtonVariant::Secondary,
                                    )
                                    .track_focus(&dialog_slot(
                                        &focus,
                                        SshFocusSlot::SavePassword,
                                    ))
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, _window, cx| {
                                            this.cycle_save_password(cx);
                                        },
                                    )),
                                )
                                .child(dialog_static(self.save_password.hint(), Color::Muted))
                                .child(
                                    dialog_button(
                                        "ssh-reset-password",
                                        "Reset",
                                        DialogButtonVariant::Secondary,
                                    )
                                    .track_focus(&dialog_slot(
                                        &focus,
                                        SshFocusSlot::ResetPassword,
                                    ))
                                    .on_click(cx.listener(
                                        |this, _: &ClickEvent, window, cx| {
                                            this.reset_password(window, cx);
                                        },
                                    )),
                                ),
                        )
                        .into_any_element(),
                );
            }
            SshAuthMode::Key => {
                rows.push(dialog_row(
                    "Private key file:",
                    dialog_editor(
                        "ssh-key-path-editor",
                        &self.key_path_editor,
                        None,
                        window,
                        cx,
                    ),
                ));
            }
            SshAuthMode::Agent => {}
            SshAuthMode::Config => {
                rows.push(dialog_row(
                    "SSH config:",
                    dialog_static(openssh_config_display_path(), Color::Muted),
                ));
                let alias_label = if self.ssh_alias.trim().is_empty() {
                    "<select host alias>".to_string()
                } else {
                    self.ssh_alias.clone()
                };
                rows.push(dialog_row(
                    "Host alias:",
                    h_flex()
                        .id("ssh-alias-button")
                        .flex_1()
                        .min_w_0()
                        .h(DIALOG_CONTROL_HEIGHT)
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .px_2()
                        .rounded_md()
                        .border_1()
                        .border_color(colors.border)
                        .cursor_pointer()
                        .hover(|style| style.bg(colors.element_hover))
                        .track_focus(&dialog_slot(&focus, SshFocusSlot::Alias))
                        .focus(|style| style.border_color(colors.border_focused))
                        .child(
                            Label::new(alias_label)
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(if self.ssh_alias.trim().is_empty() {
                                    Color::Muted
                                } else {
                                    Color::Default
                                })
                                .truncate(),
                        )
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_ssh_alias_menu(cx);
                        })),
                ));
                if self.ssh_alias_menu_open {
                    let mut alias_rows: Vec<AnyElement> = Vec::new();
                    for (index, host) in self.openssh_hosts.iter().enumerate() {
                        let is_active = self.ssh_alias.trim() == host.alias;
                        alias_rows.push(
                            h_flex()
                                .id(("ssh-alias-row", index))
                                .w_full()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .when(is_active, |this| this.bg(colors.element_selected))
                                .hover(|style| style.bg(colors.element_hover))
                                .child(
                                    Label::new(host.alias.clone())
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(if is_active {
                                            Color::Accent
                                        } else {
                                            Color::Default
                                        })
                                        .truncate(),
                                )
                                .child(
                                    Label::new(openssh_user_host_text(host))
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted)
                                        .truncate(),
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.select_ssh_alias(index, window, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    if alias_rows.is_empty() {
                        alias_rows.push(dialog_static("no host aliases found", Color::Muted));
                    }
                    rows.push(
                        v_flex()
                            .id("ssh-alias-menu")
                            .w_full()
                            .flex_none()
                            .gap_0p5()
                            .px_1()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.elevated_surface_background)
                            .shadow_sm()
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .children(alias_rows)
                            .into_any_element(),
                    );
                }
                if self.ssh_alias_missing && !self.ssh_alias.trim().is_empty() {
                    rows.push(dialog_static(
                        openssh_missing_alias_message(self.ssh_alias.trim()),
                        Color::Warning,
                    ));
                }
                rows.push(dialog_row(
                    "Private key file:",
                    dialog_editor(
                        "ssh-key-path-editor",
                        &self.key_path_editor,
                        None,
                        window,
                        cx,
                    ),
                ));
            }
        }
        rows.push(dialog_section_header("Connection Parameters"));
        rows.push(
            dialog_toggle_row_with(
                "ssh-keep-alive",
                self.keep_alive_enabled,
                "Send keep-alives every",
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(dialog_editor(
                        "ssh-keep-alive-editor",
                        &self.keep_alive_editor,
                        Some(px(45.)),
                        window,
                        cx,
                    ))
                    .child(
                        Label::new("seconds")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    ),
                cx,
            )
            .track_focus(&dialog_slot(&focus, SshFocusSlot::KeepAlive))
            .focus(|style| style.bg(colors.element_hover))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.toggle_keep_alive(cx);
                }
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_keep_alive(cx);
            }))
            .into_any_element(),
        );
        rows.push(
            dialog_toggle_row_with(
                "ssh-strict",
                self.strict,
                "Strict host key check",
                dialog_static("off = accept-new", Color::Muted),
                cx,
            )
            .track_focus(&dialog_slot(&focus, SshFocusSlot::Strict))
            .focus(|style| style.bg(colors.element_hover))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.toggle_strict(cx);
                }
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_strict(cx);
            }))
            .into_any_element(),
        );
        rows.push(dialog_static("proxy: global settings only", Color::Muted));

        let test_button = div()
            .flex_none()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.test_connection(cx);
                }
            }))
            .child(
                dialog_button("ssh-test", "Test Connection", DialogButtonVariant::Link)
                    .track_focus(&dialog_slot(&focus, SshFocusSlot::TestConnection))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.test_connection(cx);
                    })),
            );

        v_flex()
            .id("ssh-dialog")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .key_context("SshConfigsDialog")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "escape" && this.auth_menu_open {
                    this.auth_menu_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
            }))
            .on_action(cx.listener(Self::focus_next_field))
            .on_action(cx.listener(Self::focus_previous_field))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(200.))
                            .flex_none()
                            .h_full()
                            .border_r_1()
                            .border_color(colors.border)
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_end()
                                    .gap_0p5()
                                    .px_1()
                                    .py_1()
                                    .child(
                                        IconButton::new("ssh-add", IconName::Plus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.add_config(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("ssh-remove", IconName::SquareMinus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.remove_config(window, cx);
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .id("ssh-config-list")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .children(config_rows),
                            ),
                    )
                    .child(
                        div()
                            .id("ssh-tab-content")
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .p_2()
                            .child(v_flex().w_full().children(rows)),
                    ),
            )
            .children(message.map(|(text, color)| {
                h_flex()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new(text)
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(color)
                            .truncate(),
                    )
                    .into_any_element()
            }))
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(test_button)
                            .children(test_message.as_ref().map(|(text, color)| {
                                Label::new(text.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(*color)
                                    .flex_1()
                                    .truncate()
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.cancel(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ssh-cancel",
                                            "Cancel",
                                            DialogButtonVariant::Secondary,
                                        )
                                        .track_focus(&dialog_slot(&focus, SshFocusSlot::Cancel))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.cancel(window, cx);
                                            },
                                        )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.apply(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ssh-apply",
                                            "Apply",
                                            DialogButtonVariant::Secondary,
                                        )
                                        .track_focus(&dialog_slot(&focus, SshFocusSlot::Apply))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.apply(window, cx);
                                            },
                                        )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.confirm(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ssh-ok",
                                            "OK",
                                            DialogButtonVariant::Primary,
                                        )
                                        .track_focus(&dialog_slot(&focus, SshFocusSlot::Ok))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.confirm(window, cx);
                                            },
                                        )),
                                    ),
                            ),
                    ),
            )
    }
}

fn ssh_probe_command(config: &SshEntry) -> std::result::Result<util::command::Command, String> {
    let port = config.port.trim().to_string();
    let port = if port.is_empty() {
        "22".to_string()
    } else {
        port
    };
    if !port.chars().all(|character| character.is_ascii_digit()) || port.parse::<u16>().is_err() {
        return Err("wrong port number".to_string());
    }
    let host = config.host.trim().to_string();
    if host.is_empty() {
        return Err("host is required".to_string());
    }
    let user = config.user.trim().to_string();
    if user.is_empty() {
        return Err("username is required".to_string());
    }
    let strict = if config.strict_host_key {
        "StrictHostKeyChecking=yes"
    } else {
        "StrictHostKeyChecking=accept-new"
    };
    match config.auth {
        SshAuthMode::Config => {
            // Resolved by the caller; if that did not happen fall back to the
            // concrete auth implied by the effective fields.
            let mut resolved = config.clone();
            resolved.auth = ssh_effective_auth(config);
            return ssh_probe_command(&resolved);
        }
        SshAuthMode::Agent => {
            let mut command = util::command::new_command("ssh");
            command
                .arg(format!("-p{port}"))
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=5")
                .arg("-o")
                .arg(strict)
                .arg(format!("{user}@{host}"))
                .arg("true");
            Ok(command)
        }
        SshAuthMode::Key => {
            let key_path = config.key_path.trim().to_string();
            if key_path.is_empty() {
                return Err("key path is required".to_string());
            }
            let mut command = util::command::new_command("ssh");
            command
                .arg(format!("-p{port}"))
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=5")
                .arg("-o")
                .arg(strict)
                .arg("-i")
                .arg(key_path)
                .arg(format!("{user}@{host}"))
                .arg("true");
            Ok(command)
        }
        SshAuthMode::Password => {
            let password = config.password.clone();
            if password.is_empty() {
                return Err("password is required".to_string());
            }
            if !sshpass_available() {
                return Err("sshpass binary not found".to_string());
            }
            let mut command = util::command::new_command("sshpass");
            command
                .arg("-p")
                .arg(password)
                .arg("ssh")
                .arg(format!("-p{port}"))
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=5")
                .arg("-o")
                .arg(strict)
                .arg(format!("{user}@{host}"))
                .arg("true");
            Ok(command)
        }
    }
}

fn sshpass_available() -> bool {
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("sshpass");
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

pub fn open_ssh_configs_window(root: PathBuf, cx: &mut App) {
    register_dialog_key_bindings(cx, "SshConfigsDialog");
    if !cx.has_global::<SshConfigDialogWindow>() {
        cx.set_global(SshConfigDialogWindow(None));
    }
    let existing = cx.global::<SshConfigDialogWindow>().0;
    if let Some(handle) = existing.and_then(|any| any.downcast::<SshConfigsDialog>()) {
        let resumed = handle.update(cx, |dialog, window, cx| {
            dialog.resume_with_root(root.clone(), window, cx);
            window.activate_window();
            cx.notify();
        });
        if resumed.is_ok() {
            return;
        }
        cx.global_mut::<SshConfigDialogWindow>().0 = None;
    }

    let window_bounds = dialog_bounds("ssh", size(px(700.), px(480.)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("SSH Configurations")),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("SSH Configurations");
                let view = cx.new(|cx| SshConfigsDialog::new(root.clone(), window, cx));
                window.activate_window();
                view.focus_handle(cx).focus(window, cx);
                view
            },
        )
        .log_err();
    if let Some(handle) = handle {
        cx.global_mut::<SshConfigDialogWindow>().0 = Some(handle.into());
    }
}

pub struct RootPickerWindow(pub Option<AnyWindowHandle>);

impl gpui::Global for RootPickerWindow {}

pub struct RootPickerRequest {
    root: PathBuf,
    server: String,
    host_display: String,
    title: String,
    local_base: Option<PathBuf>,
}

pub struct RootPickerDialog {
    request: RootPickerRequest,
    entries: HashMap<String, std::result::Result<Vec<RemoteEntry>, String>>,
    expanded: HashSet<String>,
    selected: Option<String>,
    query_editor: Entity<Editor>,
    seq: u64,
    _bounds_subscription: Option<Subscription>,
}

impl Focusable for RootPickerDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query_editor.focus_handle(cx)
    }
}

impl RootPickerDialog {
    fn new(request: RootPickerRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut dialog = Self {
            request,
            entries: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            query_editor: cx.new(|cx| Editor::single_line(window, cx)),
            seq: 0,
            _bounds_subscription: None,
        };
        let mut last_bounds: Option<(f32, f32)> = None;
        dialog._bounds_subscription =
            Some(cx.observe_window_bounds(window, move |_, window, _| {
                let bounds = window.bounds();
                let current = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                if last_bounds != Some(current) {
                    last_bounds = Some(current);
                    save_dialog_size("root_picker", bounds);
                }
            }));
        dialog.expanded.insert(".".to_string());
        dialog.load_dir(".".to_string(), cx);
        dialog
    }

    fn root_label(&self) -> String {
        format!("{} ({})", self.request.server, self.request.host_display)
    }

    fn load_dir(&mut self, path: String, cx: &mut Context<Self>) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(local_base) = self.request.local_base.clone() {
            let task_path = path.clone();
            let task = cx.background_spawn(async move {
                let base = if task_path == "." {
                    local_base
                } else {
                    local_base.join(&task_path)
                };
                match std::fs::read_dir(&base) {
                    Ok(reader) => {
                        let mut items: Vec<(String, bool)> = Vec::new();
                        for entry in reader.flatten() {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let is_directory =
                                entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                            items.push((name, is_directory));
                        }
                        Ok(parse_local_dir_entries(items))
                    }
                    Err(error) => Err(error.to_string()),
                }
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |this, cx| {
                    if this.seq != seq {
                        return;
                    }
                    this.entries.insert(path, result);
                    cx.notify();
                })
                .log_err();
            })
            .detach();
            return;
        }
        let root = self.request.root.clone();
        let server = self.request.server.clone();
        let task_path = path.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-deploy")
                .arg("ls")
                .arg("--config")
                .arg(root.join(".zed/deploy.picker.json"))
                .arg("--server")
                .arg(&server)
                .arg("--path")
                .arg(&task_path)
                .arg("--json")
                .current_dir(&root)
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    parse_remote_entries(&String::from_utf8_lossy(&output.stdout), &task_path)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "ls command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.seq != seq {
                    return;
                }
                this.entries.insert(path, result);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
            cx.notify();
            return;
        }
        self.expanded.insert(path.clone());
        if !self.entries.contains_key(&path) {
            self.load_dir(path, cx);
        }
        cx.notify();
    }

    fn select_dir(&mut self, path: String, cx: &mut Context<Self>) {
        self.selected = Some(path);
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let picked = path;
        if cx.has_global::<DeployDialogWindow>() {
            let handle = cx
                .global::<DeployDialogWindow>()
                .0
                .and_then(|any| any.downcast::<DeploymentDialog>());
            if let Some(handle) = handle {
                let applied = handle.update(cx, |dialog, window, cx| {
                    dialog.apply_picked_root(picked, window, cx);
                });
                if applied.is_ok() {
                    self.close(window, cx);
                    return;
                }
            }
        }
        self.close(window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if cx.has_global::<RootPickerWindow>() {
            cx.global_mut::<RootPickerWindow>().0 = None;
        }
        window.remove_window();
        cx.notify();
    }
}

impl Render for RootPickerDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let query = self.query_editor.read(cx).text(cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.push(
            h_flex()
                .id("rp-root-row")
                .w_full()
                .flex_none()
                .items_center()
                .gap_1()
                .h(px(26.))
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .when(self.selected == Some(".".to_string()), |this| {
                    this.border_l_2().border_color(colors.border_focused)
                })
                .hover(|style| style.bg(colors.element_hover))
                .child(
                    Label::new(if self.expanded.contains(".") {
                        "▾"
                    } else {
                        "▸"
                    })
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
                )
                .child(
                    Label::new(self.root_label())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Default)
                        .truncate(),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.select_dir(".".to_string(), cx);
                }))
                .into_any_element(),
        );
        let children = self.entries.get(".").cloned();
        if let Some(result) = children {
            match result {
                Ok(entries) => {
                    let mut line = 0usize;
                    self.push_picker_rows(".", entries, 1, &mut line, &mut rows, &query, cx);
                    if rows.len() == 1 && query.is_empty() {
                        rows.push(
                            h_flex()
                                .px_3()
                                .py_1()
                                .child(
                                    Label::new("empty directory")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted),
                                )
                                .into_any_element(),
                        );
                    }
                }
                Err(message) => rows.push(
                    h_flex()
                        .px_3()
                        .py_1()
                        .child(
                            Label::new(message)
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Error)
                                .truncate(),
                        )
                        .into_any_element(),
                ),
            }
        } else {
            rows.push(
                h_flex()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new("loading…")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
        }

        v_flex()
            .id("root-picker")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        Label::new("Search:")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .id("rp-query-box")
                            .flex_1()
                            .min_w_0()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border)
                            .rounded_sm()
                            .px_1()
                            .py_0p5()
                            .cursor_text()
                            .child(dialog_editor_element(&self.query_editor, cx)),
                    ),
            )
            .child(
                div()
                    .id("rp-tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        dialog_button("rp-cancel", "Cancel", DialogButtonVariant::Secondary)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.cancel(window, cx);
                            })),
                    )
                    .child(
                        dialog_button("rp-ok", "OK", DialogButtonVariant::Primary).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            }),
                        ),
                    ),
            )
    }
}

impl RootPickerDialog {
    fn push_picker_rows(
        &self,
        _parent: &str,
        entries: Vec<RemoteEntry>,
        depth: usize,
        line: &mut usize,
        out: &mut Vec<AnyElement>,
        query: &str,
        cx: &Context<Self>,
    ) {
        let colors = cx.theme().colors();
        for entry in entries {
            let matches = picker_query_matches(query, &entry.name);
            let children = self.entries.get(&entry.path).cloned();
            let expanded = self.expanded.contains(&entry.path);
            let descendant_matches = query.trim().is_empty()
                || match children.as_ref() {
                    Some(Ok(children)) => {
                        expanded
                            && (children
                                .iter()
                                .any(|child| picker_query_matches(query, &child.name))
                                || children.iter().any(|child| {
                                    child.is_directory
                                        && self.descendant_matches(query, &child.path)
                                }))
                    }
                    _ => false,
                };
            if !matches && !descendant_matches {
                continue;
            }
            let selected = self.selected == Some(entry.path.clone());
            let path = entry.path.clone();
            let toggle_path = path.clone();
            let is_directory = entry.is_directory;
            let mut item = h_flex()
                .id(("rp-entry", *line))
                .w_full()
                .flex_none()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_sm()
                .cursor_pointer()
                .when(selected, |this| this.bg(colors.element_selected))
                .hover(|style| style.bg(colors.element_hover));
            if is_directory {
                item = item
                    .child(
                        div()
                            .id(("rp-chevron", *line))
                            .flex_none()
                            .w(px(14.))
                            .child(
                                Label::new(if expanded { "▾" } else { "▸" })
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted),
                            )
                            .hover(|style| style.bg(colors.element_hover))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.toggle_dir(toggle_path.clone(), cx);
                            })),
                    )
                    .child(
                        Icon::new(if expanded {
                            IconName::FolderOpen
                        } else {
                            IconName::Folder
                        })
                        .size(IconSize::Small)
                        .color(Color::Accent),
                    );
            } else {
                item = item.child(div().flex_none().w(px(14.))).child(
                    Icon::new(IconName::File)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                );
            }
            item = item.child(
                Label::new(entry.name.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(if selected {
                        Color::Accent
                    } else {
                        Color::Default
                    })
                    .truncate(),
            );
            *line += 1;
            out.push(
                item.on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    if is_directory {
                        this.select_dir(path.clone(), cx);
                    }
                }))
                .into_any_element(),
            );
            if is_directory && expanded {
                if let Some(Ok(children)) = children {
                    self.push_picker_rows(&entry.path, children, depth + 1, line, out, query, cx);
                }
            }
        }
    }

    fn descendant_matches(&self, query: &str, path: &str) -> bool {
        let Some(Ok(children)) = self.entries.get(path) else {
            return false;
        };
        children
            .iter()
            .any(|child| picker_query_matches(query, &child.name))
            || children
                .iter()
                .any(|child| child.is_directory && self.descendant_matches(query, &child.path))
    }
}

pub fn open_root_picker_window(
    root: PathBuf,
    server: String,
    title: String,
    host_display: String,
    local_base: Option<PathBuf>,
    cx: &mut App,
) {
    if !cx.has_global::<RootPickerWindow>() {
        cx.set_global(RootPickerWindow(None));
    }
    let existing = cx.global::<RootPickerWindow>().0;
    if let Some(handle) = existing.and_then(|any| any.downcast::<RootPickerDialog>()) {
        let resumed = handle.update(cx, |picker, window, cx| {
            picker.request = RootPickerRequest {
                root: root.clone(),
                server: server.clone(),
                host_display: host_display.clone(),
                title: title.clone(),
                local_base: local_base.clone(),
            };
            picker.entries.clear();
            picker.expanded.clear();
            picker.selected = None;
            picker.expanded.insert(".".to_string());
            picker.load_dir(".".to_string(), cx);
            window.activate_window();
            cx.notify();
        });
        if resumed.is_ok() {
            return;
        }
        cx.global_mut::<RootPickerWindow>().0 = None;
    }
    let window_bounds = dialog_bounds("root_picker", size(px(560.), px(520.)), cx);
    let request = RootPickerRequest {
        root,
        server,
        host_display,
        title: title.clone(),
        local_base,
    };
    let handle = cx
        .open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from(request.title.clone())),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title(request.title.as_str());
                let view = cx.new(|cx| RootPickerDialog::new(request, window, cx));
                window.activate_window();
                view.focus_handle(cx).focus(window, cx);
                view
            },
        )
        .log_err();
    if let Some(handle) = handle {
        cx.global_mut::<RootPickerWindow>().0 = Some(handle.into());
    }
}

pub fn open_data_sources_window(root: PathBuf, cx: &mut App) {
    open_data_sources_window_seeded(root, None, cx);
}

/// Opens the Data Sources dialog and, when `seed` is set, pre-creates a data
/// source with that driver (used by the Create Data Source menu). The seed
/// only populates the editor; it is persisted by Apply/OK, never here.
fn open_data_sources_window_seeded(
    root: PathBuf,
    seed: Option<DataSourceSeed>,
    cx: &mut App,
) {
    register_dialog_key_bindings(cx, "DataSourceDialog");
    if !cx.has_global::<DataSourcesDialogWindow>() {
        cx.set_global(DataSourcesDialogWindow(None));
    }
    let existing = cx.global::<DataSourcesDialogWindow>().0;
    if let Some(handle) = existing.and_then(|any| any.downcast::<DataSourcesDialog>()) {
        let resumed = handle.update(cx, |dialog, window, cx| {
            dialog.resume_with_root(root.clone(), seed.clone(), window, cx);
            window.activate_window();
            cx.notify();
        });
        if resumed.is_ok() {
            return;
        }
        cx.global_mut::<DataSourcesDialogWindow>().0 = None;
    }

    let window_bounds = dialog_bounds("data_sources", size(px(760.), px(640.)), cx);
    let handle = cx
        .open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::from("Data Sources and Drivers")),
                    ..Default::default()
                }),
                window_bounds: Some(window_bounds),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Data Sources and Drivers");
                let view =
                    cx.new(|cx| DataSourcesDialog::new(root.clone(), seed.clone(), window, cx));
                window.activate_window();
                view.focus_handle(cx).focus(window, cx);
                view
            },
        )
        .log_err();
    if let Some(handle) = handle {
        cx.global_mut::<DataSourcesDialogWindow>().0 = Some(handle.into());
    }
}

impl Render for DataSourcesDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let message = self.message.clone();
        let test_message = self.test_message.clone();
        let selected_source = self
            .selected
            .and_then(|index| self.sources.get(index))
            .cloned();
        let mut problems: Vec<String> = selected_source
            .as_ref()
            .map(data_source_problems)
            .unwrap_or_default();
        if let Some(source) = selected_source.as_ref() {
            if let Some(problem) = ssh_config_problem(source, &self.ssh_configs) {
                problems.push(problem);
            }
        }

        // Rebuild the Tab order for the active tab / driver before any
        // control asks for its focus handle.
        let focus_driver = selected_source
            .as_ref()
            .map(|source| source.driver.clone())
            .unwrap_or_default();
        let focus_ssh_enabled = selected_source
            .as_ref()
            .map(|source| source.ssh_enabled)
            .unwrap_or(false);
        let schema_rows = match &self.schemas_state {
            SchemaListState::Loaded(names) => Some(names.len()),
            _ => None,
        };
        let ds_slots = data_source_focus_slots(
            &focus_driver,
            self.tab,
            focus_ssh_enabled,
            schema_rows,
            self.advanced_rows.len(),
        );
        let mut focus: HashMap<DataSourceFocusSlot, FocusHandle> = HashMap::new();
        for slot in &ds_slots {
            let handle = self.data_source_focus_handle(*slot, cx);
            focus.insert(*slot, handle);
        }
        let order = ds_slots
            .iter()
            .map(|slot| dialog_slot(&focus, *slot))
            .collect::<Vec<_>>();
        let tab_changed = self.focus_tab != self.tab;
        self.focus_tab = self.tab;
        self.focus_order.set_slots(order, tab_changed);

        let mut source_rows = Vec::new();
        for (index, source) in self.sources.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            source_rows.push(
                h_flex()
                    .id(("ds-source-row", index))
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(colors.element_selected))
                    .hover(|style| style.bg(colors.element_hover))
                    .when(!source.driver.is_empty(), |this| {
                        this.child(driver_badge(
                            driver_badge_short(&source.driver),
                            driver_badge_color(&source.driver),
                            cx,
                        ))
                    })
                    .child(
                        Label::new(source.name.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if is_selected {
                                Color::Accent
                            } else {
                                Color::Default
                            })
                            .truncate(),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_source(index, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if source_rows.is_empty() {
            source_rows.push(dialog_static("no data sources", Color::Muted));
        }

        let mut problem_rows = Vec::new();
        for (index, problem) in problems.iter().enumerate() {
            problem_rows.push(
                h_flex()
                    .id(("ds-problem-row", index))
                    .w_full()
                    .flex_none()
                    .px_1()
                    .py_0p5()
                    .child(
                        Label::new(problem.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Error)
                            .truncate(),
                    )
                    .into_any_element(),
            );
        }

        let mut tab_rows = Vec::new();
        for tab in DataSourceTab::ALL {
            let selected = tab == self.tab;
            let tab_slot = match tab {
                DataSourceTab::General => DataSourceFocusSlot::GeneralTab,
                DataSourceTab::Options => DataSourceFocusSlot::OptionsTab,
                DataSourceTab::Ssh => DataSourceFocusSlot::SshTab,
                DataSourceTab::Schemas => DataSourceFocusSlot::SchemasTab,
                DataSourceTab::Advanced => DataSourceFocusSlot::AdvancedTab,
            };
            let tab_focus = dialog_slot(&focus, tab_slot);
            tab_rows.push(
                h_flex()
                    .id(("ds-tab", tab as usize))
                    .flex_none()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .track_focus(&tab_focus)
                    .focus(|style| style.bg(colors.element_hover))
                    .when(selected, |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .when(tab_focus.is_focused(window), |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        if dialog_activation_key(event) {
                            cx.stop_propagation();
                            this.select_tab(tab, window, cx);
                        }
                    }))
                    .child(
                        Label::new(tab.label())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if selected {
                                Color::Default
                            } else {
                                Color::Muted
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_tab(tab, window, cx);
                    }))
                    .into_any_element(),
            );
        }

        let tab_content: AnyElement = match self.tab {
            DataSourceTab::General => {
                let driver = selected_source
                    .as_ref()
                    .map(|source| source.driver.clone())
                    .unwrap_or_default();
                let fields = data_source_general_fields(&driver);
                let is_sqlite = driver == "sqlite";
                let mut rows = Vec::new();

                rows.push(dialog_row(
                    "Driver:",
                    h_flex()
                        .id("ds-driver-picker")
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .py_0p5()
                        .rounded_sm()
                        .cursor_pointer()
                        .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::Driver))
                        .focus(|style| style.bg(colors.element_hover))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if dialog_activation_key(event) {
                                cx.stop_propagation();
                                this.toggle_driver_menu(window, cx);
                            }
                        }))
                        .hover(|style| style.bg(colors.element_hover))
                        .when(!driver.is_empty(), |this| {
                            this.child(driver_badge(
                                driver_badge_short(&driver),
                                driver_badge_color(&driver),
                                cx,
                            ))
                        })
                        .when(driver.is_empty(), |this| {
                            this.child(
                                Label::new("Select driver")
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted),
                            )
                        })
                        .when(!driver.is_empty(), |this| {
                            this.child(
                                Label::new(driver_display_name(&driver))
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .truncate(),
                            )
                        })
                        .child(
                            Label::new(driver_support_hint(&driver))
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Muted),
                        )
                        .child(div().flex_1().min_w_0())
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_driver_menu(window, cx);
                        })),
                ));

                if self.driver_menu_open && selected_source.is_some() {
                    let mut menu_rows = Vec::new();
                    for (menu_index, (menu_driver, is_current)) in
                        driver_picker_rows(&driver).into_iter().enumerate()
                    {
                        menu_rows.push(
                            h_flex()
                                .id(("ds-driver-row", menu_index))
                                .w_full()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_0p5()
                                .cursor_pointer()
                                .when(is_current, |this| this.bg(colors.element_selected))
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .child(driver_badge(
                                    driver_badge_short(menu_driver),
                                    driver_badge_color(menu_driver),
                                    cx,
                                ))
                                .child(
                                    Label::new(driver_display_name(menu_driver))
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(if is_current {
                                            Color::Accent
                                        } else {
                                            Color::Default
                                        })
                                        .truncate(),
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.select_driver(menu_driver.to_string(), window, cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    rows.push(
                        v_flex()
                            .id("ds-driver-menu")
                            .w_full()
                            .flex_none()
                            .gap_0p5()
                            .px_1()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.elevated_surface_background)
                            .shadow_sm()
                            .max_h(px(220.))
                            .overflow_y_scroll()
                            .children(menu_rows)
                            .into_any_element(),
                    );
                }

                if is_sqlite && selected_source.is_some() {
                    let sqlite_path = selected_source
                        .as_ref()
                        .map(|source| source.path.trim())
                        .unwrap_or_default();
                    rows.push(dialog_row(
                        "Data source file:",
                        Label::new(if sqlite_path.is_empty() {
                            "<not set — add a file from the project tree>"
                        } else {
                            sqlite_path
                        })
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted)
                        .truncate(),
                    ));
                }

                let mut front_len = fields.len();
                for (index, field) in fields.iter().enumerate() {
                    if !matches!(field, DsGeneralField::Host | DsGeneralField::Port) {
                        front_len = index;
                        break;
                    }
                }
                let (front, rest) = fields.split_at(front_len);
                for field in front {
                    rows.push(match field {
                        DsGeneralField::Host => dialog_row(
                            "Host:",
                            dialog_editor("ds-host-editor", &self.host_editor, None, window, cx),
                        ),
                        DsGeneralField::Port => dialog_row(
                            "Port:",
                            dialog_editor(
                                "ds-port-editor",
                                &self.port_editor,
                                Some(px(60.)),
                                window,
                                cx,
                            ),
                        ),
                        _ => continue,
                    });
                }
                rows.push(dialog_row(
                    "Authentication:",
                    Label::new(if is_sqlite {
                        "file-based"
                    } else {
                        "User & Password"
                    })
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
                ));
                for field in rest {
                    rows.push(match field {
                        DsGeneralField::User => dialog_row(
                            "User:",
                            dialog_editor("ds-user-editor", &self.user_editor, None, window, cx),
                        ),
                        DsGeneralField::Password => {
                            let masked = selected_source
                                .as_ref()
                                .map(|source| mask_password(&source.password).to_string())
                                .unwrap_or_default();
                            dialog_row(
                                "Password:",
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .items_center()
                                    .gap_2()
                                    .child(dialog_editor(
                                        "ds-password-editor",
                                        &self.password_editor,
                                        None,
                                        window,
                                        cx,
                                    ))
                                    .child(
                                        Label::new(masked)
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    ),
                            )
                        }
                        DsGeneralField::Database => dialog_row(
                            "Database:",
                            dialog_editor(
                                "ds-database-editor",
                                &self.database_editor,
                                None,
                                window,
                                cx,
                            ),
                        ),
                        _ => continue,
                    });
                }
                if !is_sqlite {
                    rows.push(dialog_row(
                        "URL:",
                        dialog_editor("ds-url-editor", &self.url_editor, None, window, cx),
                    ));
                    rows.push(dialog_static("Overrides settings above", Color::Muted));
                }
                v_flex()
                    .id("ds-tab-general")
                    .w_full()
                    .gap_2()
                    .children(rows)
                    .into_any_element()
            }
            DataSourceTab::Options => {
                let read_only = selected_source
                    .as_ref()
                    .map(|source| source.read_only)
                    .unwrap_or(false);
                v_flex()
                    .id("ds-tab-options")
                    .w_full()
                    .gap_1()
                    .children(vec![
                        dialog_row(
                            "Read-only:",
                            dialog_button(
                                "ds-option-readonly",
                                if read_only { "read-only" } else { "read-write" },
                                DialogButtonVariant::Secondary,
                            )
                            .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::ReadOnly))
                            .on_click(cx.listener(
                                |this, _: &ClickEvent, _window, cx| {
                                    this.toggle_read_only(cx);
                                },
                            )),
                        ),
                        dialog_static("auto-disconnect: not supported", Color::Muted),
                    ])
                    .into_any_element()
            }
            DataSourceTab::Ssh => {
                let ssh_enabled = selected_source
                    .as_ref()
                    .map(|source| source.ssh_enabled)
                    .unwrap_or(false);
                let mut rows = Vec::new();
                rows.push(
                    dialog_toggle_row("ds-ssh-toggle", ssh_enabled, "Use SSH tunnel", cx)
                        .w_full()
                        .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::SshToggle))
                        .focus(|style| style.bg(colors.element_hover))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                            if dialog_activation_key(event) {
                                cx.stop_propagation();
                                this.toggle_ssh(cx);
                            }
                        }))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.toggle_ssh(cx);
                        }))
                        .into_any_element(),
                );
                if ssh_enabled {
                    let config_raw = selected_source
                        .as_ref()
                        .map(|source| {
                            source
                                .ssh_config
                                .clone()
                                .unwrap_or_else(|| source.ssh_name.trim().to_string())
                        })
                        .unwrap_or_default();
                    let config_name = if config_raw.is_empty() {
                        String::new()
                    } else {
                        ssh_config_display_name(&config_raw)
                    };
                    rows.push(dialog_row(
                        "SSH configuration:",
                        h_flex()
                            .id("ds-ssh-config")
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(
                                dialog_button(
                                    "ds-ssh-config-select",
                                    if config_name.is_empty() {
                                        "<select configuration>".to_string()
                                    } else {
                                        config_name.clone()
                                    },
                                    if config_name.is_empty() {
                                        DialogButtonVariant::Link
                                    } else {
                                        DialogButtonVariant::Primary
                                    },
                                )
                                .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::SshConfig))
                                .end_icon(
                                    Icon::new(IconName::ChevronDown)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, _window, cx| {
                                        this.toggle_ssh_menu(cx);
                                    },
                                )),
                            )
                            .child(
                                IconButton::new("ds-ssh-edit", IconName::Ellipsis)
                                    .shape(IconButtonShape::Square)
                                    .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::SshEdit))
                                    .icon_size(IconSize::Small)
                                    .icon_color(Color::Muted)
                                    .tooltip(Tooltip::text("Edit SSH configurations"))
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.open_ssh_configs(window, cx);
                                    })),
                            ),
                    ));
                    if self.ssh_menu_open {
                        for (index, config) in self.ssh_configs.iter().enumerate() {
                            let is_active = config_raw == config.name;
                            rows.push(
                                h_flex()
                                    .id(("ds-ssh-config-row", index))
                                    .w_full()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(is_active, |this| this.bg(colors.element_selected))
                                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                                    .child(
                                        Label::new(config.name.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(ssh_user_host_text(config))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _window, cx| {
                                            this.select_ssh_config(index, cx);
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                        rows.push(
                            h_flex()
                                .id("ds-ssh-config-add")
                                .w_full()
                                .flex_none()
                                .items_center()
                                .px_2()
                                .py_0p5()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .child(
                                    Label::new("Add new configuration...")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.open_ssh_configs(window, cx);
                                }))
                                .into_any_element(),
                        );
                        rows.push(dialog_section_header("From ~/.ssh/config"));
                        if self.openssh_hosts.is_empty() {
                            rows.push(dialog_static("no host aliases found", Color::Muted));
                        }
                        for (index, host) in self.openssh_hosts.iter().enumerate() {
                            let is_active = config_raw == format!("openssh:{}", host.alias);
                            rows.push(
                                h_flex()
                                    .id(("ds-openssh-row", index))
                                    .w_full()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_sm()
                                    .cursor_pointer()
                                    .when(is_active, |this| this.bg(colors.element_selected))
                                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                                    .child(dialog_badge("ssh", Color::Accent, cx))
                                    .child(
                                        Label::new(host.alias.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(openssh_user_host_text(host))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _window, cx| {
                                            this.select_openssh_config(index, cx);
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                    }
                    if let Some(hint) = self.ssh_config_missing.clone() {
                        rows.push(dialog_static(hint, Color::Warning));
                    }
                    rows.push(dialog_row(
                        "Local port:",
                        dialog_editor(
                            "ds-ssh-local-port-editor",
                            &self.ssh_local_port_editor,
                            Some(px(80.)),
                            window,
                            cx,
                        ),
                    ));
                }
                rows.push(dialog_static("SSL: not supported", Color::Muted));
                v_flex()
                    .id("ds-tab-ssh")
                    .w_full()
                    .children(rows)
                    .into_any_element()
            }
            DataSourceTab::Schemas => {
                let mut rows = Vec::new();
                rows.push(
                    h_flex()
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .child(
                            IconButton::new("ds-schemas-refresh", IconName::RotateCw)
                                .shape(IconButtonShape::Square)
                                .icon_size(IconSize::Small)
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.refresh_schemas(cx);
                                })),
                        )
                        .child(
                            Label::new("auto")
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Muted),
                        )
                        .into_any_element(),
                );
                let excluded = selected_source
                    .as_ref()
                    .map(|source| source.excluded_schemas.clone())
                    .unwrap_or_default();
                match &self.schemas_state {
                    SchemaListState::Idle => rows.push(
                        h_flex()
                            .px_1()
                            .py_0p5()
                            .child(
                                Label::new("press refresh to load schemas")
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted),
                            )
                            .into_any_element(),
                    ),
                    SchemaListState::Loading => rows.push(
                        h_flex()
                            .px_1()
                            .py_0p5()
                            .child(
                                Label::new("loading…")
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted),
                            )
                            .into_any_element(),
                    ),
                    SchemaListState::Failed(error) => rows.push(
                        h_flex()
                            .px_1()
                            .py_0p5()
                            .child(
                                Label::new(error.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Error)
                                    .truncate(),
                            )
                            .into_any_element(),
                    ),
                    SchemaListState::Loaded(names) => {
                        rows.push(
                            dialog_toggle_row(
                                "ds-schema-row-all",
                                excluded.is_empty(),
                                "All schemas",
                                cx,
                            )
                            .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::SchemasAll))
                            .focus(|style| style.bg(colors.element_hover))
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                                if dialog_activation_key(event) {
                                    cx.stop_propagation();
                                    this.toggle_all_schemas(cx);
                                }
                            }))
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_all_schemas(cx);
                            }))
                            .into_any_element(),
                        );
                        for (index, name) in names.iter().enumerate() {
                            let checked = schema_checked(&excluded, name);
                            let name = name.clone();
                            let schema_name = name.clone();
                            rows.push(
                                dialog_toggle_row(
                                    ("ds-schema-row", index),
                                    checked,
                                    name.clone(),
                                    cx,
                                )
                                .track_focus(&dialog_slot(
                                    &focus,
                                    DataSourceFocusSlot::Schema(index),
                                ))
                                .focus(|style| style.bg(colors.element_hover))
                                .on_key_down(cx.listener(
                                    move |this, event: &KeyDownEvent, _window, cx| {
                                        if dialog_activation_key(event) {
                                            cx.stop_propagation();
                                            this.toggle_schema(schema_name.clone(), cx);
                                        }
                                    },
                                ))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                    this.toggle_schema(name.clone(), cx);
                                }))
                                .into_any_element(),
                            );
                        }
                    }
                }
                v_flex()
                    .id("ds-tab-schemas")
                    .w_full()
                    .children(rows)
                    .into_any_element()
            }
            DataSourceTab::Advanced => {
                let mut rows = Vec::new();
                for (index, (key_editor, value_editor)) in self.advanced_rows.iter().enumerate() {
                    rows.push(
                        h_flex()
                            .w_full()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .py_0p5()
                            .child(dialog_editor(
                                ("ds-advanced-name", index),
                                key_editor,
                                None,
                                window,
                                cx,
                            ))
                            .child(dialog_editor(
                                ("ds-advanced-value", index),
                                value_editor,
                                None,
                                window,
                                cx,
                            ))
                            .into_any_element(),
                    );
                }
                rows.push(
                    h_flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .py_1()
                        .child(
                            IconButton::new("ds-advanced-add", IconName::Plus)
                                .shape(IconButtonShape::Square)
                                .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::AdvancedAdd))
                                .icon_size(IconSize::Small)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.add_advanced_row(window, cx);
                                })),
                        )
                        .child(
                            IconButton::new("ds-advanced-remove", IconName::SquareMinus)
                                .shape(IconButtonShape::Square)
                                .track_focus(&dialog_slot(
                                    &focus,
                                    DataSourceFocusSlot::AdvancedRemove,
                                ))
                                .icon_size(IconSize::Small)
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.remove_advanced_row(window, cx);
                                })),
                        )
                        .into_any_element(),
                );
                v_flex()
                    .id("ds-tab-advanced")
                    .w_full()
                    .children(rows)
                    .into_any_element()
            }
        };

        let test_button = div()
            .flex_none()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                if dialog_activation_key(event) {
                    cx.stop_propagation();
                    this.test_connection(cx);
                }
            }))
            .child(
                dialog_button("ds-test", "Test Connection", DialogButtonVariant::Link)
                    .track_focus(&dialog_slot(&focus, DataSourceFocusSlot::TestConnection))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.test_connection(cx);
                    })),
            );

        let message_line: Option<AnyElement> = message.as_ref().map(|(text, color)| {
            h_flex()
                .flex_none()
                .px_3()
                .py_1()
                .child(
                    Label::new(text.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(*color)
                        .truncate(),
                )
                .into_any_element()
        });

        v_flex()
            .id("data-sources-dialog")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .key_context("DataSourceDialog")
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
            }))
            .on_action(cx.listener(Self::focus_next_field))
            .on_action(cx.listener(Self::focus_previous_field))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .w(px(200.))
                            .flex_none()
                            .h_full()
                            .border_r_1()
                            .border_color(colors.border)
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_between()
                                    .px_2()
                                    .py_1()
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::DatabaseZap)
                                                    .size(IconSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(
                                                Label::new("Data Sources")
                                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                    .color(Color::Default),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .items_center()
                                            .gap_0p5()
                                            .child(
                                                IconButton::new("ds-add", IconName::Plus)
                                                    .shape(IconButtonShape::Square)
                                                    .icon_size(IconSize::Small)
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, window, cx| {
                                                            this.add_source(window, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                IconButton::new("ds-remove", IconName::SquareMinus)
                                                    .shape(IconButtonShape::Square)
                                                    .icon_size(IconSize::Small)
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, window, cx| {
                                                            this.remove_source(window, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                IconButton::new("ds-copy", IconName::Copy)
                                                    .shape(IconButtonShape::Square)
                                                    .icon_size(IconSize::Small)
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, window, cx| {
                                                            this.copy_source(window, cx);
                                                        },
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("ds-source-list")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .children(source_rows),
                            )
                            .child(
                                v_flex()
                                    .flex_none()
                                    .border_t_1()
                                    .border_color(colors.border)
                                    .px_2()
                                    .py_1()
                                    .child(
                                        Label::new("Problems")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    )
                                    .children(problem_rows),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .child(
                                        Label::new("Name:")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    )
                                    .child(dialog_editor(
                                        "ds-name-editor",
                                        &self.name_editor,
                                        None,
                                        window,
                                        cx,
                                    )),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .items_center()
                                    .px_2()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .children(tab_rows),
                            )
                            .child(
                                div()
                                    .id("ds-tab-content")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .p_2()
                                    .child(tab_content),
                            ),
                    ),
            )
            .children(message_line)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(test_button)
                            .children(test_message.as_ref().map(|(text, color)| {
                                Label::new(text.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(*color)
                                    .flex_1()
                                    .truncate()
                            })),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.cancel(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ds-cancel",
                                            "Cancel",
                                            DialogButtonVariant::Secondary,
                                        )
                                        .track_focus(&dialog_slot(
                                            &focus,
                                            DataSourceFocusSlot::Cancel,
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.cancel(window, cx);
                                            },
                                        )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.apply(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ds-apply",
                                            "Apply",
                                            DialogButtonVariant::Secondary,
                                        )
                                        .track_focus(&dialog_slot(
                                            &focus,
                                            DataSourceFocusSlot::Apply,
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.apply(window, cx);
                                            },
                                        )),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .on_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            if dialog_activation_key(event) {
                                                cx.stop_propagation();
                                                this.confirm(window, cx);
                                            }
                                        },
                                    ))
                                    .child(
                                        dialog_button(
                                            "ds-ok",
                                            "OK",
                                            DialogButtonVariant::Primary,
                                        )
                                        .track_focus(&dialog_slot(
                                            &focus,
                                            DataSourceFocusSlot::Ok,
                                        ))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.confirm(window, cx);
                                            },
                                        )),
                                    ),
                            ),
                    ),
            )
    }
}

const SERVER_PROTOCOLS: [&str; 2] = ["local", "sftp"];
const DATABASE_DRIVERS: [&str; 8] = [
    "postgres",
    "mysql",
    "mariadb",
    "sqlite",
    "sqlserver",
    "oracle",
    "mongodb",
    "redis",
];

/// Driver order for the Databases "+" Create Data Source menu (PhpStorm order).
const CREATE_DATA_SOURCE_DRIVERS: [&str; 8] = [
    "postgres",
    "mysql",
    "sqlserver",
    "mariadb",
    "sqlite",
    "oracle",
    "mongodb",
    "redis",
];

/// One of the bundled demo database rows in the Create Data Source menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DemoDatabase {
    Postgres,
    MySql,
    Sqlite,
}

impl DemoDatabase {
    const ALL: [DemoDatabase; 3] = [Self::Postgres, Self::MySql, Self::Sqlite];

    fn label(self) -> &'static str {
        match self {
            Self::Postgres => "Demo PostgreSQL",
            Self::MySql => "Demo MySQL",
            Self::Sqlite => "Demo SQLite",
        }
    }

    fn driver(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }

    /// Demo rows never fabricate a connection: PostgreSQL/MySQL need a local
    /// server, and SQLite only works when `demo.sqlite` already exists.
    fn enabled(self, sqlite_demo_exists: bool) -> bool {
        matches!(self, Self::Sqlite) && sqlite_demo_exists
    }

    fn disabled_reason(self) -> Option<&'static str> {
        match self {
            Self::Postgres | Self::MySql => Some("requires a local server"),
            Self::Sqlite => Some("demo.sqlite not found in project"),
        }
    }
}

/// An actionable row in the Create Data Source menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CreateDataSourceAction {
    Driver(&'static str),
    AllDataSources,
    Demo(DemoDatabase),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CreateDataSourceMenuRow {
    action: CreateDataSourceAction,
    enabled: bool,
}

/// Pure model for the Databases "+" menu: driver rows in PhpStorm order, then
/// All Data Sources, then the demo section.
fn create_data_source_menu(sqlite_demo_exists: bool) -> Vec<CreateDataSourceMenuRow> {
    let mut rows = Vec::new();
    for driver in CREATE_DATA_SOURCE_DRIVERS {
        rows.push(CreateDataSourceMenuRow {
            action: CreateDataSourceAction::Driver(driver),
            enabled: true,
        });
    }
    rows.push(CreateDataSourceMenuRow {
        action: CreateDataSourceAction::AllDataSources,
        enabled: true,
    });
    for demo in DemoDatabase::ALL {
        rows.push(CreateDataSourceMenuRow {
            action: CreateDataSourceAction::Demo(demo),
            enabled: demo.enabled(sqlite_demo_exists),
        });
    }
    rows
}

/// PhpStorm-style display names for the Create Data Source menu.
fn create_data_source_driver_label(driver: &str) -> &'static str {
    match driver {
        "postgres" => "PostgreSQL",
        "mysql" => "MySQL",
        "sqlserver" => "Microsoft SQL Server",
        "mariadb" => "MariaDB",
        "sqlite" => "SQLite",
        "oracle" => "Oracle",
        "mongodb" => "MongoDB",
        "redis" => "Redis",
        _ => "Unknown",
    }
}

#[derive(Clone)]
pub(crate) struct DatabaseColumn {
    pub(crate) name: String,
    pub(crate) column_type: String,
    pub(crate) primary_key: bool,
}

#[derive(Clone)]
struct DatabaseTable {
    name: String,
    columns: Vec<DatabaseColumn>,
}

enum DatabaseStatus {
    Loading,
    Loaded(Vec<DatabaseTable>),
    Failed(String),
}

enum DbListStatus {
    Loading,
    Loaded(Vec<String>),
    Failed(String),
}

enum TableStatus {
    Loading,
    Loaded(Vec<serde_json::Map<String, Value>>),
    Failed(String),
}

#[derive(Clone)]
struct HttpRequest {
    file: ProjectPath,
    absolute_path: PathBuf,
    name: String,
    method: String,
    url: String,
    root: PathBuf,
    line: usize,
}

#[derive(Clone)]
struct CollectionRequest {
    collection_path: PathBuf,
    env_path: Option<PathBuf>,
    folder: Vec<String>,
    name: String,
    method: String,
    url: String,
    request_id: String,
    root: PathBuf,
}

#[derive(Clone)]
struct PostmanCollection {
    name: String,
    requests: Vec<CollectionRequest>,
}

#[derive(Clone)]
enum SyncStatus {
    Syncing,
    Ok,
    Failed(String),
}

enum HttpStatus {
    Running,
    Status(u16),
    Failed(String),
    Response(HttpProbeResponse),
}

#[derive(Clone)]
struct HttpProbeResponse {
    status: u16,
    time_ms: u64,
    headers: Vec<(String, String)>,
    body: String,
}

enum TestListStatus {
    Unloaded,
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestStatus {
    Pass,
    Fail,
    Error,
    Skip,
}

impl TestStatus {
    fn parse(value: &str) -> TestStatus {
        match value {
            "pass" | "passed" => TestStatus::Pass,
            "error" => TestStatus::Error,
            "skip" | "skipped" => TestStatus::Skip,
            _ => TestStatus::Fail,
        }
    }

    fn label(self) -> &'static str {
        match self {
            TestStatus::Pass => "✓",
            TestStatus::Fail => "✗",
            TestStatus::Error => "!",
            TestStatus::Skip => "–",
        }
    }

    fn color(self) -> Color {
        match self {
            TestStatus::Pass => Color::Success,
            TestStatus::Fail | TestStatus::Error => Color::Error,
            TestStatus::Skip => Color::Muted,
        }
    }
}

#[derive(Clone)]
struct TestCase {
    class: String,
    name: String,
    file: String,
    status: TestStatus,
    message: String,
}

#[derive(Clone, Copy, Default)]
struct TestSummary {
    total: usize,
    passed: usize,
    failed: usize,
    errors: usize,
    skipped: usize,
}

impl TestSummary {
    fn text(self) -> String {
        let mut parts = vec![
            format!("{} passed", self.passed),
            format!("{} failed", self.failed),
        ];
        if self.errors > 0 {
            parts.push(format!("{} errors", self.errors));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        parts.join(" · ")
    }
}

struct TestRunResult {
    tests: Vec<TestCase>,
    summary: TestSummary,
}

enum TestRunState {
    Running,
    Finished(TestRunResult),
    Failed(String),
}

fn parse_request_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let first = parts.next()?;
    let method = first.to_ascii_uppercase();
    let is_known_method = matches!(
        method.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS" | "TRACE" | "CONNECT"
    );
    if is_known_method {
        let url = parts.next()?;
        return Some((method, url.to_string()));
    }
    if first.starts_with("http://")
        || first.starts_with("https://")
        || first.starts_with("{{")
        || first.starts_with('/')
    {
        return Some(("GET".to_string(), first.to_string()));
    }
    None
}

fn parse_http_requests(
    contents: &str,
    file: &ProjectPath,
    absolute_path: &Path,
    root: &Path,
) -> Vec<HttpRequest> {
    let mut requests = Vec::new();
    let mut section_name: Option<String> = None;
    let mut section_line = 0;
    let mut request_started = false;
    let mut request_index = 0;

    for (line_index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if let Some(rest) = line.strip_prefix("###") {
            section_name = Some(rest.trim().to_string());
            section_line = line_index + 1;
            request_started = false;
            continue;
        }
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("//")
            || line.starts_with('@')
        {
            continue;
        }
        if request_started {
            continue;
        }
        let Some((method, url)) = parse_request_line(line) else {
            continue;
        };
        request_started = true;
        request_index += 1;
        let name = section_name
            .take()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("{method}-{request_index}"));
        requests.push(HttpRequest {
            file: file.clone(),
            absolute_path: absolute_path.to_path_buf(),
            name,
            method,
            url,
            root: root.to_path_buf(),
            line: if section_line == 0 {
                line_index + 1
            } else {
                section_line
            },
        });
    }

    requests
}

fn http_request_key(request: &HttpRequest) -> String {
    format!("{}:{}", request.absolute_path.display(), request.line)
}

fn is_postman_collection(path: &str) -> bool {
    path.ends_with(".postman_collection.json")
}

fn is_postman_environment(path: &str) -> bool {
    path.ends_with(".postman_environment.json")
}

fn collection_request_url(request: &Value) -> String {
    match request.get("url") {
        Some(Value::String(url)) => url.clone(),
        Some(Value::Object(url)) => url
            .get("raw")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn collect_postman_items(
    items: &[Value],
    collection_path: &Path,
    env_path: Option<&Path>,
    root: &Path,
    folders: &mut Vec<String>,
    requests: &mut Vec<CollectionRequest>,
) {
    for item in items {
        if let Some(children) = item.get("item").and_then(Value::as_array) {
            folders.push(string_field(item, "name").unwrap_or_else(|| "folder".to_string()));
            collect_postman_items(children, collection_path, env_path, root, folders, requests);
            folders.pop();
            continue;
        }

        let Some(request) = item.get("request") else {
            continue;
        };
        let request_id = string_field(item, "id")
            .or_else(|| string_field(item, "name"))
            .unwrap_or_else(|| format!("request-{}", requests.len() + 1));
        let name = string_field(item, "name").unwrap_or_else(|| request_id.clone());
        let (method, url) = match request {
            Value::String(url) => ("GET".to_string(), url.clone()),
            _ => (
                string_field(request, "method").unwrap_or_else(|| "GET".to_string()),
                collection_request_url(request),
            ),
        };

        requests.push(CollectionRequest {
            collection_path: collection_path.to_path_buf(),
            env_path: env_path.map(Path::to_path_buf),
            folder: folders.clone(),
            name,
            method,
            url,
            request_id,
            root: root.to_path_buf(),
        });
    }
}

fn parse_postman_collection(
    contents: &str,
    collection_path: &Path,
    env_path: Option<PathBuf>,
    root: &Path,
) -> PostmanCollection {
    let fallback_name = collection_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Collection")
        .trim_end_matches(".postman_collection.json")
        .to_string();
    let mut collection = PostmanCollection {
        name: fallback_name,
        requests: Vec::new(),
    };

    let Ok(value) = serde_json::from_str::<Value>(contents) else {
        return collection;
    };
    if let Some(name) = value
        .get("info")
        .and_then(|info| string_field(info, "name"))
    {
        collection.name = name;
    }
    if let Some(items) = value.get("item").and_then(Value::as_array) {
        collect_postman_items(
            items,
            collection_path,
            env_path.as_deref(),
            root,
            &mut Vec::new(),
            &mut collection.requests,
        );
    }

    collection
}

fn collection_request_key(request: &CollectionRequest) -> String {
    format!(
        "{}:{}:{}",
        request.collection_path.display(),
        request.folder.join("/"),
        request.request_id
    )
}

fn collection_response_status(response: &Value) -> Option<u16> {
    response
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| results.first())
        .and_then(|result| result.get("status"))
        .or_else(|| response.get("status"))
        .and_then(Value::as_u64)
        .map(|status| status as u16)
}

fn parse_test_files(stdout: &str) -> std::result::Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines())
            .unwrap_or_else(|| "invalid JSON from php-test".to_string())
    })?;
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "test list had no files".to_string())?;
    Ok(files
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}

fn parse_test_run(stdout: &str) -> std::result::Result<TestRunResult, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines())
            .unwrap_or_else(|| "invalid JSON from php-test".to_string())
    })?;
    let tests = value
        .get("tests")
        .and_then(Value::as_array)
        .ok_or_else(|| "test run had no tests".to_string())?;

    let mut cases = Vec::with_capacity(tests.len());
    for test in tests {
        let status = test.get("status").and_then(Value::as_str).unwrap_or("fail");
        cases.push(TestCase {
            class: test
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: test
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            file: test
                .get("file")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            status: TestStatus::parse(status),
            message: test
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }

    let summary_value = value.get("summary");
    let summary_number = |key: &str| {
        summary_value
            .and_then(|summary| summary.get(key))
            .and_then(Value::as_u64)
            .map(|value| value as usize)
    };
    let count = |status: TestStatus| cases.iter().filter(|case| case.status == status).count();
    let summary = TestSummary {
        total: summary_number("total").unwrap_or(cases.len()),
        passed: summary_number("passed").unwrap_or_else(|| count(TestStatus::Pass)),
        failed: summary_number("failed").unwrap_or_else(|| count(TestStatus::Fail)),
        errors: summary_number("errors").unwrap_or_else(|| count(TestStatus::Error)),
        skipped: summary_number("skipped").unwrap_or_else(|| count(TestStatus::Skip)),
    };

    Ok(TestRunResult {
        tests: cases,
        summary,
    })
}

pub(crate) fn first_non_empty_line<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    lines
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HttpSubTab {
    Params,
    Authorization,
    Headers,
    Body,
    Scripts,
    Settings,
}

impl HttpSubTab {
    const ALL: [HttpSubTab; 6] = [
        HttpSubTab::Params,
        HttpSubTab::Authorization,
        HttpSubTab::Headers,
        HttpSubTab::Body,
        HttpSubTab::Scripts,
        HttpSubTab::Settings,
    ];

    fn label(self) -> &'static str {
        match self {
            HttpSubTab::Params => "Params",
            HttpSubTab::Authorization => "Authorization",
            HttpSubTab::Headers => "Headers",
            HttpSubTab::Body => "Body",
            HttpSubTab::Scripts => "Scripts",
            HttpSubTab::Settings => "Settings",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HttpAuthType {
    None,
    ApiKey,
    Basic,
    Bearer,
    OAuth2,
}

impl HttpAuthType {
    const ALL: [HttpAuthType; 5] = [
        HttpAuthType::None,
        HttpAuthType::ApiKey,
        HttpAuthType::Basic,
        HttpAuthType::Bearer,
        HttpAuthType::OAuth2,
    ];

    fn label(self) -> &'static str {
        match self {
            HttpAuthType::None => "No Auth",
            HttpAuthType::ApiKey => "API Key",
            HttpAuthType::Basic => "Basic Auth",
            HttpAuthType::Bearer => "Bearer Token",
            HttpAuthType::OAuth2 => "OAuth 2.0",
        }
    }

    fn key(self) -> &'static str {
        match self {
            HttpAuthType::None => "none",
            HttpAuthType::ApiKey => "api-key",
            HttpAuthType::Basic => "basic",
            HttpAuthType::Bearer => "bearer",
            HttpAuthType::OAuth2 => "oauth2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ApiKeyLocation {
    Header,
    Query,
}

impl ApiKeyLocation {
    fn label(self) -> &'static str {
        match self {
            ApiKeyLocation::Header => "Header",
            ApiKeyLocation::Query => "Query",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HttpBodyKind {
    None,
    Raw,
    FormData,
    UrlEncoded,
}

impl HttpBodyKind {
    const ALL: [HttpBodyKind; 4] = [
        HttpBodyKind::None,
        HttpBodyKind::Raw,
        HttpBodyKind::FormData,
        HttpBodyKind::UrlEncoded,
    ];

    fn label(self) -> &'static str {
        match self {
            HttpBodyKind::None => "none",
            HttpBodyKind::Raw => "raw",
            HttpBodyKind::FormData => "form-data",
            HttpBodyKind::UrlEncoded => "x-www-form-urlencoded",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RawContentType {
    Json,
    Xml,
    Text,
    Html,
    JavaScript,
}

impl RawContentType {
    const ALL: [RawContentType; 5] = [
        RawContentType::Json,
        RawContentType::Xml,
        RawContentType::Text,
        RawContentType::Html,
        RawContentType::JavaScript,
    ];

    fn label(self) -> &'static str {
        match self {
            RawContentType::Json => "JSON",
            RawContentType::Xml => "XML",
            RawContentType::Text => "Text",
            RawContentType::Html => "HTML",
            RawContentType::JavaScript => "JavaScript",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            RawContentType::Json => "application/json",
            RawContentType::Xml => "application/xml",
            RawContentType::Text => "text/plain",
            RawContentType::Html => "text/html",
            RawContentType::JavaScript => "application/javascript",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HttpResponseTab {
    Body,
    Headers,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct KeyValueRow {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: String,
}

fn default_true() -> bool {
    true
}

impl KeyValueRow {
    fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            enabled: true,
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HttpAuthSettings {
    #[serde(default = "default_auth_type")]
    kind: HttpAuthType,
    #[serde(default)]
    api_key_name: String,
    #[serde(default)]
    api_key_value: String,
    #[serde(default = "default_api_key_location")]
    api_key_in: ApiKeyLocation,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    token: String,
}

fn default_auth_type() -> HttpAuthType {
    HttpAuthType::None
}

fn default_api_key_location() -> ApiKeyLocation {
    ApiKeyLocation::Header
}

impl Default for HttpAuthSettings {
    fn default() -> Self {
        Self {
            kind: HttpAuthType::None,
            api_key_name: String::new(),
            api_key_value: String::new(),
            api_key_in: ApiKeyLocation::Header,
            username: String::new(),
            password: String::new(),
            token: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct HttpRequestSettings {
    #[serde(default)]
    method: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    subtab: Option<HttpSubTab>,
    #[serde(default)]
    params: Vec<KeyValueRow>,
    #[serde(default)]
    auth: HttpAuthSettings,
    #[serde(default)]
    headers: Vec<KeyValueRow>,
    #[serde(default = "default_body_kind")]
    body_kind: HttpBodyKind,
    #[serde(default = "default_raw_content_type")]
    raw_content_type: RawContentType,
    #[serde(default)]
    raw_body: String,
    #[serde(default)]
    body_fields: Vec<KeyValueRow>,
    #[serde(default)]
    follow_redirects: bool,
    #[serde(default = "default_timeout")]
    timeout_secs: u32,
    #[serde(default = "default_true")]
    verify_tls: bool,
}

fn default_body_kind() -> HttpBodyKind {
    HttpBodyKind::None
}

fn default_raw_content_type() -> RawContentType {
    RawContentType::Json
}

fn default_timeout() -> u32 {
    30
}

impl Default for HttpRequestSettings {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url: String::new(),
            subtab: Some(HttpSubTab::Params),
            params: Vec::new(),
            auth: HttpAuthSettings::default(),
            headers: Vec::new(),
            body_kind: HttpBodyKind::None,
            raw_content_type: RawContentType::Json,
            raw_body: String::new(),
            body_fields: Vec::new(),
            follow_redirects: false,
            timeout_secs: default_timeout(),
            verify_tls: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HttpSettingsFile {
    #[serde(default = "default_settings_version")]
    version: u32,
    #[serde(default)]
    requests: BTreeMap<String, HttpRequestSettings>,
}

fn default_settings_version() -> u32 {
    1
}

fn http_settings_key(file_path: &str, name: &str) -> String {
    format!("{file_path}::{name}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpPairKind {
    Params,
    Headers,
    BodyFields,
}

#[derive(Clone)]
struct PairEditors {
    enabled: bool,
    key: Entity<Editor>,
    value: Entity<Editor>,
}

struct HttpBuilderEditors {
    url: Entity<Editor>,
    raw_body: Entity<Editor>,
    auth_api_key_name: Entity<Editor>,
    auth_api_key_value: Entity<Editor>,
    auth_username: Entity<Editor>,
    auth_password: Entity<Editor>,
    auth_token: Entity<Editor>,
    timeout: Entity<Editor>,
    params: Vec<PairEditors>,
    headers: Vec<PairEditors>,
    body_fields: Vec<PairEditors>,
}

pub struct PhpPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
    /// Open table-data tabs so clicking the same table focuses the tab again.
    open_table_views: HashMap<String, WeakEntity<DatabaseTableView>>,
    project: Entity<Project>,
    active_tab: PanelTab,
    http_requests: Vec<HttpRequest>,
    collections: Vec<PostmanCollection>,
    remote_servers: Vec<RemoteServer>,
    active_server: Option<String>,
    remote_current_path: HashMap<String, String>,
    remote_selected_file: Option<(String, String)>,
    remote_entries: HashMap<String, Vec<RemoteEntry>>,
    remote_dir_entries: HashMap<(String, String), Vec<RemoteEntry>>,
    remote_expanded_paths: HashSet<(String, String)>,
    remote_status: HashMap<(String, String), RemoteStatus>,
    databases: Vec<DatabaseEntry>,
    sync_status: HashMap<String, SyncStatus>,
    database_status: HashMap<String, DatabaseStatus>,
    query_status: HashMap<String, TableStatus>,
    db_expanded_conns: HashSet<String>,
    db_expanded_views: HashSet<String>,
    db_expanded_names: HashSet<String>,
    db_names_status: HashMap<String, DbListStatus>,
    database_views_status: HashMap<String, DatabaseStatus>,
    db_menu_open: bool,
    http_status: HashMap<String, HttpStatus>,
    http_builder: Option<HttpBuilderEditors>,
    http_method: String,
    http_subtab: HttpSubTab,
    http_params: Vec<KeyValueRow>,
    http_headers: Vec<KeyValueRow>,
    http_auth: HttpAuthSettings,
    http_body_kind: HttpBodyKind,
    http_raw_content_type: RawContentType,
    http_raw_body: String,
    http_body_fields: Vec<KeyValueRow>,
    http_follow_redirects: bool,
    http_timeout_secs: u32,
    http_verify_tls: bool,
    http_active_request: Option<HttpRequest>,
    http_settings_file: HttpSettingsFile,
    http_settings_root: Option<PathBuf>,
    http_url_from_rows: bool,
    http_rows_from_url: bool,
    http_rows_dirty: bool,
    http_auth_dirty: bool,
    http_sync_scheduled: bool,
    http_sync_guard: bool,
    http_response_tab: HttpResponseTab,
    http_response_pretty: bool,
    http_builder_response: Option<HttpProbeResponse>,
    http_builder_busy: bool,
    test_list_status: TestListStatus,
    test_files: Vec<String>,
    test_run: Option<TestRunState>,
    test_active_file: Option<String>,
    form: Option<FormState>,
    server_menu_open: bool,
    db_context_menu: Option<DbContextMenu>,
    saved_message: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// Context menu deployed from a Databases tab connection root row.
struct DbContextMenu {
    menu: Entity<ContextMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

impl PhpPanel {
    pub fn init(cx: &mut App) {
        cx.observe_new(|workspace: &mut Workspace, _, _| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<PhpPanel>(window, cx);
            });
            workspace.register_action(|workspace, _: &OpenDeploymentDialog, _, cx| {
                let root = if cx.has_global::<DeployShared>() {
                    cx.global::<DeployShared>().root.clone()
                } else {
                    None
                };
                let root = root.or_else(|| {
                    workspace
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                });
                let Some(root) = root else {
                    return;
                };
                open_deployment_window(root, cx);
            });
            workspace.register_action(|workspace, _: &OpenDataSourcesDialog, _, cx| {
                let root = if cx.has_global::<DeployShared>() {
                    cx.global::<DeployShared>().root.clone()
                } else {
                    None
                };
                let root = root.or_else(|| {
                    workspace
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                });
                let Some(root) = root else {
                    return;
                };
                open_data_sources_window(root, cx);
            });
            workspace.register_action(|workspace, _: &OpenSshConfigsDialog, _, cx| {
                let root = if cx.has_global::<DeployShared>() {
                    cx.global::<DeployShared>().root.clone()
                } else {
                    None
                };
                let root = root.or_else(|| {
                    workspace
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                });
                let Some(root) = root else {
                    return;
                };
                open_ssh_configs_window(root, cx);
            });
            workspace.register_action(|workspace, _: &BrowseRemoteHost, window, cx| {
                if cx.has_global::<DeployShared>() {
                    if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                        panel
                            .update(cx, |panel, cx| panel.select_tab(PanelTab::Remote, cx))
                            .ok();
                    }
                }
                workspace.toggle_panel_focus::<PhpPanel>(window, cx);
            });
            workspace.register_action(|_, _: &SelectNextTab, window, cx| {
                select_tab_cyclically(window, cx, |active| next_tab(active, 1));
            });
            workspace.register_action(|_, _: &SelectPrevTab, window, cx| {
                select_tab_cyclically(window, cx, |active| next_tab(active, -1));
            });
            workspace.register_action(|workspace, action: &AddAsDataSource, _, cx| {
                let Some(abs_path) = action.abs_path.as_deref().map(str::trim) else {
                    return;
                };
                if abs_path.is_empty() {
                    return;
                }
                let outcome = match add_data_source_from_sqlite_file(workspace, abs_path, cx) {
                    Ok(name) => format!("data source added '{name}'"),
                    Err(message) => format!("failed: {message}"),
                };
                if cx.has_global::<DeployShared>() {
                    if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                        panel
                            .update(cx, |panel, cx| {
                                panel.refresh(cx);
                                panel.saved_message = Some(outcome);
                            })
                            .log_err();
                    }
                }
            });
            workspace.register_action(|workspace, action: &DeleteDataSource, _, cx| {
                let name = action.name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let root = if cx.has_global::<DeployShared>() {
                    cx.global::<DeployShared>().root.clone()
                } else {
                    None
                };
                let root = root.or_else(|| {
                    workspace
                        .project()
                        .read(cx)
                        .visible_worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                });
                let Some(root) = root else {
                    return;
                };
                let outcome = match delete_data_source(&root, &name) {
                    Ok(true) => format!("data source removed '{name}'"),
                    Ok(false) => format!("failed: data source '{name}' not found"),
                    Err(message) => format!("failed: {message}"),
                };
                if cx.has_global::<DeployShared>() {
                    if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                        panel
                            .update(cx, |panel, cx| {
                                panel.refresh(cx);
                                panel.refresh_databases(cx);
                                panel.saved_message = Some(outcome);
                            })
                            .log_err();
                    }
                }
            });
            workspace.register_action(|workspace, action: &OpenSqliteDataSource, window, cx| {
                log::info!("phpwb-action: OpenSqliteDataSource fired: {:?}", action.abs_path);
                let Some(abs_path) = action.abs_path.as_deref().map(str::trim) else {
                    return;
                };
                if abs_path.is_empty() {
                    return;
                }
                // Like PhpStorm: the Databases tool window comes to the
                // front right away, so the connection is immediately visible.
                workspace.reveal_panel::<PhpPanel>(window, cx);
                open_sqlite_data_source(workspace, abs_path, window, cx);
            });
        })
        .detach();
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| {
            let project = workspace.project().clone();
            let workspace_handle = cx.entity().downgrade();

            let panel = cx.new(|cx| {
                let focus_handle = cx.focus_handle();
                let subscription = cx.subscribe(&project, |_: &mut Self, _, event, cx| {
                    if matches!(
                        event,
                        project::Event::WorktreeAdded(_)
                            | project::Event::WorktreeRemoved(_)
                            | project::Event::WorktreeUpdatedEntries(..)
                    ) {
                        cx.notify();
                    }
                });

                Self {
                    focus_handle,
                    workspace: workspace_handle,
                    project,
                    active_tab: PanelTab::Remote,
                    http_requests: Vec::new(),
                    collections: Vec::new(),
                    remote_servers: Vec::new(),
                    active_server: None,
                    remote_current_path: HashMap::new(),
                    remote_selected_file: None,
                    remote_entries: HashMap::new(),
                    remote_dir_entries: HashMap::new(),
                    remote_expanded_paths: HashSet::new(),
                    remote_status: HashMap::new(),
                    open_table_views: HashMap::new(),
                    databases: Vec::new(),
                    sync_status: HashMap::new(),
                    database_status: HashMap::new(),
                    query_status: HashMap::new(),
                    db_expanded_conns: HashSet::new(),
                    db_expanded_views: HashSet::new(),
                    db_expanded_names: HashSet::new(),
                    db_names_status: HashMap::new(),
                    database_views_status: HashMap::new(),
                    db_menu_open: false,
                    http_status: HashMap::new(),
                    http_builder: None,
                    http_method: "GET".to_string(),
                    http_subtab: HttpSubTab::Params,
                    http_params: Vec::new(),
                    http_headers: Vec::new(),
                    http_auth: HttpAuthSettings::default(),
                    http_body_kind: HttpBodyKind::None,
                    http_raw_content_type: RawContentType::Json,
                    http_raw_body: String::new(),
                    http_body_fields: Vec::new(),
                    http_follow_redirects: false,
                    http_timeout_secs: default_timeout(),
                    http_verify_tls: true,
                    http_active_request: None,
                    http_settings_file: HttpSettingsFile::default(),
                    http_settings_root: None,
                    http_url_from_rows: false,
                    http_rows_from_url: false,
                    http_rows_dirty: false,
                    http_auth_dirty: false,
                    http_sync_scheduled: false,
                    http_sync_guard: false,
                    http_response_tab: HttpResponseTab::Body,
                    http_response_pretty: true,
                    http_builder_response: None,
                    http_builder_busy: false,
                    test_list_status: TestListStatus::Unloaded,
                    test_files: Vec::new(),
                    test_run: None,
                    test_active_file: None,
                    form: None,
                    server_menu_open: false,
                    db_context_menu: None,
                    saved_message: None,
                    _subscriptions: vec![subscription],
                }
            });
            let indicator = cx.new(|cx| DeployServerIndicator::new(cx));
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_right_item(indicator, window, cx);
            });
            panel
        })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let project = self.project.read(cx);
        let mut http_requests = Vec::new();
        let mut collections = Vec::new();
        let mut remote_servers = Vec::new();
        let mut deploy_config = None;
        let mut databases = Vec::new();

        for worktree in project.visible_worktrees(cx) {
            let worktree = worktree.read(cx);
            let snapshot = worktree.snapshot();
            let worktree_id = worktree.id();
            let root = worktree.abs_path().to_path_buf();
            let entries = snapshot.files(false, 0).collect::<Vec<_>>();
            let env_path = entries
                .iter()
                .filter(|entry| is_postman_environment(entry.path.as_unix_str()))
                .map(|entry| root.join(entry.path.as_unix_str()))
                .min();

            for entry in entries {
                let project_path = ProjectPath {
                    worktree_id,
                    path: entry.path.clone(),
                };
                let absolute_path = root.join(entry.path.as_unix_str());
                if is_postman_collection(entry.path.as_unix_str()) {
                    if let Ok(contents) = std::fs::read_to_string(&absolute_path) {
                        collections.push(parse_postman_collection(
                            &contents,
                            &absolute_path,
                            env_path.clone(),
                            &root,
                        ));
                    }
                }
                if entry.path.extension() == Some("http") {
                    if let Ok(contents) = std::fs::read_to_string(&absolute_path) {
                        http_requests.extend(parse_http_requests(
                            &contents,
                            &project_path,
                            &absolute_path,
                            &root,
                        ));
                    }
                }
            }

            if let Some(config) = read_json(&root.join(".zed/deploy.json")) {
                if let Some(servers) = config.get("servers").and_then(Value::as_array) {
                    for server in servers {
                        if let Some(parsed) = parse_deploy_server(server, &root) {
                            remote_servers.push(parsed);
                        }
                    }
                }
                if deploy_config.is_none() {
                    deploy_config = Some(config);
                }
            }

            if let Some(config) = read_json(&root.join(".zed/databases.json")) {
                if let Some(connections) = config.get("connections").and_then(Value::as_array) {
                    for connection in connections {
                        let Some(name) = string_field(connection, "name") else {
                            continue;
                        };
                        databases.push(DatabaseEntry {
                            name,
                            driver: string_field(connection, "driver"),
                            read_only: connection
                                .get("read_only")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            target: string_field(connection, "path")
                                .or_else(|| string_field(connection, "url"))
                                .or_else(|| {
                                    let host = string_field(connection, "host")?;
                                    Some(match connection.get("port").and_then(Value::as_u64) {
                                        Some(port) => format!("{host}:{port}"),
                                        None => host,
                                    })
                                }),
                            user: string_field(connection, "user"),
                            host: string_field(connection, "host"),
                            root: root.clone(),
                        });
                    }
                }
            }
        }

        http_requests.sort_by(|left, right| {
            left.file
                .path
                .as_unix_str()
                .cmp(right.file.path.as_unix_str())
                .then(left.line.cmp(&right.line))
        });

        self.http_requests = http_requests;
        self.collections = collections;
        self.remote_servers = remote_servers;
        self.databases = databases;

        // global (visible: false) servers stay in remote_servers (so panel saves keep
        // them on disk) but are hidden from the panel's server list and active pick
        let names = self
            .remote_servers
            .iter()
            .filter(|server| server.visible)
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let configured_active = deploy_config
            .as_ref()
            .and_then(|config| config.get("active"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let configured_default = deploy_config
            .as_ref()
            .and_then(|config| config.get("default"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.active_server = resolve_active_server(configured_active, configured_default, &names);

        cx.set_global(DeployShared {
            servers: names.clone(),
            active: self.active_server.clone(),
            panel: Some(cx.weak_entity()),
            root: self
                .remote_servers
                .first()
                .map(|server| server.root.clone()),
        });
    }

    fn sync_server(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        if matches!(self.sync_status.get(&name), Some(SyncStatus::Syncing)) {
            return;
        }
        self.sync_status.insert(name.clone(), SyncStatus::Syncing);
        cx.notify();

        let server_name = name.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-deploy")
                .arg("sync")
                .arg("--server")
                .arg(&name)
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let message = stderr
                        .lines()
                        .chain(stdout.lines())
                        .find(|line| !line.trim().is_empty())
                        .map(|line| line.trim().to_string())
                        .unwrap_or_else(|| "sync failed".to_string());
                    Err(message)
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(()) => SyncStatus::Ok,
                    Err(message) => SyncStatus::Failed(message),
                };
                this.sync_status.insert(server_name, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn current_remote_server(&self) -> Option<&RemoteServer> {
        self.active_server
            .as_ref()
            .and_then(|name| {
                self.remote_servers
                    .iter()
                    .find(|server| &server.name == name && server.visible)
            })
            .or_else(|| self.remote_servers.iter().find(|server| server.visible))
    }

    fn select_tab(&mut self, tab: PanelTab, cx: &mut Context<Self>) {
        if self.active_tab == tab {
            if tab == PanelTab::Remote {
                self.ensure_remote_selected(cx);
            }
            if tab == PanelTab::Tests {
                self.ensure_tests_loaded(cx);
            }
            cx.notify();
            return;
        }
        self.active_tab = tab;
        self.form = None;
        self.saved_message = None;
        self.server_menu_open = false;
        self.db_menu_open = false;
        if tab == PanelTab::Remote {
            self.ensure_remote_selected(cx);
        }
        if tab == PanelTab::Tests {
            self.ensure_tests_loaded(cx);
        }
        cx.notify();
    }

    fn ensure_remote_selected(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.current_remote_server().cloned() else {
            return;
        };
        if self.active_server.as_deref() != Some(server.name.as_str()) {
            self.active_server = Some(server.name.clone());
        }
        let key = (server.name.clone(), ".".to_string());
        self.remote_expanded_paths.insert(key.clone());
        let needs_load = !self.remote_entries.contains_key(&server.name)
            && !matches!(
                self.remote_status.get(&key),
                Some(RemoteStatus::Loading) | Some(RemoteStatus::Failed(_))
            );
        if needs_load {
            self.load_remote_dir(server.name, ".".to_string(), server.root, cx);
        }
    }

    fn switch_remote_server(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        if self.active_server.as_deref() == Some(name.as_str()) {
            self.ensure_remote_selected(cx);
            self.server_menu_open = false;
            cx.notify();
            return;
        }
        self.active_server = Some(name.clone());
        self.remote_entries.clear();
        self.remote_dir_entries.clear();
        self.remote_status.clear();
        self.remote_expanded_paths.clear();
        self.remote_current_path.clear();
        self.remote_selected_file = None;
        self.server_menu_open = false;
        let servers = self.remote_servers.clone();
        let _ = persist_active_server(&root, &servers, &name);
        self.ensure_remote_selected(cx);
        cx.notify();
    }

    fn refresh_remote(&mut self, server: String, root: PathBuf, cx: &mut Context<Self>) {
        self.active_server = Some(server.clone());
        self.remote_entries.remove(&server);
        self.remote_dir_entries
            .retain(|(cached_server, _), _| cached_server != &server);
        self.remote_status
            .retain(|(cached_server, _), _| cached_server != &server);
        self.remote_expanded_paths
            .retain(|(cached_server, _)| cached_server != &server);
        self.remote_expanded_paths
            .insert((server.clone(), ".".to_string()));
        self.remote_current_path
            .insert(server.clone(), ".".to_string());
        self.remote_selected_file = None;
        self.load_remote_dir(server, ".".to_string(), root, cx);
    }

    fn select_remote_file(&mut self, server: String, path: String, cx: &mut Context<Self>) {
        self.active_server = Some(server.clone());
        self.remote_current_path
            .insert(server.clone(), remote_parent_path(&path));
        self.remote_selected_file = Some((server, path));
        cx.notify();
    }

    fn deploy_db_context_menu(
        &mut self,
        name: String,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context_menu = ContextMenu::build(window, cx, |menu, _, _| {
            menu.context(self.focus_handle.clone()).map(|menu| {
                menu.action("Edit Data Source...", Box::new(OpenDataSourcesDialog))
                    .separator()
                    .action(
                        "Delete Data Source",
                        Box::new(DeleteDataSource { name: name.clone() }),
                    )
            })
        });

        window.focus(&context_menu.focus_handle(cx), cx);
        let subscription = cx.subscribe(&context_menu, |this, _, _: &DismissEvent, cx| {
            this.db_context_menu.take();
            cx.notify();
        });
        self.db_context_menu = Some(DbContextMenu {
            menu: context_menu,
            position,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn refresh_databases(&mut self, cx: &mut Context<Self>) {
        self.database_status.clear();
        self.database_views_status.clear();
        self.db_names_status.clear();
        self.db_expanded_conns.clear();
        self.db_expanded_views.clear();
        self.db_expanded_names.clear();
        self.db_menu_open = false;
        cx.notify();
    }

    fn toggle_remote_dir(
        &mut self,
        server: String,
        path: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let key = (server.clone(), path.clone());
        self.active_server = Some(server.clone());
        if self.remote_expanded_paths.contains(&key) {
            self.remote_expanded_paths.remove(&key);
            self.remote_current_path
                .insert(server.clone(), remote_parent_path(&path));
            cx.notify();
            return;
        }

        self.remote_expanded_paths.insert(key.clone());
        self.remote_current_path
            .insert(server.clone(), path.clone());
        let loaded = if path == "." {
            self.remote_entries.contains_key(&server)
        } else {
            self.remote_dir_entries.contains_key(&key)
        };
        if loaded || matches!(self.remote_status.get(&key), Some(RemoteStatus::Loading)) {
            cx.notify();
            return;
        }
        self.load_remote_dir(server, path, root, cx);
    }

    fn load_remote_dir(
        &mut self,
        server: String,
        path: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let key = (server.clone(), path.clone());
        self.remote_status
            .insert(key.clone(), RemoteStatus::Loading);
        cx.notify();

        let task_server = server.clone();
        let task_path = path.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-deploy")
                .arg("ls")
                .arg("--server")
                .arg(&task_server)
                .arg("--path")
                .arg(&task_path)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    parse_remote_entries(&String::from_utf8_lossy(&output.stdout), &task_path)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "ls command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => {
                        if path == "." {
                            this.remote_entries.insert(server.clone(), entries);
                        } else {
                            this.remote_dir_entries
                                .insert((server.clone(), path.clone()), entries);
                        }
                        this.remote_status.remove(&key);
                    }
                    Err(message) => {
                        this.remote_status
                            .insert(key, RemoteStatus::Failed(message));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn download_remote_file(
        &mut self,
        server: String,
        path: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let key = (server.clone(), path.clone());
        if matches!(self.remote_status.get(&key), Some(RemoteStatus::Loading)) {
            return;
        }
        self.active_server = Some(server.clone());
        self.remote_current_path
            .insert(server.clone(), remote_parent_path(&path));
        self.remote_selected_file = Some((server.clone(), path.clone()));
        self.remote_status
            .insert(key.clone(), RemoteStatus::Loading);
        cx.notify();

        let target = PathBuf::from(format!(
            "/tmp/phpworkbench-remote/{}",
            path.replace('/', "__")
        ));
        let task_server = server.clone();
        let task_path = path.clone();
        let task_target = target.clone();
        let task = cx.background_spawn(async move {
            if let Some(parent) = task_target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let output = std::process::Command::new("php-deploy")
                .arg("download")
                .arg("--server")
                .arg(&task_server)
                .arg("--path")
                .arg(&task_path)
                .arg("--to")
                .arg(&task_target)
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => Ok(()),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "download failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.remote_status.insert(key, RemoteStatus::Opened);
                        this.workspace
                            .update_in(cx, |workspace, window, cx| {
                                workspace
                                    .open_abs_path(target, OpenOptions::default(), window, cx)
                                    .detach_and_log_err(cx);
                            })
                            .log_err();
                    }
                    Err(message) => {
                        this.remote_status
                            .insert(key, RemoteStatus::Failed(message));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn push_remote_rows(
        &self,
        server: &RemoteServer,
        entries: &[RemoteEntry],
        depth: usize,
        line: &mut usize,
        out: &mut Vec<gpui::AnyElement>,
        cx: &Context<Self>,
    ) {
        for entry in entries {
            let key = (server.name.clone(), entry.path.clone());
            let status = self.remote_status.get(&key);
            let expanded = self.remote_expanded_paths.contains(&key);
            let selected = self.remote_selected_file.as_ref() == Some(&key);
            let start_icon = if entry.is_directory {
                Icon::new(if expanded {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                })
                .size(IconSize::Small)
                .color(Color::Accent)
            } else {
                match FileIcons::get_icon(Path::new(&entry.name), cx) {
                    Some(icon_path) => Icon::from_path(icon_path).size(IconSize::Small),
                    None => Icon::new(IconName::File).size(IconSize::Small),
                }
            };

            let mut item = ListItem::new(("remote-entry", *line))
                .indent_level(depth + 1)
                .height(px(26.))
                .rounded()
                .selectable(true)
                .focused(selected)
                .dock(DockSide::Left)
                .always_show_disclosure_icon(true)
                .start_slot(start_icon)
                .child(Label::new(entry.name.clone()).truncate())
                .tooltip(Tooltip::text(entry.path.clone()));
            *line += 1;
            if entry.is_directory {
                item = item.toggle(expanded);
            }

            let end_slot = match status {
                Some(RemoteStatus::Loading) if entry.is_directory => {
                    Some(("loading…".to_string(), Color::Muted))
                }
                Some(RemoteStatus::Loading) => Some(("downloading…".to_string(), Color::Muted)),
                Some(RemoteStatus::Opened) => Some(("opened".to_string(), Color::Success)),
                Some(RemoteStatus::Failed(message)) => Some((message.clone(), Color::Error)),
                None if !entry.is_directory => {
                    entry.size.map(|size| (format_size(size), Color::Muted))
                }
                None => None,
            };
            if let Some((text, color)) = end_slot {
                item = item.end_slot(
                    Label::new(text)
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(color)
                        .truncate(),
                );
            }

            let server_name = server.name.clone();
            let path = entry.path.clone();
            let root = server.root.clone();
            let is_directory = entry.is_directory;
            item = item.on_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
                if is_directory {
                    if event.click_count() < 2 {
                        this.toggle_remote_dir(server_name.clone(), path.clone(), root.clone(), cx);
                    }
                } else if event.click_count() >= 2 {
                    this.download_remote_file(server_name.clone(), path.clone(), root.clone(), cx);
                } else {
                    this.select_remote_file(server_name.clone(), path.clone(), cx);
                }
            }));
            out.push(item.into_any_element());

            if entry.is_directory && expanded {
                if let Some(children) = self.remote_dir_entries.get(&key) {
                    self.push_remote_rows(server, children, depth + 1, line, out, cx);
                }
            }
        }
    }

    fn run_http_request(&mut self, request: HttpRequest, cx: &mut Context<Self>) {
        let key = http_request_key(&request);
        if matches!(self.http_status.get(&key), Some(HttpStatus::Running)) {
            return;
        }
        self.http_status.insert(key.clone(), HttpStatus::Running);
        cx.notify();

        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-http")
                .arg("run")
                .arg(&request.absolute_path)
                .arg("--name")
                .arg(&request.name)
                .arg("--json")
                .current_dir(&request.root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    match serde_json::from_str::<Value>(&stdout) {
                        Ok(response) => match response.get("status").and_then(Value::as_u64) {
                            Some(status) => Ok(status as u16),
                            None => Err(first_non_empty_line(stdout.lines())
                                .unwrap_or_else(|| "response had no status".to_string())),
                        },
                        Err(_) => Err(first_non_empty_line(stdout.lines())
                            .unwrap_or_else(|| "invalid JSON response".to_string())),
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "request failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(code) => HttpStatus::Status(code),
                    Err(message) => HttpStatus::Failed(message),
                };
                this.http_status.insert(key, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn run_collection_request(&mut self, request: CollectionRequest, cx: &mut Context<Self>) {
        let key = collection_request_key(&request);
        if matches!(self.http_status.get(&key), Some(HttpStatus::Running)) {
            return;
        }
        self.http_status.insert(key.clone(), HttpStatus::Running);
        cx.notify();

        let task = cx.background_spawn(async move {
            let mut command = std::process::Command::new("php-http");
            command
                .arg("collection")
                .arg("run")
                .arg(&request.collection_path)
                .arg("--request")
                .arg(&request.request_id);
            if let Some(env_path) = &request.env_path {
                command.arg("--env").arg(env_path);
            }
            let output = command.arg("--json").current_dir(&request.root).output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    match serde_json::from_str::<Value>(&stdout) {
                        Ok(response) => match collection_response_status(&response) {
                            Some(status) => Ok(status),
                            None => Err(first_non_empty_line(stdout.lines())
                                .unwrap_or_else(|| "response had no status".to_string())),
                        },
                        Err(_) => Err(first_non_empty_line(stdout.lines())
                            .unwrap_or_else(|| "invalid JSON response".to_string())),
                    }
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "request failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(code) => HttpStatus::Status(code),
                    Err(message) => HttpStatus::Failed(message),
                };
                this.http_status.insert(key, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn ensure_tests_loaded(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.test_list_status,
            TestListStatus::Unloaded | TestListStatus::Failed(_)
        ) {
            self.refresh_tests(cx);
        }
    }

    fn refresh_tests(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.worktree_root(cx) else {
            self.test_list_status = TestListStatus::Failed("no worktree".to_string());
            cx.notify();
            return;
        };
        if matches!(self.test_list_status, TestListStatus::Loading) {
            return;
        }
        self.test_list_status = TestListStatus::Loading;
        cx.notify();

        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-test")
                .arg("list")
                .arg("--dir")
                .arg(&root)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_test_files(&stdout)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "php-test list failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(files) => {
                        this.test_files = files;
                        this.test_list_status = TestListStatus::Loaded;
                    }
                    Err(message) => {
                        this.test_files.clear();
                        this.test_list_status = TestListStatus::Failed(message);
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn run_tests(&mut self, file: Option<String>, cx: &mut Context<Self>) {
        if matches!(self.test_run, Some(TestRunState::Running)) {
            return;
        }
        let Some(root) = self.worktree_root(cx) else {
            self.test_run = Some(TestRunState::Failed("no worktree".to_string()));
            cx.notify();
            return;
        };
        self.test_active_file = file.clone();
        self.test_run = Some(TestRunState::Running);
        cx.notify();

        let task = cx.background_spawn(async move {
            let mut command = std::process::Command::new("php-test");
            command.arg("run").arg("--dir").arg(&root);
            if let Some(file) = &file {
                command.arg("--file").arg(file);
            }
            let output = command.arg("--json").current_dir(&root).output();

            match output {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if let Ok(result) = parse_test_run(&stdout) {
                        Ok(result)
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "php-test run failed".to_string()))
                    }
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.test_run = Some(match result {
                    Ok(result) => TestRunState::Finished(result),
                    Err(message) => TestRunState::Failed(message),
                });
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn worktree_root(&self, cx: &App) -> Option<PathBuf> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
    }

    fn open_sql_form(&mut self, connection: String, window: &mut Window, cx: &mut Context<Self>) {
        self.form = Some(FormState::sql(connection));
        self.saved_message = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn handle_form_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let outcome = self
            .form
            .as_mut()
            .map(|form| form.handle_key(&event.keystroke.key, event.keystroke.key_char.as_deref()));
        match outcome {
            Some(FormKeyOutcome::Cancel) => {
                self.form = None;
                cx.notify();
            }
            Some(FormKeyOutcome::Save) => self.save_form(cx),
            Some(FormKeyOutcome::Continue) => cx.notify(),
            None => {}
        }
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let fields = form.fields.clone();
        let connection = form.connection.clone();
        let Some(root) = self.worktree_root(cx) else {
            if let Some(form) = self.form.as_mut() {
                form.message = Some("no open worktree".to_string());
            }
            cx.notify();
            return;
        };

        let sql = form_field(&fields, 0);
        if sql.is_empty() {
            if let Some(form) = self.form.as_mut() {
                form.message = Some("sql is empty".to_string());
            }
            cx.notify();
            return;
        }
        let Some(connection) = connection else {
            if let Some(form) = self.form.as_mut() {
                form.message = Some("no connection".to_string());
            }
            cx.notify();
            return;
        };
        self.form = None;
        self.run_sql_query(connection, sql, root, cx);
        cx.notify();
    }

    fn clear_active_server(&mut self, cx: &mut Context<Self>) {
        self.server_menu_open = false;
        let servers = self.remote_servers.clone();
        let root = self.worktree_root(cx);
        self.remote_entries.clear();
        self.remote_dir_entries.clear();
        self.remote_status.clear();
        self.remote_expanded_paths.clear();
        self.remote_current_path.clear();
        self.remote_selected_file = None;
        if let Some(root) = root {
            let _ = save_deploy_config(&root, &servers, None);
        }
        self.active_server = None;
        cx.notify();
    }

    pub fn set_active_server_from_status_bar(
        &mut self,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(name) = name.as_ref() {
            let exists = self
                .remote_servers
                .iter()
                .any(|server| &server.name == name);
            if !exists {
                self.refresh(cx);
                // config may have been written before the entry lands; ignore failures
            }
        }
        self.server_menu_open = false;
        match name {
            Some(name) => {
                let root = self
                    .remote_servers
                    .iter()
                    .find(|server| server.name == name)
                    .map(|server| server.root.clone())
                    .or_else(|| self.worktree_root(cx));
                if self.active_server.as_deref() == Some(name.as_str()) {
                    return;
                }
                self.active_server = Some(name.clone());
                self.remote_entries.clear();
                self.remote_dir_entries.clear();
                self.remote_status.clear();
                self.remote_expanded_paths.clear();
                self.remote_current_path.clear();
                self.remote_selected_file = None;
                if let Some(root) = root {
                    let servers = self.remote_servers.clone();
                    let _ = persist_active_server(&root, &servers, &name);
                }
                self.ensure_remote_selected(cx);
            }
            None => {
                self.remote_entries.clear();
                self.remote_dir_entries.clear();
                self.remote_status.clear();
                self.remote_expanded_paths.clear();
                self.remote_current_path.clear();
                self.remote_selected_file = None;
                if let Some(root) = self.worktree_root(cx) {
                    let servers = self.remote_servers.clone();
                    let _ = save_deploy_config(&root, &servers, None);
                }
                self.active_server = None;
            }
        }
        cx.notify();
    }

    pub fn reload_remote(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.worktree_root(cx) else {
            return;
        };
        let servers = load_deploy_servers(&root);
        let names = servers
            .iter()
            .map(|server| server.name.clone())
            .collect::<Vec<_>>();
        let config = read_json(&root.join(".zed/deploy.json"));
        let configured_active = config
            .as_ref()
            .and_then(|config| config.get("active"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let configured_default = config
            .as_ref()
            .and_then(|config| config.get("default"))
            .and_then(Value::as_str)
            .map(str::to_string);
        self.remote_servers = servers;
        self.active_server = resolve_active_server(configured_active, configured_default, &names);
        self.remote_entries.clear();
        self.remote_dir_entries.clear();
        self.remote_status.clear();
        self.remote_expanded_paths.clear();
        self.remote_current_path.clear();
        self.remote_selected_file = None;
        self.server_menu_open = false;
        cx.set_global(DeployShared {
            servers: names.clone(),
            active: self.active_server.clone(),
            panel: Some(cx.weak_entity()),
            root: Some(root),
        });
        self.ensure_remote_selected(cx);
        cx.notify();
    }

    fn run_server_check(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        self.sync_status.insert(name.clone(), SyncStatus::Syncing);
        cx.notify();

        let task_name = name.clone();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-deploy");
            command
                .arg("check")
                .arg("--server")
                .arg(&task_name)
                .current_dir(&root);
            command_result(command.output().await, "check failed")
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(()) => SyncStatus::Ok,
                    Err(message) => SyncStatus::Failed(message),
                };
                this.sync_status.insert(name, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn load_database_schema(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        self.database_status
            .insert(name.clone(), DatabaseStatus::Loading);
        cx.notify();

        let connection_name = name.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("schema")
                .arg(&connection_name)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_database_schema(&stdout)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "schema command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(tables) => DatabaseStatus::Loaded(tables),
                    Err(message) => DatabaseStatus::Failed(message),
                };
                this.database_status.insert(name, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Expands a database connection in the Databases tab and loads its schema,
    /// switching the panel to the Databases tab (PhpStorm-like "open database").
    pub fn expand_database_connection(
        &mut self,
        name: &str,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.select_tab(PanelTab::Databases, cx);
        if self.db_expanded_conns.contains(name) {
            return;
        }
        self.db_expanded_conns.insert(name.to_string());
        if !matches!(
            self.database_status.get(name),
            Some(DatabaseStatus::Loaded(_))
        ) {
            self.load_database_schema(name.to_string(), root, cx);
        }
        cx.notify();
    }

    fn toggle_connection_expand(
        &mut self,
        name: String,
        is_sqlite: bool,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if !self.db_expanded_conns.remove(&name) {
            self.db_expanded_conns.insert(name.clone());
            if !matches!(
                self.database_status.get(&name),
                Some(DatabaseStatus::Loaded(_))
            ) {
                self.load_database_schema(name.clone(), root.clone(), cx);
            }
            if is_sqlite
                && !matches!(
                    self.database_views_status.get(&name),
                    Some(DatabaseStatus::Loaded(_))
                )
            {
                self.load_database_views(name, root, cx);
            }
        }
        cx.notify();
    }

    fn toggle_database_names(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        if !self.db_expanded_names.remove(&name) {
            self.db_expanded_names.insert(name.clone());
            if !matches!(
                self.db_names_status.get(&name),
                Some(DbListStatus::Loading) | Some(DbListStatus::Loaded(_))
            ) {
                self.load_database_names(name, root, cx);
            }
        }
        cx.notify();
    }

    fn toggle_db_menu(&mut self, cx: &mut Context<Self>) {
        self.db_menu_open = !self.db_menu_open;
        cx.notify();
    }

    fn load_database_names(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        self.db_names_status
            .insert(name.clone(), DbListStatus::Loading);
        cx.notify();

        let connection_name = name.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("databases")
                .arg(&connection_name)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_database_names(&stdout)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "databases command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(names) => DbListStatus::Loaded(names),
                    Err(message) => DbListStatus::Failed(message),
                };
                this.db_names_status.insert(name, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn load_database_views(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        self.database_views_status
            .insert(name.clone(), DatabaseStatus::Loading);
        cx.notify();

        let connection_name = name.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("views")
                .arg(&connection_name)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_database_views(&stdout)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "views command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(views) => DatabaseStatus::Loaded(views),
                    Err(message) => DatabaseStatus::Failed(message),
                };
                this.database_views_status.insert(name, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Opens a table (or view) as a data tab in the editor area, like
    /// double-clicking a table node in PhpStorm.
    fn open_table_tab(
        &mut self,
        connection: String,
        connection_label: String,
        table: String,
        root: PathBuf,
        columns: Vec<DatabaseColumn>,
        editable_candidate: bool,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Reuse an already open tab for the same table, like PhpStorm.
        self.open_table_views
            .retain(|_, view| view.upgrade().is_some());
        let key = table_view::table_tab_key(&connection, &table);
        let Some(workspace) = self.workspace.upgrade() else {
            self.saved_message = Some(format!(
                "failed: cannot open `{connection}/{table}` — workspace unavailable"
            ));
            cx.notify();
            return;
        };
        if let Some(view) = self
            .open_table_views
            .get(&key)
            .and_then(WeakEntity::upgrade)
        {
            let activated = workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&view, true, true, window, cx)
            });
            if !activated {
                self.saved_message = Some(format!(
                    "failed: cannot open `{connection}/{table}` — tab not found"
                ));
            } else {
                self.saved_message = None;
            }
            cx.notify();
            return;
        }
        let view = DatabaseTableView::open(
            TableTabTarget {
                connection: connection.clone(),
                connection_label,
                table: table.clone(),
                root,
                columns,
                editable_candidate,
                read_only,
            },
            cx,
        );
        self.open_table_views.insert(key, view.downgrade());
        workspace.update(cx, |workspace, cx| {
            workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
        });
        self.saved_message = None;
        cx.notify();
    }

    fn run_sql_query(
        &mut self,
        connection: String,
        sql: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.query_status
            .insert(connection.clone(), TableStatus::Loading);
        cx.notify();

        let task_connection = connection.clone();
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("query")
                .arg(&task_connection)
                .arg(&sql)
                .arg("--json")
                .current_dir(&root)
                .output();

            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    parse_table_rows(&stdout)
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "query command failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(rows) => TableStatus::Loaded(rows),
                    Err(message) => TableStatus::Failed(message),
                };
                this.query_status.insert(connection, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

const DIALOG_SIZE_MIN_WIDTH: f32 = 480.;
const DIALOG_SIZE_MIN_HEIGHT: f32 = 360.;

/// Path of the user-resizable dialog sizes store
fn dialog_sizes_path() -> PathBuf {
    paths::data_dir()
        .join("phpworkbench")
        .join("dialog_sizes.json")
}

/// Window bounds for a tool dialog: last user-resized size if persisted, else centered default.
fn dialog_bounds(key: &str, default: Size<Pixels>, cx: &App) -> WindowBounds {
    let saved = read_json(&dialog_sizes_path())
        .and_then(|value| value.get(key).cloned())
        .and_then(|value| {
            let width = value.get("width")?.as_f64()? as f32;
            let height = value.get("height")?.as_f64()? as f32;
            Some((width, height))
        })
        .filter(|(width, height)| {
            *width >= DIALOG_SIZE_MIN_WIDTH && *height >= DIALOG_SIZE_MIN_HEIGHT
        });
    match saved {
        Some((width, height)) => {
            WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx))
        }
        None => WindowBounds::centered(default, cx),
    }
}

fn save_dialog_size(key: &str, bounds: Bounds<Pixels>) {
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    if width < DIALOG_SIZE_MIN_WIDTH || height < DIALOG_SIZE_MIN_HEIGHT {
        return;
    }
    let path = dialog_sizes_path();
    let mut sizes = read_json(&path)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    sizes.insert(
        key.to_string(),
        serde_json::json!({ "width": width, "height": height }),
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(contents) = serde_json::to_string_pretty(&sizes) {
        let _ = std::fs::write(&path, contents);
    }
}

fn read_json(path: &Path) -> Option<Value> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn form_field(fields: &[String], index: usize) -> String {
    fields
        .get(index)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn load_config_object(path: &Path) -> std::result::Result<serde_json::Map<String, Value>, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::Map::new());
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Object(config)) => Ok(config),
        Ok(_) => Err(format!("{} is not a JSON object", path.display())),
        Err(error) => Err(format!("invalid JSON in {}: {error}", path.display())),
    }
}

fn upsert_config_entry(
    entries: &mut Vec<Value>,
    name: &str,
    update: impl FnOnce(&mut serde_json::Map<String, Value>),
) {
    let index = entries
        .iter()
        .position(|entry| entry.get("name").and_then(Value::as_str) == Some(name));
    let mut entry = index
        .and_then(|index| entries.get(index))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    entry.insert("name".to_string(), Value::String(name.to_string()));
    update(&mut entry);
    let entry = Value::Object(entry);

    if let Some(index) = index {
        if let Some(existing) = entries.get_mut(index) {
            *existing = entry;
        }
    } else {
        entries.push(entry);
    }
}

fn write_config_object(
    path: &Path,
    config: &serde_json::Map<String, Value>,
) -> std::result::Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|error| format!("cannot serialize {}: {error}", path.display()))?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn resolve_active_server(
    active: Option<String>,
    default: Option<String>,
    names: &[String],
) -> Option<String> {
    let known = |name: &str| names.iter().any(|known| known == name);
    active
        .filter(|name| known(name))
        .or_else(|| default.filter(|name| known(name)))
        .or_else(|| names.first().cloned())
}

fn persist_active_server(
    root: &Path,
    servers: &[RemoteServer],
    name: &str,
) -> std::result::Result<(), String> {
    save_deploy_config(root, servers, Some(name))
}

fn deploy_server_entry(server: &RemoteServer) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("name".to_string(), Value::String(server.name.clone()));
    let protocol = server.protocol.as_deref().unwrap_or_default();
    if !protocol.is_empty() {
        entry.insert("protocol".to_string(), Value::String(protocol.to_string()));
    }
    let host = server.host.as_deref().unwrap_or_default();
    if !host.is_empty() {
        entry.insert("host".to_string(), Value::String(host.to_string()));
    }
    if let Some(port) = server.port.as_deref() {
        if !port.is_empty() {
            match port.parse::<u16>() {
                Ok(number) => entry.insert("port".to_string(), Value::from(number)),
                Err(_) => entry.insert("port".to_string(), Value::String(port.to_string())),
            };
        }
    }
    let user = server.user.as_deref().unwrap_or_default();
    if !user.is_empty() {
        entry.insert("user".to_string(), Value::String(user.to_string()));
    }
    let password = server.password.as_deref().unwrap_or_default();
    if !password.is_empty() && !server.password_session {
        entry.insert("password".to_string(), Value::String(password.to_string()));
    }
    let ssh_config = server.ssh_config.as_deref().unwrap_or_default();
    if !ssh_config.is_empty() {
        entry.insert(
            "ssh_config".to_string(),
            Value::String(ssh_config.to_string()),
        );
    }
    if let Some(remote_root) = server.remote_root.as_deref() {
        if !remote_root.is_empty() {
            entry.insert(
                "remote_root".to_string(),
                Value::String(remote_root.to_string()),
            );
        }
    }
    if let Some(local_root) = server.local_root.as_deref() {
        if !local_root.is_empty() {
            entry.insert(
                "local_root".to_string(),
                Value::String(local_root.to_string()),
            );
        }
    }
    if let Some(web_url) = server.web_url.as_deref() {
        if !web_url.is_empty() {
            entry.insert("web_url".to_string(), Value::String(web_url.to_string()));
        }
    }
    entry.insert(
        "exclude".to_string(),
        Value::Array(
            server
                .excluded
                .iter()
                .map(|pattern| Value::String(pattern.clone()))
                .collect(),
        ),
    );
    if server.upload_on_save {
        entry.insert("upload_on_save".to_string(), Value::Bool(true));
    }
    if server.use_rsync {
        entry.insert("use_rsync".to_string(), Value::Bool(true));
    }
    if server.use_sudo {
        entry.insert("use_sudo".to_string(), Value::Bool(true));
    }
    entry.insert("visible".to_string(), Value::Bool(server.visible));
    if deploy_type_has_advanced(protocol) {
        entry.insert("passive_mode".to_string(), Value::Bool(server.passive_mode));
        entry.insert("show_hidden".to_string(), Value::Bool(server.show_hidden));
        entry.insert("list_compat".to_string(), Value::Bool(server.list_compat));
        entry.insert("use_mlsd".to_string(), Value::Bool(server.use_mlsd));
        entry.insert(
            "max_connections".to_string(),
            Value::from(server.max_connections),
        );
        entry.insert(
            "keep_alive_enabled".to_string(),
            Value::Bool(server.keep_alive_enabled),
        );
        entry.insert(
            "keep_alive_seconds".to_string(),
            Value::from(server.keep_alive_seconds),
        );
        entry.insert(
            "encoding".to_string(),
            Value::String(server.encoding.clone()),
        );
    }
    Value::Object(entry)
}

fn save_deploy_config(
    root: &Path,
    servers: &[RemoteServer],
    active: Option<&str>,
) -> std::result::Result<(), String> {
    let path = root.join(".zed/deploy.json");
    let mut config = load_config_object(&path)?;
    let entries = servers.iter().map(deploy_server_entry).collect::<Vec<_>>();
    config.insert("servers".to_string(), Value::Array(entries));
    match active {
        Some(name) => {
            config.insert("active".to_string(), Value::String(name.to_string()));
        }
        None => {
            config.remove("active");
        }
    }
    write_config_object(&path, &config)
}

fn parse_deploy_server(server: &Value, root: &Path) -> Option<RemoteServer> {
    let name = string_field(server, "name")?;
    let protocol = string_field(server, "protocol");
    Some(RemoteServer {
        name,
        password: string_field(server, "password"),
        password_session: false,
        ssh_config: string_field(server, "ssh_config"),
        remote_root: string_field(server, "remote_root"),
        local_root: string_field(server, "local_root"),
        web_url: string_field(server, "web_url"),
        excluded: excluded_paths(server),
        upload_on_save: server
            .get("upload_on_save")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        use_rsync: server
            .get("use_rsync")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        use_sudo: server
            .get("use_sudo")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        visible: server
            .get("visible")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        passive_mode: server
            .get("passive_mode")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        show_hidden: server
            .get("show_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        list_compat: server
            .get("list_compat")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        use_mlsd: server
            .get("use_mlsd")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        max_connections: server
            .get("max_connections")
            .and_then(Value::as_u64)
            .map(|number| number.min(u32::MAX as u64) as u32)
            .unwrap_or(ADVANCED_DEFAULT_MAX_CONNECTIONS),
        keep_alive_enabled: server
            .get("keep_alive_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        keep_alive_seconds: server
            .get("keep_alive_seconds")
            .and_then(Value::as_u64)
            .map(|number| number.min(u32::MAX as u64) as u32)
            .unwrap_or(ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS),
        encoding: string_field(server, "encoding")
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| ADVANCED_DEFAULT_ENCODING.to_string()),
        protocol,
        host: string_field(server, "host"),
        port: port_field(server),
        user: string_field(server, "user"),
        root: root.to_path_buf(),
    })
}

fn port_field(server: &Value) -> Option<String> {
    match server.get("port") {
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    }
}

fn excluded_paths(server: &Value) -> Vec<String> {
    for key in ["exclude", "excluded"] {
        if let Some(Value::Array(patterns)) = server.get(key) {
            return patterns
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

fn warning_texts(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                other => other
                    .get("message")
                    .or_else(|| other.get("text"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .filter(|text| !text.is_empty())
            .collect(),
        Some(Value::Object(map)) => warning_texts(map.get("warnings")),
        _ => Vec::new(),
    }
}

fn parse_validate_output(value: &Value) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    if let Some(servers) = value.get("servers").and_then(Value::as_array) {
        for server in servers {
            let Some(name) = string_field(server, "name") else {
                continue;
            };
            let warnings = warning_texts(server.get("warnings"));
            if !warnings.is_empty() {
                result.push((name, warnings));
            }
        }
        if !result.is_empty() {
            return result;
        }
    }
    for key in ["warnings", "connections"] {
        if let Some(map) = value.get(key).and_then(Value::as_object) {
            for (name, list) in map {
                let warnings = warning_texts(Some(list));
                if !warnings.is_empty() {
                    result.push((name.clone(), warnings));
                }
            }
            if !result.is_empty() {
                return result;
            }
        }
    }
    let flat = warning_texts(value.get("warnings"));
    if !flat.is_empty() {
        result.push((String::new(), flat));
    }
    result
}

fn parse_autodetect_root(value: &Value) -> Option<String> {
    for key in [
        "root",
        "path",
        "remote_root",
        "directory",
        "found",
        "listing",
    ] {
        if let Some(text) = string_field(value, key) {
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    value.get("result").and_then(parse_autodetect_root)
}

fn deploy_temp_config_content(servers: &[Value], active: &str) -> Value {
    serde_json::json!({
        "servers": servers,
        "active": active,
    })
}

fn local_base_display(local_base: &Option<PathBuf>, root: &Path) -> String {
    local_base
        .as_ref()
        .map(|path| {
            path.strip_prefix(root)
                .map(|relative| relative.to_string_lossy().to_string())
                .unwrap_or_else(|_| path.to_string_lossy().to_string())
        })
        .unwrap_or_default()
}

fn blank_server(root: PathBuf) -> RemoteServer {
    let local_root = if root.as_os_str().is_empty() {
        None
    } else {
        Some(root.to_string_lossy().to_string())
    };
    RemoteServer {
        name: String::new(),
        protocol: Some("local".to_string()),
        host: None,
        port: None,
        user: None,
        password: None,
        password_session: false,
        ssh_config: None,
        remote_root: Some("/".to_string()),
        local_root,
        web_url: None,
        excluded: Vec::new(),
        upload_on_save: false,
        use_rsync: false,
        use_sudo: false,
        visible: true,
        passive_mode: true,
        show_hidden: false,
        list_compat: false,
        use_mlsd: false,
        max_connections: ADVANCED_DEFAULT_MAX_CONNECTIONS,
        keep_alive_enabled: true,
        keep_alive_seconds: ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS,
        encoding: ADVANCED_DEFAULT_ENCODING.to_string(),
        root,
    }
}

fn connection_fields(server: Option<&RemoteServer>) -> Vec<String> {
    match server {
        Some(server) => vec![
            server.name.clone(),
            server.protocol.clone().unwrap_or_default(),
            server.host.clone().unwrap_or_default(),
            server.port.clone().unwrap_or_default(),
            server.user.clone().unwrap_or_default(),
            server.remote_root.clone().unwrap_or_default(),
        ],
        None => vec![String::new(); 6],
    }
}

fn mapping_fields(server: Option<&RemoteServer>) -> Vec<String> {
    match server {
        Some(server) => vec![
            server.local_root.clone().unwrap_or_default(),
            server.remote_root.clone().unwrap_or_default(),
            server.web_url.clone().unwrap_or_default(),
        ],
        None => vec![String::new(); 3],
    }
}

fn excluded_text(excluded: Option<&[String]>) -> String {
    excluded.unwrap_or(&[]).join(", ")
}

fn parse_excluded(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_string)
        .collect()
}

fn apply_connection_fields(server: &mut RemoteServer, fields: &[String]) {
    let name = form_field(fields, 0);
    if !name.is_empty() {
        server.name = name;
    }
    let protocol = form_field(fields, 1).to_ascii_lowercase();
    if protocol.is_empty() {
        server.protocol = None;
    } else {
        server.protocol = Some(protocol);
    }
    let host = form_field(fields, 2);
    server.host = if host.is_empty() { None } else { Some(host) };
    let port = form_field(fields, 3);
    server.port = if port.is_empty() { None } else { Some(port) };
    let user = form_field(fields, 4);
    server.user = if user.is_empty() { None } else { Some(user) };
    let remote_root = form_field(fields, 5);
    server.remote_root = if remote_root.is_empty() {
        None
    } else {
        Some(remote_root)
    };
}

fn apply_mapping_fields(server: &mut RemoteServer, fields: &[String]) {
    let local_root = form_field(fields, 0);
    server.local_root = if local_root.is_empty() {
        None
    } else {
        Some(local_root)
    };
    let remote_root = form_field(fields, 1);
    server.remote_root = if remote_root.is_empty() {
        None
    } else {
        Some(remote_root)
    };
    let web_url = form_field(fields, 2);
    server.web_url = if web_url.is_empty() {
        None
    } else {
        Some(web_url)
    };
}

fn apply_excluded_field(server: &mut RemoteServer, fields: &[String]) {
    server.excluded = parse_excluded(fields.first().map(String::as_str).unwrap_or_default());
}

fn validate_staged_servers(servers: &[RemoteServer]) -> Option<String> {
    for server in servers {
        if server.name.trim().is_empty() {
            return Some("server name is required".to_string());
        }
        if let Some(protocol) = server.protocol.as_deref() {
            if !protocol.is_empty() && deploy_type_canonical(protocol).is_none() {
                return Some(format!(
                    "\"{}\" protocol must be one of: {}",
                    server.name,
                    DEPLOY_TYPES.join(", ")
                ));
            }
        }
        if let Some(port) = server.port.as_deref() {
            if !port.is_empty() && port.parse::<u16>().is_err() {
                return Some(format!("\"{}\" port must be a number", server.name));
            }
        }
    }
    for (index, server) in servers.iter().enumerate() {
        if servers[..index]
            .iter()
            .any(|other| other.name == server.name)
        {
            return Some(format!("duplicate server name \"{}\"", server.name));
        }
    }
    None
}

fn save_server_config(root: &Path, fields: &[String]) -> std::result::Result<String, String> {
    let name = form_field(fields, 0);
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let protocol = form_field(fields, 1).to_ascii_lowercase();
    if !SERVER_PROTOCOLS.contains(&protocol.as_str()) {
        return Err("protocol must be local or sftp".to_string());
    }
    let port_text = form_field(fields, 3);
    let port = if port_text.is_empty() {
        None
    } else {
        Some(
            port_text
                .parse::<u16>()
                .map_err(|_| "port must be a number".to_string())?,
        )
    };

    let host = form_field(fields, 2);
    let user = form_field(fields, 4);
    let remote_root = form_field(fields, 5);
    let local_root = form_field(fields, 6);

    let path = root.join(".zed/deploy.json");
    let mut config = load_config_object(&path)?;
    let mut servers = match config.remove("servers") {
        Some(Value::Array(servers)) => servers,
        Some(_) => return Err("servers in deploy.json is not an array".to_string()),
        None => Vec::new(),
    };

    upsert_config_entry(&mut servers, &name, |server| {
        for key in [
            "protocol",
            "host",
            "port",
            "user",
            "remote_root",
            "local_root",
        ] {
            server.remove(key);
        }
        server.insert("protocol".to_string(), Value::String(protocol.clone()));
        if !host.is_empty() {
            server.insert("host".to_string(), Value::String(host.clone()));
        }
        if let Some(port) = port {
            server.insert("port".to_string(), Value::Number(port.into()));
        }
        if !user.is_empty() {
            server.insert("user".to_string(), Value::String(user.clone()));
        }
        server.insert(
            "remote_root".to_string(),
            Value::String(remote_root.clone()),
        );
        server.insert("local_root".to_string(), Value::String(local_root.clone()));
    });

    config.insert("servers".to_string(), Value::Array(servers));
    write_config_object(&path, &config)?;
    Ok(name)
}

fn command_result(
    output: std::io::Result<std::process::Output>,
    fallback: &str,
) -> std::result::Result<(), String> {
    match output {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                .unwrap_or_else(|| fallback.to_string()))
        }
        Err(error) => Err(error.to_string()),
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
}

fn display_field(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("-")
}

fn driver_badge_short(driver: &str) -> &'static str {
    match driver {
        "postgres" => "pg",
        "mysql" => "my",
        "mariadb" => "mar",
        "sqlite" => "lite",
        "sqlserver" => "msr",
        "oracle" => "orc",
        "mongodb" => "mdb",
        "redis" => "sh",
        _ => "?",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DsGeneralField {
    Host,
    Port,
    User,
    Password,
    Database,
}

fn data_source_general_fields(driver: &str) -> &'static [DsGeneralField] {
    match driver {
        "sqlite" => &[],
        "redis" => &[
            DsGeneralField::Host,
            DsGeneralField::Port,
            DsGeneralField::Password,
        ],
        "mongodb" => &[
            DsGeneralField::Host,
            DsGeneralField::Port,
            DsGeneralField::User,
            DsGeneralField::Password,
            DsGeneralField::Database,
        ],
        _ => &[
            DsGeneralField::Host,
            DsGeneralField::Port,
            DsGeneralField::User,
            DsGeneralField::Password,
            DsGeneralField::Database,
        ],
    }
}

fn driver_default_port(driver: &str) -> Option<&'static str> {
    match driver {
        "postgres" => Some("5432"),
        "mysql" | "mariadb" => Some("3306"),
        "sqlserver" => Some("1433"),
        "oracle" => Some("1521"),
        "mongodb" => Some("27017"),
        "redis" => Some("6379"),
        _ => None,
    }
}

fn driver_badge_color(driver: &str) -> Color {
    match driver {
        "postgres" => Color::Info,
        "mysql" => Color::Warning,
        "mariadb" => Color::Warning,
        "sqlite" => Color::Created,
        "sqlserver" => Color::Accent,
        "oracle" => Color::Error,
        "mongodb" => Color::Success,
        "redis" => Color::Error,
        _ => Color::Muted,
    }
}

fn driver_display_name(driver: &str) -> String {
    match driver {
        "postgres" => "PostgreSQL".to_string(),
        "mysql" => "MySQL".to_string(),
        "mariadb" => "MariaDB".to_string(),
        "sqlite" => "SQLite".to_string(),
        "sqlserver" => "SQL Server".to_string(),
        "oracle" => "Oracle".to_string(),
        "mongodb" => "MongoDB".to_string(),
        "redis" => "Redis".to_string(),
        other => other.to_string(),
    }
}

fn parse_remote_entries(
    stdout: &str,
    base_path: &str,
) -> std::result::Result<Vec<RemoteEntry>, String> {
    let value: Value =
        serde_json::from_str(stdout.trim()).map_err(|error| format!("invalid ls JSON: {error}"))?;
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "ls output missing entries".to_string())?;

    Ok(entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?;
            if name.is_empty() {
                return None;
            }
            let path = if base_path == "." {
                name.to_string()
            } else {
                format!("{base_path}/{name}")
            };
            Some(RemoteEntry {
                name: name.to_string(),
                path,
                is_directory: entry.get("type").and_then(Value::as_str) == Some("dir"),
                size: entry.get("size").and_then(Value::as_u64),
            })
        })
        .collect())
}

fn parse_table_list(tables: &[Value]) -> Vec<DatabaseTable> {
    let mut parsed = Vec::new();
    for table in tables {
        let Some(name) = string_field(table, "name") else {
            continue;
        };
        let columns = table
            .get("columns")
            .and_then(Value::as_array)
            .map(|columns| {
                columns
                    .iter()
                    .filter_map(|column| {
                        let name = string_field(column, "name")?;
                        Some(DatabaseColumn {
                            name,
                            column_type: string_field(column, "type").unwrap_or_default(),
                            primary_key: column
                                .get("primary_key")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        parsed.push(DatabaseTable { name, columns });
    }
    parsed
}

fn parse_database_schema(stdout: &str) -> Result<Vec<DatabaseTable>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid schema JSON".to_string())
    })?;
    let tables = value
        .get("tables")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema JSON had no tables".to_string())?;
    Ok(parse_table_list(tables))
}

fn parse_database_views(stdout: &str) -> Result<Vec<DatabaseTable>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid views JSON".to_string())
    })?;
    let views = value
        .get("views")
        .and_then(Value::as_array)
        .ok_or_else(|| "views JSON had no views".to_string())?;
    Ok(parse_table_list(views))
}

fn parse_database_names(stdout: &str) -> Result<Vec<String>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid databases JSON".to_string())
    })?;
    let databases = value
        .get("databases")
        .and_then(Value::as_array)
        .ok_or_else(|| "databases JSON had no databases".to_string())?;
    Ok(databases
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}

pub(crate) fn parse_table_rows(
    stdout: &str,
) -> Result<Vec<serde_json::Map<String, Value>>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid query JSON".to_string())
    })?;
    let rows = value
        .as_array()
        .ok_or_else(|| "query JSON was not an array".to_string())?;
    Ok(rows.iter().filter_map(Value::as_object).cloned().collect())
}

pub(crate) fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    }
}

fn format_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let size = size as f64;
    if size >= GB {
        format!("{:.1} GB", size / GB)
    } else if size >= MB {
        format!("{:.1} MB", size / MB)
    } else if size >= KB {
        format!("{:.1} KB", size / KB)
    } else {
        format!("{} B", size as u64)
    }
}

fn remote_parent_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
        _ => ".".to_string(),
    }
}

impl Focusable for PhpPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for PhpPanel {}

impl Panel for PhpPanel {
    fn persistent_name() -> &'static str {
        "PHP"
    }

    fn panel_key() -> &'static str {
        "PhpPanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        DockPosition::Right
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, _: DockPosition, _: &mut Window, _: &mut Context<Self>) {}

    fn default_size(&self, _: &Window, _: &App) -> gpui::Pixels {
        gpui::px(480.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::FileCode)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("PHP")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        100
    }

    fn starts_open(&self, _window: &Window, _cx: &App) -> bool {
        true
    }
}

impl Render for PhpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);

        let selected_remote = self.current_remote_server().cloned();
        let active_name = self.active_server.clone();

        let picker_servers = self
            .remote_servers
            .iter()
            .filter(|server| server.visible)
            .cloned()
            .collect::<Vec<_>>();
        let picker_active = active_name.clone();
        let picker = selected_remote.as_ref().map(|server| {
            (
                server.protocol.clone().unwrap_or_default(),
                server.name.clone(),
            )
        });
        let remote_server_picker = h_flex()
            .id("remote-server-picker")
            .flex_1()
            .h(px(26.))
            .min_w_0()
            .items_center()
            .gap_1()
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .border_1()
            .border_color(if self.server_menu_open {
                cx.theme().colors().element_selected
            } else {
                cx.theme().colors().border
            })
            .when(self.server_menu_open, |this| {
                this.bg(cx.theme().colors().element_selected)
            })
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(remote_protocol_badge(
                        &picker
                            .as_ref()
                            .map(|(protocol, _)| {
                                if protocol.is_empty() {
                                    "server".to_string()
                                } else {
                                    protocol.clone()
                                }
                            })
                            .unwrap_or_else(|| "-".to_string()),
                        cx,
                    ))
                    .when_some(
                        picker.as_ref().map(|(_, name)| name.clone()),
                        |this, name| {
                            this.child(
                                Label::new(name)
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Default)
                                    .truncate(),
                            )
                        },
                    )
                    .when(picker.is_none(), |this| {
                        this.child(
                            Label::new("no server")
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }),
            )
            .child(
                div().flex_none().child(
                    Icon::new(IconName::ChevronDown)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                ),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.server_menu_open = !this.server_menu_open;
                this.ensure_remote_selected(cx);
                cx.notify();
            }));

        let refresh_target = selected_remote
            .as_ref()
            .map(|server| (server.name.clone(), server.root.clone()));
        let sync_target = refresh_target.clone();

        let remote_toolbar = h_flex()
            .id("remote-toolbar")
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(remote_server_picker)
            .child(
                IconButton::new("remote-configure", IconName::Ellipsis)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Deployment configuration"))
                    .aria_label("Deployment configuration")
                    .on_click(cx.listener(|_: &mut Self, _: &ClickEvent, window, cx| {
                        window.dispatch_action(Box::new(OpenDeploymentDialog), cx);
                    })),
            )
            .child(
                IconButton::new("remote-refresh", IconName::RotateCw)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Refresh"))
                    .aria_label("Refresh remote listing")
                    .disabled(refresh_target.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        if let Some((name, root)) = refresh_target.clone() {
                            this.refresh_remote(name, root, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("remote-sync", IconName::Share)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Sync"))
                    .aria_label("Sync files to server")
                    .disabled(sync_target.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        if let Some((name, root)) = sync_target.clone() {
                            this.sync_server(name, root, cx);
                        }
                    })),
            );

        let remote_server_menu = if self.server_menu_open {
            let mut menu_rows = picker_servers
                .iter()
                .enumerate()
                .map(|(index, server)| {
                    let is_active = picker_active.as_deref() == Some(server.name.as_str());
                    let server_name = server.name.clone();
                    let root = server.root.clone();
                    let badge = server
                        .protocol
                        .clone()
                        .unwrap_or_else(|| "server".to_string());
                    h_flex()
                        .id(("remote-server-menu", index))
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .cursor_pointer()
                        .when(is_active, |this| {
                            this.bg(cx.theme().colors().element_selected)
                        })
                        .hover(|style| style.bg(cx.theme().colors().element_hover))
                        .child(remote_protocol_badge(&badge, cx))
                        .child(
                            Label::new(server.name.clone())
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(if is_active {
                                    Color::Accent
                                } else {
                                    Color::Default
                                })
                                .truncate(),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.switch_remote_server(server_name.clone(), root.clone(), cx);
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>();
            menu_rows.push(
                h_flex()
                    .id("remote-server-menu-none")
                    .w_full()
                    .flex_none()
                    .items_center()
                    .px_2()
                    .py_0p5()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        Label::new("<no default server>")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.clear_active_server(cx);
                    }))
                    .into_any_element(),
            );
            Some(
                v_flex()
                    .id("remote-server-menu")
                    .w_full()
                    .flex_none()
                    .gap_0p5()
                    .px_1()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().elevated_surface_background)
                    .shadow_sm()
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .children(menu_rows),
            )
        } else {
            None
        };

        let (remote_status_line, remote_error_line) = if let Some(server) = &selected_remote {
            let host = match (&server.user, &server.host) {
                (Some(user), Some(host)) => format!("{user}@{host}"),
                (None, Some(host)) => host.clone(),
                (Some(user), None) => user.clone(),
                (None, None) => "-".to_string(),
            };
            let mut line = format!(
                "{} · {} · {} · {} · {}",
                server.name,
                display_field(&server.protocol),
                host,
                display_field(&server.remote_root),
                if server.upload_on_save {
                    "auto-upload"
                } else {
                    "manual"
                }
            );
            let status_color = match self.sync_status.get(&server.name) {
                Some(SyncStatus::Syncing) => {
                    line.push_str(" · syncing…");
                    Color::Muted
                }
                Some(SyncStatus::Ok) => {
                    line.push_str(" · sync ok");
                    Color::Success
                }
                Some(SyncStatus::Failed(message)) => {
                    line.push_str(&format!(" · {}", message));
                    Color::Error
                }
                None => Color::Muted,
            };
            let status_label = Label::new(line)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(status_color)
                .truncate()
                .into_any_element();
            let error_label = match self
                .remote_status
                .get(&(server.name.clone(), ".".to_string()))
            {
                Some(RemoteStatus::Failed(message)) => Some(
                    Label::new(message.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Error)
                        .truncate()
                        .into_any_element(),
                ),
                _ => None,
            };
            (Some(status_label), error_label)
        } else {
            (None, None)
        };

        let remote_status = v_flex()
            .w_full()
            .flex_none()
            .px_2()
            .py_1()
            .when_some(remote_status_line, |this, line| this.child(line))
            .when_some(remote_error_line, |this, line| this.child(line));

        let current_path = selected_remote
            .as_ref()
            .and_then(|server| self.remote_current_path.get(&server.name))
            .cloned()
            .unwrap_or_else(|| ".".to_string());
        let mut crumb_parts = Vec::new();
        if let Some(server) = &selected_remote {
            crumb_parts.push(server.name.clone());
            if current_path != "." {
                crumb_parts.extend(
                    current_path
                        .split('/')
                        .filter(|part| !part.is_empty())
                        .map(str::to_string),
                );
            }
        }
        let crumb_text = if crumb_parts.is_empty() {
            "Remote".to_string()
        } else {
            crumb_parts.join(" / ")
        };
        let mut crumb_children = Vec::new();
        for (index, part) in crumb_parts.iter().enumerate() {
            if index > 0 {
                crumb_children.push(
                    Label::new("/")
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted)
                        .into_any_element(),
                );
            }
            crumb_children.push(
                Label::new(part.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(if index + 1 == crumb_parts.len() {
                        Color::Default
                    } else {
                        Color::Muted
                    })
                    .truncate()
                    .into_any_element(),
            );
        }
        let remote_breadcrumb = if crumb_parts.len() > 1 {
            Some(
                h_flex()
                    .id("remote-breadcrumb-2")
                    .w_full()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .overflow_hidden()
                    .tooltip(Tooltip::text(crumb_text))
                    .children(crumb_children)
                    .into_any_element(),
            )
        } else {
            None
        };

        let mut remote_rows = Vec::new();
        let mut remote_line = 0usize;
        if let Some(server) = &selected_remote {
            let host_display = match (&server.user, &server.host) {
                (Some(user), Some(host)) => format!("{user}@{host}"),
                (None, Some(host)) => host.clone(),
                (Some(user), None) => user.clone(),
                (None, None) => String::new(),
            };
            let host_part = if host_display.is_empty() {
                String::new()
            } else {
                format!(
                    " ({}{})",
                    host_display,
                    server
                        .remote_root
                        .as_deref()
                        .map(|root| format!("/{}", root))
                        .unwrap_or_default()
                )
            };
            remote_rows.push(
                h_flex()
                    .id("remote-root-row")
                    .px_3()
                    .py_1()
                    .gap_1()
                    .child(
                        div().flex_none().child(
                            Icon::new(IconName::ChevronDown)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        ),
                    )
                    .child(remote_protocol_badge(
                        &remote_root_badge(&selected_remote),
                        cx,
                    ))
                    .child(
                        Label::new(server.name.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Default)
                            .flex_none(),
                    )
                    .child(
                        Label::new(host_part.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .into_any_element(),
            );
            if let Some(entries) = self.remote_entries.get(&server.name) {
                self.push_remote_rows(server, entries, 0, &mut remote_line, &mut remote_rows, cx);
                if remote_rows.is_empty() {
                    remote_rows.push(empty_row("empty directory"));
                }
            } else {
                let key = (server.name.clone(), ".".to_string());
                let (message, color) = match self.remote_status.get(&key) {
                    Some(RemoteStatus::Failed(message)) => (message.clone(), Color::Error),
                    Some(RemoteStatus::Loading) => ("loading…".to_string(), Color::Muted),
                    _ => ("not loaded · press Refresh".to_string(), Color::Muted),
                };
                remote_rows.push(
                    h_flex()
                        .px_3()
                        .py_1()
                        .child(
                            Label::new(message)
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(color)
                                .truncate(),
                        )
                        .into_any_element(),
                );
            }
        } else {
            remote_rows.push(empty_state(IconName::Info, "No servers configured", "Add a server or pick one from the list", cx));
        }

        let mut database_rows = Vec::new();
        for (index, database) in self.databases.iter().enumerate() {
            let expanded = self.db_expanded_conns.contains(&database.name);

            let mut schema_rows = Vec::new();
            if expanded {
                match self.database_status.get(&database.name) {
                    Some(DatabaseStatus::Loaded(tables)) => {
                        if tables.is_empty() {
                            schema_rows.push(
                                ListItem::new(format!("database-empty-{index}"))
                                    .indent_level(1)
                                    .child(
                                        Label::new("no tables")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    )
                                    .into_any_element(),
                            );
                        }
                        for (table_index, table) in tables.iter().enumerate() {
                            let table_read_only = database.read_only;
                            let table_label = database_connection_label(database);
                            let connection_name = database.name.clone();
                            let table_name = table.name.clone();
                            let table_root = database.root.clone();
                            let table_columns = table.columns.clone();
                            let table_editable = database.driver.as_deref() == Some("sqlite");

                            schema_rows.push(
                                ListItem::new(format!("database-table-{index}-{table_index}"))
                                    .indent_level(1)
                                    .height(px(24.))
                                    .rounded()
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::Table)
                                                    .size(IconSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(
                                                Label::new(table.name.clone())
                                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                    .truncate(),
                                            ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.open_table_tab(
                                                connection_name.clone(),
                                                table_label.clone(),
                                                table_name.clone(),
                                                table_root.clone(),
                                                table_columns.clone(),
                                                table_editable,
                                                table_read_only,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .into_any_element(),
                            );

                            for (column_index, column) in table.columns.iter().enumerate() {
                                let type_text = column.column_type.clone();
                                let column_label = h_flex()
                                    .gap_1()
                                    .child(
                                        Icon::new(if column.primary_key {
                                            IconName::BoltFilled
                                        } else {
                                            IconName::Circle
                                        })
                                        .size(IconSize::Small)
                                        .color(
                                            if column.primary_key {
                                                Color::Warning
                                            } else {
                                                Color::Muted
                                            },
                                        ),
                                    )
                                    .child(
                                        Label::new(column.name.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Default)
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(type_text)
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    );
                                schema_rows.push(
                                    ListItem::new(format!(
                                        "database-column-{index}-{table_index}-{column_index}"
                                    ))
                                    .indent_level(2)
                                    .child(column_label)
                                    .into_any_element(),
                                );
                            }
                        }
                    }
                    Some(DatabaseStatus::Loading) => {
                        schema_rows.push(
                            ListItem::new(format!("database-loading-{index}"))
                                .indent_level(1)
                                .child(
                                    Label::new("loading schema…")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted),
                                )
                                .into_any_element(),
                        );
                    }
                    Some(DatabaseStatus::Failed(message)) => {
                        schema_rows.push(
                            ListItem::new(format!("database-schema-error-{index}"))
                                .indent_level(1)
                                .child(
                                    Label::new(message.clone())
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Error)
                                        .truncate(),
                                )
                                .into_any_element(),
                        );
                    }
                    None => {}
                }

                if database.driver.as_deref() == Some("sqlite")
                    && let Some(DatabaseStatus::Loaded(views)) =
                        self.database_views_status.get(&database.name)
                    && !views.is_empty()
                {
                    let views_open = self.db_expanded_views.contains(&database.name);
                    let views_connection = database.name.clone();
                    schema_rows.push(
                        ListItem::new(format!("database-views-{index}"))
                            .indent_level(1)
                            .toggle(views_open)
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Label::new("Views")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Default),
                                    )
                                    .child(
                                        Label::new(format!("({})", views.len()))
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                if this.db_expanded_views.remove(&views_connection) {
                                    cx.notify();
                                } else {
                                    this.db_expanded_views.insert(views_connection.clone());
                                    cx.notify();
                                }
                            }))
                            .into_any_element(),
                    );
                    if views_open {
                        for (view_index, view) in views.iter().enumerate() {
                            let view_read_only = database.read_only;
                            let view_label = database_connection_label(database);
                            let view_connection = database.name.clone();
                            let view_name = view.name.clone();
                            let view_root = database.root.clone();
                            let view_columns = view.columns.clone();
                            schema_rows.push(
                                ListItem::new(format!("database-view-{index}-{view_index}"))
                                    .indent_level(2)
                                    .height(px(24.))
                                    .rounded()
                                    .child(
                                        h_flex()
                                            .gap_1()
                                            .child(
                                                Icon::new(IconName::Table)
                                                    .size(IconSize::Small)
                                                    .color(Color::Muted),
                                            )
                                            .child(
                                                Label::new(view.name.clone())
                                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                    .truncate(),
                                            ),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, window, cx| {
                                            this.open_table_tab(
                                                view_connection.clone(),
                                                view_label.clone(),
                                                view_name.clone(),
                                                view_root.clone(),
                                                view_columns.clone(),
                                                false,
                                                view_read_only,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                    .into_any_element(),
                            );
                        }
                    }
                }

                if database.driver.as_deref() != Some("sqlite") {
                    let names_open = self.db_expanded_names.contains(&database.name);
                    let names_count = match self.db_names_status.get(&database.name) {
                        Some(DbListStatus::Loaded(names)) => format!(" ({})", names.len()),
                        _ => String::new(),
                    };
                    let names_connection = database.name.clone();
                    let names_root = database.root.clone();
                    schema_rows.push(
                        ListItem::new(format!("database-names-{index}"))
                            .indent_level(1)
                            .toggle(names_open)
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Label::new("Databases")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Default),
                                    )
                                    .child(
                                        Label::new(names_count)
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.toggle_database_names(
                                    names_connection.clone(),
                                    names_root.clone(),
                                    cx,
                                );
                            }))
                            .into_any_element(),
                    );
                    if names_open {
                        match self.db_names_status.get(&database.name) {
                            Some(DbListStatus::Loading) | None => {
                                schema_rows.push(
                                    ListItem::new(format!("database-names-loading-{index}"))
                                        .indent_level(2)
                                        .child(
                                            Label::new("connecting…")
                                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                .color(Color::Muted),
                                        )
                                        .into_any_element(),
                                );
                            }
                            Some(DbListStatus::Loaded(names)) if names.is_empty() => {
                                schema_rows.push(
                                    ListItem::new(format!("database-names-empty-{index}"))
                                        .indent_level(2)
                                        .child(
                                            Label::new("no databases")
                                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                .color(Color::Muted),
                                        )
                                        .into_any_element(),
                                );
                            }
                            Some(DbListStatus::Loaded(names)) => {
                                for (name_index, db_name) in names.iter().enumerate() {
                                    schema_rows.push(
                                        ListItem::new(format!(
                                            "database-name-{index}-{name_index}"
                                        ))
                                        .indent_level(2)
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .child(
                                                    Icon::new(IconName::DatabaseZap)
                                                        .size(IconSize::Small)
                                                        .color(Color::Muted),
                                                )
                                                .child(
                                                    Label::new(db_name.clone())
                                                        .size(LabelSize::Custom(PANEL_TEXT_REMS)),
                                                ),
                                        )
                                        .into_any_element(),
                                    );
                                }
                            }
                            Some(DbListStatus::Failed(message)) => {
                                schema_rows.push(
                                    ListItem::new(format!("database-names-error-{index}"))
                                        .indent_level(2)
                                        .child(
                                            Label::new(message.clone())
                                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                                .color(Color::Error)
                                                .truncate(),
                                        )
                                        .into_any_element(),
                                );
                            }
                        }
                    }
                }
            }

            let driver = database.driver.as_deref().unwrap_or("server");
            let paren = match (&database.user, &database.host) {
                (Some(user), Some(host)) => format!("{user}@{host}"),
                (None, Some(host)) => host.clone(),
                _ => display_field(&database.target).to_string(),
            };
            let name = database.name.clone();
            let root = database.root.clone();
            let is_sqlite = driver == "sqlite";
            let status = self
                .database_status
                .get(&database.name)
                .map(|status| match status {
                    DatabaseStatus::Loading => ("loading…".to_string(), Color::Muted),
                    DatabaseStatus::Loaded(tables) => (
                        format!(
                            "{} {}",
                            tables.len(),
                            if tables.len() == 1 { "table" } else { "tables" }
                        ),
                        Color::Default,
                    ),
                    DatabaseStatus::Failed(message) => (message.clone(), Color::Error),
                })
                .or_else(|| {
                    self.query_status
                        .get(&database.name)
                        .and_then(|status| match status {
                            TableStatus::Loading | TableStatus::Loaded(_) => None,
                            TableStatus::Failed(message) => Some((message.clone(), Color::Error)),
                        })
                });

            let mut connection_row = h_flex()
                .id(("database-connection", index))
                .w_full()
                .flex_none()
                .h(px(26.))
                .px_2()
                .gap_1()
                .rounded_sm()
                .cursor_pointer()
                .when(expanded, |this| {
                    this.border_l_2().border_color(cx.theme().colors().border_focused)
                })
                .hover(|this| this.bg(cx.theme().colors().element_hover))
                .child(
                    div().flex_none().child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::Small)
                        .color(Color::Muted),
                    ),
                )
                .child(
                    Icon::new(IconName::DatabaseZap)
                        .size(IconSize::Small)
                        .color(Color::Accent),
                )
                .child(driver_badge(driver, driver_badge_color(driver), cx))
                .child(
                    Label::new(database.name.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Default)
                        .flex_none(),
                )
                .child(
                    Label::new(format!("({})", paren))
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(div().flex_1().min_w_0());
            if let Some((status_text, color)) = status {
                connection_row = connection_row.child(
                    Label::new(status_text)
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(color)
                        .truncate()
                        .flex_none(),
                );
            }
            let menu_name = name.clone();
            connection_row = connection_row
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.deploy_db_context_menu(menu_name.clone(), event.position, window, cx);
                    }),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_connection_expand(name.clone(), is_sqlite, root.clone(), cx);
                }));
            database_rows.push(connection_row.into_any_element());

            database_rows.extend(schema_rows);
        }

        let mut collection_rows = Vec::new();
        let mut collection_row_index = 0usize;
        for collection in &self.collections {
            let requests = collection.requests.iter().collect::<Vec<_>>();
            if requests.is_empty() {
                continue;
            }

            collection_rows.push(
                h_flex()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new(collection.name.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .into_any_element(),
            );

            for request in requests {
                let text = if request.folder.is_empty() {
                    format!("{} · {} · {}", request.name, request.method, request.url)
                } else {
                    format!(
                        "{} / {} · {} · {}",
                        request.folder.join(" / "),
                        request.name,
                        request.method,
                        request.url
                    )
                };
                let key = collection_request_key(request);
                let request_for_click = request.clone();
                let status = self.http_status.get(&key).map(|status| match status {
                    HttpStatus::Running => ("running…".to_string(), Color::Muted),
                    HttpStatus::Status(code) if (200..400).contains(code) => {
                        (format!("HTTP {code}"), Color::Success)
                    }
                    HttpStatus::Status(code) => (format!("HTTP {code}"), Color::Error),
                    HttpStatus::Failed(message) => (message.clone(), Color::Error),
                    HttpStatus::Response(response) => {
                        if (200..400).contains(&response.status) {
                            (
                                format!("HTTP {} · {} ms", response.status, response.time_ms),
                                Color::Success,
                            )
                        } else {
                            (
                                format!("HTTP {} · {} ms", response.status, response.time_ms),
                                Color::Error,
                            )
                        }
                    }
                });

                let mut item = ListItem::new(("collection-request", collection_row_index))
                    .height(px(26.))
                    .rounded()
                    .selectable(true)
                    .child(Label::new(text).truncate())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.run_collection_request(request_for_click.clone(), cx);
                    }));
                if let Some((status_text, color)) = status {
                    item = item.end_slot(
                        Label::new(status_text)
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(color)
                            .truncate(),
                    );
                }
                collection_rows.push(item.into_any_element());
                collection_row_index += 1;
            }
        }

        let http_rows = self
            .http_requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                let file_name = request
                    .file
                    .path
                    .as_unix_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default();
                let text = format!(
                    "{} · {} · {} · {}",
                    file_name, request.name, request.method, request.url
                );
                let key = http_request_key(request);
                let request_for_click = request.clone();
                let status = self.http_status.get(&key).map(|status| match status {
                    HttpStatus::Running => ("running…".to_string(), Color::Muted),
                    HttpStatus::Status(code) if (200..400).contains(code) => {
                        (format!("HTTP {code}"), Color::Success)
                    }
                    HttpStatus::Status(code) => (format!("HTTP {code}"), Color::Error),
                    HttpStatus::Failed(message) => (message.clone(), Color::Error),
                    HttpStatus::Response(response) => {
                        if (200..400).contains(&response.status) {
                            (
                                format!("HTTP {} · {} ms", response.status, response.time_ms),
                                Color::Success,
                            )
                        } else {
                            (
                                format!("HTTP {} · {} ms", response.status, response.time_ms),
                                Color::Error,
                            )
                        }
                    }
                });

                let mut item = ListItem::new(("http-request", index))
                    .height(px(26.))
                    .rounded()
                    .selectable(true)
                    .child(Label::new(text).truncate())
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.run_http_request(request_for_click.clone(), cx);
                        } else {
                            this.load_http_request_into_builder(
                                request_for_click.clone(),
                                window,
                                cx,
                            );
                        }
                    }));
                if let Some((status_text, color)) = status {
                    item = item.end_slot(
                        Label::new(status_text)
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(color)
                            .truncate(),
                    );
                }
                item.into_any_element()
            })
            .collect::<Vec<_>>();

        let tests_running = matches!(self.test_run, Some(TestRunState::Running));
        let (tests_summary_text, tests_summary_color) = match &self.test_run {
            Some(TestRunState::Running) => ("running…".to_string(), Color::Muted),
            Some(TestRunState::Finished(result)) => (result.summary.text(), Color::Default),
            Some(TestRunState::Failed(message)) => (message.clone(), Color::Error),
            None => match &self.test_list_status {
                TestListStatus::Loading => ("loading…".to_string(), Color::Muted),
                TestListStatus::Failed(message) => (message.clone(), Color::Error),
                _ => ("idle".to_string(), Color::Muted),
            },
        };

        let tests_toolbar = h_flex()
            .id("tests-toolbar")
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                IconButton::new("tests-run-all", IconName::PlayFilled)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Run all tests"))
                    .aria_label("Run all tests")
                    .disabled(tests_running)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.run_tests(None, cx);
                    })),
            )
            .child(
                IconButton::new("tests-refresh", IconName::RotateCw)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Refresh test files"))
                    .aria_label("Refresh test files")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.refresh_tests(cx);
                    })),
            )
            .child(
                Label::new(tests_summary_text)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(tests_summary_color)
                    .truncate()
                    .flex_1(),
            );

        let mut test_rows = Vec::new();
        for (index, file) in self.test_files.iter().enumerate() {
            let file_for_click = file.clone();
            let mut item = ListItem::new(("test-file", index))
                .height(px(26.))
                .rounded()
                .selectable(true)
                .child(Label::new(file.clone()).truncate())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.run_tests(Some(file_for_click.clone()), cx);
                }));
            let status = match &self.test_run {
                Some(TestRunState::Running)
                    if self.test_active_file.as_deref() == Some(file.as_str()) =>
                {
                    Some(("running…".to_string(), Color::Muted))
                }
                Some(TestRunState::Finished(result)) => {
                    let cases = result
                        .tests
                        .iter()
                        .filter(|case| case.file == *file)
                        .collect::<Vec<_>>();
                    if cases.is_empty() {
                        None
                    } else {
                        let passed = cases
                            .iter()
                            .filter(|case| case.status == TestStatus::Pass)
                            .count();
                        let failed = cases
                            .iter()
                            .filter(|case| {
                                matches!(case.status, TestStatus::Fail | TestStatus::Error)
                            })
                            .count();
                        let color = if failed > 0 {
                            Color::Error
                        } else if passed > 0 {
                            Color::Success
                        } else {
                            Color::Muted
                        };
                        Some((format!("{passed}/{} pass", cases.len()), color))
                    }
                }
                _ => None,
            };
            if let Some((status_text, color)) = status {
                item = item.end_slot(
                    Label::new(status_text)
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(color)
                        .truncate(),
                );
            }
            test_rows.push(item.into_any_element());
        }

        if test_rows.is_empty() {
            match &self.test_list_status {
                TestListStatus::Failed(message) => test_rows.push(
                    empty_state(IconName::Warning, "Tests failed to load", message.clone(), cx),
                ),
                TestListStatus::Loading => test_rows.push(empty_state(
                    IconName::Info,
                    "Loading tests…",
                    "",
                    cx,
                )),
                _ => test_rows.push(empty_state(
                    IconName::PlayFilled,
                    "No test files",
                    "Refresh to discover PHPUnit tests",
                    cx,
                )),
            }
        }

        let test_result_rows = match &self.test_run {
            Some(TestRunState::Running) => vec![
                h_flex()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new("running…")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            ],
            Some(TestRunState::Failed(message)) => vec![
                h_flex()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new(message.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Error)
                            .truncate(),
                    )
                    .into_any_element(),
            ],
            Some(TestRunState::Finished(result)) => result
                .tests
                .iter()
                .map(|case| {
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .items_start()
                        .child(
                            Label::new(case.status.label())
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(case.status.color())
                                .flex_none(),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    Label::new(format!("{}::{}", case.class, case.name))
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(case.status.color())
                                        .truncate(),
                                )
                                .when(!case.message.is_empty(), |this| {
                                    this.child(
                                        Label::new(case.message.clone())
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
                                }),
                        )
                        .into_any_element()
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        };

        let tests_results_header = h_flex()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                Label::new(match &self.test_run {
                    Some(TestRunState::Finished(result)) => format!(
                        "Results · {} tests · {}",
                        result.summary.total,
                        result.summary.text()
                    ),
                    Some(TestRunState::Running) => "Results · running…".to_string(),
                    Some(TestRunState::Failed(_)) => "Results · failed".to_string(),
                    None => "Results".to_string(),
                })
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(Color::Muted),
            );

        let sql_target = self.databases.first().map(|first| first.name.clone());

        let database_toolbar = h_flex()
            .id("database-toolbar")
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                IconButton::new("db-add", IconName::Plus)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Add data source"))
                    .aria_label("Add data source")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_db_menu(cx);
                    })),
            )
            .child(
                IconButton::new("database-refresh", IconName::RotateCw)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("Refresh"))
                    .aria_label("Refresh databases")
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.refresh_databases(cx);
                    })),
            )
            .child(
                IconButton::new("database-sql", IconName::Terminal)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::Small)
                    .icon_color(Color::Muted)
                    .tooltip(Tooltip::text("SQL console"))
                    .aria_label("Open SQL console")
                    .disabled(sql_target.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(connection) = sql_target.clone() {
                            this.open_sql_form(connection, window, cx);
                        }
                    })),
            );

        let database_driver_menu = if self.db_menu_open {
            let root = self.worktree_root(cx);
            let sqlite_demo_exists = root
                .as_ref()
                .map(|root| root.join("demo.sqlite").is_file())
                .unwrap_or(false);
            let mut menu_rows: Vec<gpui::AnyElement> = Vec::new();
            menu_rows.push(
                h_flex()
                    .w_full()
                    .flex_none()
                    .px_2()
                    .pt_1()
                    .pb_0p5()
                    .child(
                        Label::new("Create Data Source")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
            let mut demo_header_added = false;
            for (menu_index, row) in create_data_source_menu(sqlite_demo_exists)
                .into_iter()
                .enumerate()
            {
                match row.action {
                    CreateDataSourceAction::Driver(driver) => {
                        let seed = DataSourceSeed::driver(driver);
                        menu_rows.push(
                            h_flex()
                                .id(("cds-driver", menu_index))
                                .w_full()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .child(driver_badge(
                                    driver_badge_short(driver),
                                    driver_badge_color(driver),
                                    cx,
                                ))
                                .child(
                                    Label::new(create_data_source_driver_label(driver))
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Default)
                                        .truncate(),
                                )
                                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                    this.db_menu_open = false;
                                    if let Some(root) = this.worktree_root(cx) {
                                        open_data_sources_window_seeded(
                                            root,
                                            Some(seed.clone()),
                                            cx,
                                        );
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                    CreateDataSourceAction::AllDataSources => {
                        menu_rows.push(
                            h_flex()
                                .id("cds-all")
                                .w_full()
                                .flex_none()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .child(
                                    Label::new("All Data Sources...")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Default)
                                        .truncate(),
                                )
                                .child(div().flex_1())
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(IconSize::Small)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.db_menu_open = false;
                                    if let Some(root) = this.worktree_root(cx) {
                                        open_data_sources_window(root, cx);
                                    }
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                    CreateDataSourceAction::Demo(demo) => {
                        if !demo_header_added {
                            demo_header_added = true;
                            menu_rows.push(
                                h_flex()
                                    .w_full()
                                    .flex_none()
                                    .px_2()
                                    .pt_1()
                                    .pb_0p5()
                                    .child(
                                        Label::new("Explore Demo Databases")
                                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                            .color(Color::Muted),
                                    )
                                    .into_any_element(),
                            );
                        }
                        let enabled = row.enabled;
                        let seed = DataSourceSeed::demo(demo);
                        let mut item = h_flex()
                            .id(("cds-demo", menu_index))
                            .w_full()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_sm();
                        if enabled {
                            item = item
                                .cursor_pointer()
                                .hover(|style| style.bg(cx.theme().colors().element_hover))
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _window, cx| {
                                        this.db_menu_open = false;
                                        if let Some(root) = this.worktree_root(cx) {
                                            open_data_sources_window_seeded(
                                                root,
                                                Some(seed.clone()),
                                                cx,
                                            );
                                        }
                                        cx.notify();
                                    },
                                ));
                        } else {
                            item = item.opacity(0.55).tooltip(Tooltip::text(
                                demo.disabled_reason().unwrap_or_default(),
                            ));
                        }
                        menu_rows.push(
                            item.child(driver_badge(
                                driver_badge_short(demo.driver()),
                                driver_badge_color(demo.driver()),
                                cx,
                            ))
                            .child(
                                Label::new(demo.label())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(if enabled {
                                        Color::Default
                                    } else {
                                        Color::Muted
                                    })
                                    .truncate(),
                            )
                            .into_any_element(),
                        );
                    }
                }
            }
            Some(
                v_flex()
                    .id("db-driver-menu")
                    .w_full()
                    .flex_none()
                    .gap_0p5()
                    .px_1()
                    .py_1()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .bg(cx.theme().colors().elevated_surface_background)
                    .shadow_sm()
                    .max_h(px(460.))
                    .overflow_y_scroll()
                    .children(menu_rows),
            )
        } else {
            None
        };

        let saved_message_row = self.saved_message.clone().map(|message| {
            let color = if message.starts_with("failed") {
                Color::Error
            } else {
                Color::Success
            };
            h_flex()
                .id("php-panel-message")
                .w_full()
                .flex_none()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().colors().border)
                .child(
                    Label::new(message)
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(color)
                        .truncate(),
                )
        });

        let tab_bar = render_tab_bar(self.active_tab, cx);

        let content = match self.active_tab {
            PanelTab::Remote => v_flex()
                .id("remote-content")
                .relative()
                .flex_1()
                .min_h_0()
                .child(remote_toolbar)
                .children(remote_server_menu)
                .child(remote_status)
                .when_some(remote_breadcrumb, |this, crumb| this.child(crumb))
                .child(section_header("Remote Files", cx))
                .child(
                    div()
                        .id("php-remote-tree")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(remote_rows),
                )
                .into_any_element(),
            PanelTab::Databases => v_flex()
                .id("database-content")
                .relative()
                .flex_1()
                .min_h_0()
                .child(database_toolbar)
                .children(database_driver_menu)
                .when_some(
                    self.form
                        .as_ref()
                        .filter(|form| matches!(form.kind, FormKind::Sql)),
                    |this, form| this.child(render_form(form)),
                )
                .child(section_header("Data Sources", cx))
                .child(
                    div()
                        .id("php-database-tree")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(if database_rows.is_empty() {
                            vec![empty_state(IconName::DatabaseZap, "No databases configured", "Create a data source to get started", cx)]
                        } else {
                            database_rows
                        }),
                )
                .children(self.db_context_menu.as_ref().map(|context_menu| {
                    deferred(
                        anchored()
                            .position(context_menu.position)
                            .child(context_menu.menu.clone()),
                    )
                    .with_priority(3)
                }))
                .into_any_element(),
            PanelTab::Tests => v_flex()
                .flex_1()
                .min_h_0()
                .child(tests_toolbar)
                .child(section_header("Test Files", cx))
                .child(
                    div()
                        .id("php-tests-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(test_rows),
                )
                .child(tests_results_header)
                .child(
                    div()
                        .id("php-test-results")
                        .flex_shrink_0()
                        .max_h(relative(0.5))
                        .overflow_y_scroll()
                        .children(test_result_rows),
                )
                .into_any_element(),
            PanelTab::Collections => v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(section_header("Collections", cx))
                .child(
                    div()
                        .id("php-collections-scroll")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .children(if collection_rows.is_empty() {
                            vec![empty_state(
                                IconName::File,
                                "No collections",
                                "Add a Postman collection to the project",
                                cx,
                            )]
                        } else {
                            collection_rows
                        }),
                )
                .into_any_element(),
            PanelTab::Http => v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_http_builder(window, cx))
                .child(
                    div()
                        .id("php-http-scroll")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_y_scroll()
                        .children(if http_rows.is_empty() {
                            vec![empty_state(
                                IconName::Info,
                                "No HTTP requests",
                                "Add a .http file to the project",
                                cx,
                            )]
                        } else {
                            http_rows
                        }),
                )
                .into_any_element(),
        };

        v_flex()
            .id("php-workbench")
            .size_full()
            .track_focus(&self.focus_handle)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let modifiers = &event.keystroke.modifiers;
                if this.form.is_none()
                    && modifiers.control
                    && modifiers.alt
                    && !modifiers.shift
                    && !modifiers.platform
                    && !modifiers.function
                {
                    if let Ok(index) = event.keystroke.key.parse::<usize>() {
                        if (1..=PanelTab::VISIBLE.len()).contains(&index) {
                            this.select_tab(PanelTab::VISIBLE[index - 1], cx);
                            return;
                        }
                    }
                }
                if modifiers.platform || modifiers.control || modifiers.alt {
                    return;
                }
                if this.form.is_some() {
                    this.handle_form_key(event, cx);
                    return;
                }
            }))
            .children(saved_message_row)
            .child(tab_bar)
            .child(content)
    }
}

impl PhpPanel {
    fn new_http_pair(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        kind: HttpPairKind,
    ) -> PairEditors {
        let key = cx.new(|cx| Editor::single_line(window, cx));
        let value = cx.new(|cx| Editor::single_line(window, cx));
        let wants = move |event: &EditorEvent| {
            if kind == HttpPairKind::Params {
                matches!(event, EditorEvent::BufferEdited)
            } else {
                matches!(event, EditorEvent::Blurred)
            }
        };
        let key_sub = cx.subscribe(&key, move |this: &mut Self, _, event, cx| {
            if wants(event) && !this.http_sync_guard {
                this.on_http_pair_blurred(kind, cx);
            }
        });
        let value_sub = cx.subscribe(&value, move |this: &mut Self, _, event, cx| {
            if wants(event) && !this.http_sync_guard {
                this.on_http_pair_blurred(kind, cx);
            }
        });
        self._subscriptions.push(key_sub);
        self._subscriptions.push(value_sub);
        PairEditors {
            enabled: true,
            key,
            value,
        }
    }

    fn on_http_pair_blurred(&mut self, kind: HttpPairKind, cx: &mut Context<Self>) {
        self.sync_http_state_from_editors(cx);
        if kind == HttpPairKind::Params {
            self.http_url_from_rows = true;
            self.http_rows_from_url = false;
        }
        self.persist_http_settings(cx);
        self.http_builder_response = None;
        cx.notify();
    }

    fn sync_http_state_from_editors(&mut self, cx: &mut Context<Self>) {
        let Some(editors) = self.http_builder.as_ref() else {
            return;
        };
        let read = |editor: &Entity<Editor>| editor.read(cx).text(cx);
        let params = editors
            .params
            .iter()
            .map(|pair| KeyValueRow {
                enabled: pair.enabled,
                key: read(&pair.key),
                value: read(&pair.value),
            })
            .collect::<Vec<_>>();
        let headers = editors
            .headers
            .iter()
            .map(|pair| KeyValueRow {
                enabled: pair.enabled,
                key: read(&pair.key),
                value: read(&pair.value),
            })
            .collect::<Vec<_>>();
        let body_fields = editors
            .body_fields
            .iter()
            .map(|pair| KeyValueRow {
                enabled: pair.enabled,
                key: read(&pair.key),
                value: read(&pair.value),
            })
            .collect::<Vec<_>>();
        self.http_params = params;
        self.http_headers = headers;
        self.http_body_fields = body_fields;
        self.http_raw_body = read(&editors.raw_body);
        self.http_auth.api_key_name = read(&editors.auth_api_key_name);
        self.http_auth.api_key_value = read(&editors.auth_api_key_value);
        self.http_auth.username = read(&editors.auth_username);
        self.http_auth.password = read(&editors.auth_password);
        self.http_auth.token = read(&editors.auth_token);
        self.http_timeout_secs = read(&editors.timeout)
            .trim()
            .parse::<u32>()
            .unwrap_or(default_timeout());
    }

    fn ensure_http_pair_count(
        &mut self,
        kind: HttpPairKind,
        desired: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = match kind {
            HttpPairKind::Params => self.http_builder.as_ref().map(|e| e.params.len()),
            HttpPairKind::Headers => self.http_builder.as_ref().map(|e| e.headers.len()),
            HttpPairKind::BodyFields => self.http_builder.as_ref().map(|e| e.body_fields.len()),
        }
        .unwrap_or(0);
        if current == desired {
            return;
        }
        if current < desired {
            let mut added = Vec::new();
            for _ in current..desired {
                added.push(self.new_http_pair(window, cx, kind));
            }
            if let Some(editors) = self.http_builder.as_mut() {
                match kind {
                    HttpPairKind::Params => editors.params.extend(added),
                    HttpPairKind::Headers => editors.headers.extend(added),
                    HttpPairKind::BodyFields => editors.body_fields.extend(added),
                }
            }
        } else if let Some(editors) = self.http_builder.as_mut() {
            match kind {
                HttpPairKind::Params => editors.params.truncate(desired),
                HttpPairKind::Headers => editors.headers.truncate(desired),
                HttpPairKind::BodyFields => editors.body_fields.truncate(desired),
            }
        }
    }

    /// Applies pending two-way URL/table and load-time editor updates after the
    /// current frame, avoiding re-entrant entity updates from `set_text`.
    fn schedule_http_editor_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.http_sync_scheduled
            || !(self.http_rows_from_url
                || self.http_url_from_rows
                || self.http_rows_dirty
                || self.http_auth_dirty)
        {
            return;
        }
        self.http_sync_scheduled = true;
        let params = self.http_params.clone();
        let headers = self.http_headers.clone();
        let body_fields = self.http_body_fields.clone();
        let auth = self.http_auth.clone();
        let raw_body = self.http_raw_body.clone();
        let timeout = self.http_timeout_secs.to_string();
        let rows_from_url = self.http_rows_from_url;
        let url_from_rows = self.http_url_from_rows;
        let rows_dirty = self.http_rows_dirty;
        let auth_dirty = self.http_auth_dirty;
        cx.defer_in(window, move |this, window, cx| {
            this.http_sync_guard = true;
            this.ensure_http_pair_count(
                HttpPairKind::Params,
                params.len().max(1),
                window,
                cx,
            );
            this.ensure_http_pair_count(
                HttpPairKind::Headers,
                headers.len().max(1),
                window,
                cx,
            );
            this.ensure_http_pair_count(
                HttpPairKind::BodyFields,
                body_fields.len().max(1),
                window,
                cx,
            );
            if rows_from_url || rows_dirty {
                if let Some(editors) = this.http_builder.as_ref() {
                    let mut set_pair = |pair: &PairEditors, row: &KeyValueRow| {
                        pair.key.update(cx, |editor, cx| {
                            editor.set_text(row.key.clone(), window, cx);
                        });
                        pair.value.update(cx, |editor, cx| {
                            editor.set_text(row.value.clone(), window, cx);
                        });
                    };
                    for (pair, row) in editors.params.iter().zip(params.iter()) {
                        set_pair(pair, row);
                    }
                    for (pair, row) in editors.headers.iter().zip(headers.iter()) {
                        set_pair(pair, row);
                    }
                    for (pair, row) in editors.body_fields.iter().zip(body_fields.iter()) {
                        set_pair(pair, row);
                    }
                }
            }
            if auth_dirty {
                if let Some(editors) = this.http_builder.as_ref() {
                    let mut set = |editor: &Entity<Editor>, value: &str| {
                        editor.update(cx, |editor, cx| {
                            editor.set_text(value.to_string(), window, cx);
                        });
                    };
                    set(&editors.auth_api_key_name, &auth.api_key_name);
                    set(&editors.auth_api_key_value, &auth.api_key_value);
                    set(&editors.auth_username, &auth.username);
                    set(&editors.auth_password, &auth.password);
                    set(&editors.auth_token, &auth.token);
                    set(&editors.raw_body, &raw_body);
                    set(&editors.timeout, &timeout);
                }
            }
            if url_from_rows {
                if let Some(editors) = this.http_builder.as_ref() {
                    let current = editors.url.read(cx).text(cx);
                    let (base, _) = parse_url_query(&current);
                    let url = serialize_url_query(&base, &params);
                    editors.url.update(cx, |editor, cx| {
                        editor.set_text(url, window, cx);
                    });
                }
            }
            this.http_sync_guard = false;
            this.http_sync_scheduled = false;
            this.http_rows_from_url = false;
            this.http_url_from_rows = false;
            this.http_rows_dirty = false;
            this.http_auth_dirty = false;
            cx.notify();
        });
    }

    fn current_http_url(&self, cx: &Context<Self>) -> String {
        let Some(editors) = self.http_builder.as_ref() else {
            return String::new();
        };
        let current = editors.url.read(cx).text(cx);
        if self.http_url_from_rows {
            let (base, _) = parse_url_query(&current);
            serialize_url_query(&base, &self.http_params)
        } else {
            current
        }
    }

    fn persist_http_settings(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.http_active_request.clone() else {
            return;
        };
        let Some(root) = self.http_settings_root.clone() else {
            return;
        };
        let url = self.current_http_url(cx);
        let key = http_settings_key(request.file.path.as_unix_str(), &request.name);
        let settings = HttpRequestSettings {
            method: self.http_method.clone(),
            url,
            subtab: Some(self.http_subtab),
            params: self.http_params.clone(),
            auth: self.http_auth.clone(),
            headers: self.http_headers.clone(),
            body_kind: self.http_body_kind,
            raw_content_type: self.http_raw_content_type,
            raw_body: self.http_raw_body.clone(),
            body_fields: self.http_body_fields.clone(),
            follow_redirects: self.http_follow_redirects,
            timeout_secs: self.http_timeout_secs,
            verify_tls: self.http_verify_tls,
        };
        self.http_settings_file
            .requests
            .insert(key, settings);
        let _ = save_http_settings(&root, &self.http_settings_file);
    }

    fn toggle_http_pair(&mut self, kind: HttpPairKind, index: usize, cx: &mut Context<Self>) {
        if let Some(editors) = self.http_builder.as_mut() {
            let pair = match kind {
                HttpPairKind::Params => editors.params.get_mut(index),
                HttpPairKind::Headers => editors.headers.get_mut(index),
                HttpPairKind::BodyFields => editors.body_fields.get_mut(index),
            };
            if let Some(pair) = pair {
                pair.enabled = !pair.enabled;
            }
        }
        self.sync_http_state_from_editors(cx);
        self.persist_http_settings(cx);
        cx.notify();
    }

    fn add_http_pair(
        &mut self,
        kind: HttpPairKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pair = self.new_http_pair(window, cx, kind);
        if let Some(editors) = self.http_builder.as_mut() {
            match kind {
                HttpPairKind::Params => editors.params.push(pair),
                HttpPairKind::Headers => editors.headers.push(pair),
                HttpPairKind::BodyFields => editors.body_fields.push(pair),
            }
        }
        self.sync_http_state_from_editors(cx);
        self.http_rows_dirty = true;
        cx.notify();
    }

    fn remove_http_pair(&mut self, kind: HttpPairKind, index: usize, cx: &mut Context<Self>) {
        if let Some(editors) = self.http_builder.as_mut() {
            match kind {
                HttpPairKind::Params => {
                    if index < editors.params.len() {
                        editors.params.remove(index);
                    }
                }
                HttpPairKind::Headers => {
                    if index < editors.headers.len() {
                        editors.headers.remove(index);
                    }
                }
                HttpPairKind::BodyFields => {
                    if index < editors.body_fields.len() {
                        editors.body_fields.remove(index);
                    }
                }
            }
        }
        self.sync_http_state_from_editors(cx);
        self.http_rows_dirty = true;
        if kind == HttpPairKind::Params {
            self.http_url_from_rows = true;
        }
        self.persist_http_settings(cx);
        cx.notify();
    }

    fn ensure_http_editors(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> &HttpBuilderEditors {
        if self.http_builder.is_none() {
            let url = cx.new(|cx| Editor::single_line(window, cx));
            let raw_body = cx.new(|cx| Editor::auto_height(6, 18, window, cx));
            let auth_api_key_name = cx.new(|cx| Editor::single_line(window, cx));
            let auth_api_key_value = cx.new(|cx| Editor::single_line(window, cx));
            let auth_username = cx.new(|cx| Editor::single_line(window, cx));
            let auth_password = cx.new(|cx| Editor::single_line(window, cx));
            let auth_token = cx.new(|cx| Editor::single_line(window, cx));
            let timeout = cx.new(|cx| Editor::single_line(window, cx));

            let url_sub = cx.subscribe(&url, |this: &mut Self, editor, event, cx| {
                if this.http_sync_guard {
                    return;
                }
                match event {
                    EditorEvent::BufferEdited => {
                        this.http_builder_response = None;
                        cx.notify();
                    }
                    EditorEvent::Blurred => {
                        let text = editor.read(cx).text(cx);
                        let (_, rows) = parse_url_query(&text);
                        this.http_params = rows;
                        this.http_rows_from_url = true;
                        this.http_url_from_rows = false;
                        this.http_rows_dirty = true;
                        this.persist_http_settings(cx);
                        cx.notify();
                    }
                    _ => {}
                }
            });
            let raw_body_sub = cx.subscribe(&raw_body, |this: &mut Self, editor, event, cx| {
                if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                    this.http_raw_body = editor.read(cx).text(cx);
                    this.persist_http_settings(cx);
                }
            });
            let auth_subs = [
                cx.subscribe(&auth_api_key_name, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
                cx.subscribe(&auth_api_key_value, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
                cx.subscribe(&auth_username, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
                cx.subscribe(&auth_password, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
                cx.subscribe(&auth_token, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
                cx.subscribe(&timeout, |this: &mut Self, _, event, cx| {
                    if matches!(event, EditorEvent::Blurred) && !this.http_sync_guard {
                        this.sync_http_state_from_editors(cx);
                        this.persist_http_settings(cx);
                    }
                }),
            ];
            self._subscriptions.push(url_sub);
            self._subscriptions.push(raw_body_sub);
            self._subscriptions.extend(auth_subs);

            let params = vec![self.new_http_pair(window, cx, HttpPairKind::Params)];
            let headers = vec![self.new_http_pair(window, cx, HttpPairKind::Headers)];
            let body_fields = vec![self.new_http_pair(window, cx, HttpPairKind::BodyFields)];
            self.http_builder = Some(HttpBuilderEditors {
                url,
                raw_body,
                auth_api_key_name,
                auth_api_key_value,
                auth_username,
                auth_password,
                auth_token,
                timeout,
                params,
                headers,
                body_fields,
            });
        }
        self.http_builder.as_ref().expect("http editors")
    }

    fn load_http_request_into_builder(
        &mut self,
        request: HttpRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.persist_http_settings(cx);
        let root = request.root.clone();
        if self.http_settings_root.as_deref() != Some(root.as_path()) {
            self.http_settings_file = load_http_settings(&root);
            self.http_settings_root = Some(root);
        }
        let detail = std::fs::read_to_string(&request.absolute_path)
            .ok()
            .and_then(|contents| parse_http_request_detail(&contents, &request.name));
        let key = http_settings_key(request.file.path.as_unix_str(), &request.name);
        let stored = self.http_settings_file.requests.get(&key).cloned();
        let mut settings = stored.unwrap_or_else(|| {
            let mut settings = HttpRequestSettings::default();
            if let Some(detail) = &detail {
                settings.method = detail.method.clone();
                settings.url = detail.url.clone();
                settings.headers = detail
                    .headers
                    .iter()
                    .map(|(name, value)| KeyValueRow::new(name.clone(), value.clone()))
                    .collect();
                if !detail.body.is_empty() {
                    settings.body_kind = HttpBodyKind::Raw;
                    settings.raw_body = detail.body.clone();
                }
            }
            settings
        });
        if settings.method.is_empty() {
            settings.method = "GET".to_string();
        }
        if settings.params.is_empty() && settings.url.contains('?') {
            let (_, rows) = parse_url_query(&settings.url);
            settings.params = rows;
        }

        let loaded_url = settings.url.clone();
        self.http_method = settings.method;
        self.http_subtab = settings.subtab.unwrap_or(HttpSubTab::Params);
        self.http_params = settings.params;
        self.http_auth = settings.auth;
        self.http_headers = settings.headers;
        self.http_body_kind = settings.body_kind;
        self.http_raw_content_type = settings.raw_content_type;
        self.http_raw_body = settings.raw_body;
        self.http_body_fields = settings.body_fields;
        self.http_follow_redirects = settings.follow_redirects;
        self.http_timeout_secs = settings.timeout_secs;
        self.http_verify_tls = settings.verify_tls;
        self.http_active_request = Some(request.clone());
        self.http_rows_dirty = true;
        self.http_auth_dirty = true;
        self.http_rows_from_url = false;
        self.http_url_from_rows = false;

        self.ensure_http_editors(window, cx);
        self.http_sync_guard = true;
        if let Some(editors) = self.http_builder.as_ref() {
            let mut set = |editor: &Entity<Editor>, value: &str| {
                editor.update(cx, |editor, cx| {
                    editor.set_text(value.to_string(), window, cx);
                });
            };
            set(&editors.url, &loaded_url);
            set(&editors.raw_body, &self.http_raw_body);
            set(&editors.auth_api_key_name, &self.http_auth.api_key_name);
            set(&editors.auth_api_key_value, &self.http_auth.api_key_value);
            set(&editors.auth_username, &self.http_auth.username);
            set(&editors.auth_password, &self.http_auth.password);
            set(&editors.auth_token, &self.http_auth.token);
            set(&editors.timeout, &self.http_timeout_secs.to_string());
        }
        self.http_sync_guard = false;
        self.saved_message = Some(format!("loaded request: {}", request.name));
        cx.notify();
    }

    fn render_http_pair_table(
        &mut self,
        kind: HttpPairKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editors) = self.http_builder.as_ref() else {
            return div().into_any_element();
        };
        let pairs = match kind {
            HttpPairKind::Params => editors.params.clone(),
            HttpPairKind::Headers => editors.headers.clone(),
            HttpPairKind::BodyFields => editors.body_fields.clone(),
        };
        let colors = cx.theme().colors();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, pair) in pairs.iter().enumerate() {
            let panel = cx.entity().downgrade();
            let toggle_panel = panel.clone();
            let row = h_flex()
                .id(format!("http-pair-row-{}-{}", kind as usize, index))
                .w_full()
                .flex_none()
                .items_center()
                .gap_1()
                .h(px(28.))
                .px_1()
                .rounded_sm()
                .hover(|style| style.bg(colors.element_hover))
                .child(
                    Checkbox::new(
                        format!("http-pair-check-{}-{}", kind as usize, index),
                        if pair.enabled {
                            ToggleState::Selected
                        } else {
                            ToggleState::Unselected
                        },
                    )
                    .on_click(move |_state, _window, cx| {
                        if let Some(panel) = toggle_panel.upgrade() {
                            let _ = panel.update(cx, |this, cx| {
                                this.toggle_http_pair(kind, index, cx);
                            });
                        }
                    }),
                )
                .child(dialog_editor(
                    format!("http-pair-key-{}-{}", kind as usize, index),
                    &pair.key,
                    None,
                    window,
                    cx,
                ))
                .child(dialog_editor(
                    format!("http-pair-value-{}-{}", kind as usize, index),
                    &pair.value,
                    None,
                    window,
                    cx,
                ))
                .child(
                    IconButton::new(format!("http-pair-remove-{}-{}", kind as usize, index), IconName::Close)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::Small)
                        .icon_color(Color::Muted)
                        .tooltip(Tooltip::text("Remove row"))
                        .aria_label("Remove row")
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            this.remove_http_pair(kind, index, cx);
                        })),
                );
            rows.push(row.into_any_element());
        }
        rows.push(
            h_flex()
                .id(("http-pair-add", kind as usize))
                .w_full()
                .flex_none()
                .items_center()
                .px_1()
                .py_1()
                .child(
                    Button::new(("http-add-row", kind as usize), "＋ Add")
                        .size(ButtonSize::Compact)
                        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .style(ButtonStyle::Subtle)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.add_http_pair(kind, window, cx);
                        })),
                )
                .into_any_element(),
        );
        v_flex()
            .id(("http-pair-table", kind as usize))
            .w_full()
            .gap_0p5()
            .children(rows)
            .into_any_element()
    }

    fn render_http_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.ensure_http_editors(window, cx);
        self.schedule_http_editor_sync(window, cx);
        let colors = cx.theme().colors().clone();
        let method = self.http_method.clone();

        let panel = cx.entity().downgrade();
        let trigger = Button::new("http-method-trigger", method.clone())
            .size(ButtonSize::Compact)
            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
            .style(ButtonStyle::Subtle)
            .tooltip(Tooltip::text("HTTP method"));
        let method_menu = PopoverMenu::new("http-method-menu")
            .menu(move |window, cx| {
                let panel = panel.clone();
                let active = method.clone();
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    for candidate in HTTP_METHODS.iter() {
                        let chosen = candidate.to_string();
                        let menu_panel = panel.clone();
                        let is_active = active == chosen;
                        menu = menu.item(
                            ui::ContextMenuEntry::new(chosen.clone())
                                .toggleable(ui::IconPosition::Start, is_active)
                                .handler(move |_window: &mut Window, cx: &mut App| {
                                    if let Some(panel) = menu_panel.upgrade() {
                                        panel.update(cx, |panel, cx| {
                                            panel.http_method = chosen.clone();
                                            panel.persist_http_settings(cx);
                                            cx.notify();
                                        });
                                    }
                                }),
                        );
                    }
                    menu
                }))
            })
            .anchor(Anchor::BottomLeft)
            .trigger(trigger);

        let url_editor = self
            .http_builder
            .as_ref()
            .expect("http editors")
            .url
            .clone();
        let url_input = div()
            .id("http-url-box")
            .flex_1()
            .min_w_0()
            .h(px(28.))
            .flex()
            .items_center()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(if url_editor.read(cx).focus_handle(cx).is_focused(window) {
                colors.border_focused
            } else {
                colors.border_variant
            })
            .bg(colors.editor_background)
            .cursor_text()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(dialog_editor_element(&url_editor, cx)),
            );

        let busy = self.http_builder_busy;
        let send_label = if busy { "Sending…" } else { "Send" };
        let send_button = dialog_button("http-send", send_label, DialogButtonVariant::Primary)
            .disabled(busy)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.send_http_builder(window, cx);
            }));

        let mut rows = Vec::new();
        rows.push(
            h_flex()
                .id("http-request-line")
                .w_full()
                .flex_none()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(colors.border)
                .child(method_menu)
                .child(url_input)
                .child(send_button)
                .into_any_element(),
        );

        let mut subtab_row = h_flex()
            .id("http-subtabs")
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .border_b_1()
            .border_color(colors.border);
        for tab in HttpSubTab::ALL {
            let selected = self.http_subtab == tab;
            subtab_row = subtab_row.child(
                h_flex()
                    .id(("http-subtab", tab as usize))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .child(
                        Label::new(tab.label())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if selected {
                                Color::Default
                            } else {
                                Color::Muted
                            }),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.http_subtab = tab;
                        this.persist_http_settings(cx);
                        cx.notify();
                    })),
            );
        }
        rows.push(subtab_row.into_any_element());

        let content = match self.http_subtab {
            HttpSubTab::Params => v_flex()
                .id("http-params")
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .child(section_header("Query Params", cx))
                        .child(div().flex_1())
                        .child(
                            Label::new("editing the URL updates these rows; edits here rewrite the URL")
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Muted)
                                .truncate(),
                        ),
                )
                .child(self.render_http_pair_table(HttpPairKind::Params, window, cx))
                .into_any_element(),
            HttpSubTab::Authorization => self.render_http_auth(window, cx),
            HttpSubTab::Headers => {
                let auto = self.auto_generated_headers(cx);
                let mut auto_rows: Vec<AnyElement> = auto
                    .iter()
                    .map(|(name, value)| {
                        h_flex()
                            .w_full()
                            .flex_none()
                            .h(px(24.))
                            .px_1()
                            .gap_1()
                            .child(
                                Label::new(name.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted)
                                    .flex_none(),
                            )
                            .child(
                                Label::new(value.clone())
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                            .into_any_element()
                    })
                    .collect();
                if auto_rows.is_empty() {
                    auto_rows.push(
                        Label::new("no generated headers for the current body")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .into_any_element(),
                    );
                }
                v_flex()
                    .id("http-headers")
                    .w_full()
                    .gap_1()
                    .child(section_header("Headers", cx))
                    .child(self.render_http_pair_table(HttpPairKind::Headers, window, cx))
                    .child(section_header("Auto-generated headers", cx))
                    .child(
                        v_flex()
                            .id("http-auto-headers")
                            .w_full()
                            .rounded_md()
                            .border_1()
                            .border_color(colors.border_variant)
                            .bg(colors.editor_background)
                            .p_1()
                            .gap_0p5()
                            .children(auto_rows),
                    )
                    .into_any_element()
            }
            HttpSubTab::Body => self.render_http_body(window, cx),
            HttpSubTab::Scripts => v_flex()
                .id("http-scripts")
                .w_full()
                .p_3()
                .gap_1()
                .child(section_header("Scripts", cx))
                .child(
                    Label::new("pre-request / test scripts: not supported yet")
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted),
                )
                .into_any_element(),
            HttpSubTab::Settings => self.render_http_settings(window, cx),
        };

        rows.push(
            div()
                .id("http-subtab-content")
                .w_full()
                .flex_none()
                .max_h(px(360.))
                .overflow_y_scroll()
                .px_2()
                .py_1()
                .child(content)
                .into_any_element(),
        );

        if let Some(response) = self.http_builder_response.clone() {
            rows.push(self.render_http_response(&response, cx));
        }

        v_flex()
            .id("http-builder")
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(colors.border)
            .children(rows)
            .into_any_element()
    }

    fn auto_generated_headers(&self, cx: &Context<Self>) -> Vec<(String, String)> {
        let body = compose_http_request(
            "",
            &self.http_headers,
            &self.http_auth,
            self.http_body_kind,
            self.http_raw_content_type,
            &self.http_raw_body,
            &self.http_body_fields,
            "boundary",
        );
        let mut headers = Vec::new();
        if let Some(content_type) = body.content_type {
            headers.push(("Content-Type".to_string(), content_type));
        }
        headers.push(("Accept".to_string(), "*/*".to_string()));
        headers.push(("User-Agent".to_string(), "php-http/curl".to_string()));
        if let Some(body) = &body.body {
            headers.push(("Content-Length".to_string(), body.len().to_string()));
        }
        let _ = cx;
        headers
    }

    fn render_http_auth(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.http_auth.kind;
        let panel = cx.entity().downgrade();
        let trigger = Button::new("http-auth-trigger", kind.label())
            .size(ButtonSize::Compact)
            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
            .style(ButtonStyle::Subtle)
            .tooltip(Tooltip::text("Authorization type"));
        let menu = PopoverMenu::new("http-auth-menu")
            .menu(move |window, cx| {
                let panel = panel.clone();
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    for candidate in HttpAuthType::ALL {
                        let menu_panel = panel.clone();
                        let is_active = candidate == kind;
                        menu = menu.item(
                            ui::ContextMenuEntry::new(candidate.label())
                                .toggleable(ui::IconPosition::Start, is_active)
                                .handler(move |_window: &mut Window, cx: &mut App| {
                                    if let Some(panel) = menu_panel.upgrade() {
                                        panel.update(cx, |panel, cx| {
                                            panel.http_auth.kind = candidate;
                                            panel.persist_http_settings(cx);
                                            cx.notify();
                                        });
                                    }
                                }),
                        );
                    }
                    menu
                }))
            })
            .anchor(Anchor::BottomLeft)
            .trigger(trigger);

        let mut fields = v_flex().id("http-auth-fields").w_full().gap_1();
        if let Some(editors) = self.http_builder.as_ref() {
            match kind {
                HttpAuthType::None => {
                    fields = fields.child(
                        Label::new("This request does not use authorization.")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    );
                }
                HttpAuthType::ApiKey => {
                    fields = fields
                        .child(dialog_row("Key", dialog_editor(
                            "http-auth-key-name",
                            &editors.auth_api_key_name,
                            None,
                            window,
                            cx,
                        )))
                        .child(dialog_row("Value", dialog_editor(
                            "http-auth-key-value",
                            &editors.auth_api_key_value,
                            None,
                            window,
                            cx,
                        )))
                        .child(dialog_row(
                            "Add to",
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("http-auth-key-header", "Header")
                                        .size(ButtonSize::Compact)
                                        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .style(if self.http_auth.api_key_in == ApiKeyLocation::Header {
                                            ButtonStyle::Filled
                                        } else {
                                            ButtonStyle::Subtle
                                        })
                                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                            this.http_auth.api_key_in = ApiKeyLocation::Header;
                                            this.persist_http_settings(cx);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("http-auth-key-query", "Query")
                                        .size(ButtonSize::Compact)
                                        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .style(if self.http_auth.api_key_in == ApiKeyLocation::Query {
                                            ButtonStyle::Filled
                                        } else {
                                            ButtonStyle::Subtle
                                        })
                                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                            this.http_auth.api_key_in = ApiKeyLocation::Query;
                                            this.persist_http_settings(cx);
                                            cx.notify();
                                        })),
                                )
                                .into_any_element(),
                        ));
                }
                HttpAuthType::Basic => {
                    fields = fields
                        .child(dialog_row("Username", dialog_editor(
                            "http-auth-username",
                            &editors.auth_username,
                            None,
                            window,
                            cx,
                        )))
                        .child(dialog_row("Password", dialog_editor(
                            "http-auth-password",
                            &editors.auth_password,
                            None,
                            window,
                            cx,
                        )));
                }
                HttpAuthType::Bearer => {
                    fields = fields.child(dialog_row("Token", dialog_editor(
                        "http-auth-token",
                        &editors.auth_token,
                        None,
                        window,
                        cx,
                    )));
                }
                HttpAuthType::OAuth2 => {
                    fields = fields.child(
                        Label::new("OAuth 2.0 fields are not implemented; no auth header is sent.")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    );
                }
            }
        }
        v_flex()
            .id("http-auth")
            .w_full()
            .gap_2()
            .child(section_header("Authorization", cx))
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(
                        Label::new("Auth type")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .flex_none(),
                    )
                    .child(menu),
            )
            .child(fields)
            .into_any_element()
    }

    fn render_http_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors().clone();
        let mut modes = h_flex().w_full().items_center().gap_1();
        for mode in HttpBodyKind::ALL {
            let selected = self.http_body_kind == mode;
            modes = modes.child(
                Button::new(("http-body-mode", mode as usize), mode.label())
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .style(if selected {
                        ButtonStyle::Filled
                    } else {
                        ButtonStyle::Subtle
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.http_body_kind = mode;
                        this.persist_http_settings(cx);
                        cx.notify();
                    })),
            );
        }

        let mut content = v_flex().w_full().gap_1();
        if let Some(editors) = self.http_builder.as_ref() {
            match self.http_body_kind {
                HttpBodyKind::None => {
                    content = content.child(
                        Label::new("This request has no body.")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    );
                }
                HttpBodyKind::Raw => {
                    let panel = cx.entity().downgrade();
                    let active = self.http_raw_content_type;
                    let trigger = Button::new("http-raw-content-type", active.label())
                        .size(ButtonSize::Compact)
                        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .style(ButtonStyle::Subtle)
                        .tooltip(Tooltip::text("Raw body content type"));
                    let menu = PopoverMenu::new("http-raw-content-type-menu")
                        .menu(move |window, cx| {
                            let panel = panel.clone();
                            Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                for candidate in RawContentType::ALL {
                                    let menu_panel = panel.clone();
                                    let is_active = candidate == active;
                                    menu = menu.item(
                                        ui::ContextMenuEntry::new(candidate.label())
                                            .toggleable(ui::IconPosition::Start, is_active)
                                            .handler(move |_window: &mut Window, cx: &mut App| {
                                                if let Some(panel) = menu_panel.upgrade() {
                                                    panel.update(cx, |panel, cx| {
                                                        panel.http_raw_content_type = candidate;
                                                        panel.persist_http_settings(cx);
                                                        cx.notify();
                                                    });
                                                }
                                            }),
                                    );
                                }
                                menu
                            }))
                        })
                        .anchor(Anchor::BottomLeft)
                        .trigger(trigger);
                    content = content
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Label::new("Content-Type")
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Muted),
                                )
                                .child(menu),
                        )
                        .child(
                            div()
                                .id("http-raw-body-editor")
                                .w_full()
                                .rounded_md()
                                .border_1()
                                .border_color(colors.border_variant)
                                .bg(colors.editor_background)
                                .px_2()
                                .py_1()
                                .cursor_text()
                                .child(dialog_editor_element(&editors.raw_body, cx)),
                        );
                }
                HttpBodyKind::FormData | HttpBodyKind::UrlEncoded => {
                    content = content.child(
                        self.render_http_pair_table(HttpPairKind::BodyFields, window, cx),
                    );
                }
            }
        }
        v_flex()
            .id("http-body")
            .w_full()
            .gap_2()
            .child(section_header("Body", cx))
            .child(modes)
            .child(content)
            .into_any_element()
    }

    fn render_http_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(editors) = self.http_builder.as_ref() else {
            return div().into_any_element();
        };
        let timeout_editor = editors.timeout.clone();
        let follow = self.http_follow_redirects;
        let verify = self.http_verify_tls;
        v_flex()
            .id("http-settings")
            .w_full()
            .gap_2()
            .child(section_header("Settings", cx))
            .child(
                dialog_toggle_with(
                    "http-follow-redirects",
                    follow,
                    "Follow redirects",
                    None,
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.http_follow_redirects = !this.http_follow_redirects;
                    this.persist_http_settings(cx);
                    cx.notify();
                })),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .min_h(px(28.))
                    .child(
                        Label::new("Timeout (seconds)")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .flex_none(),
                    )
                    .child(div().w(px(80.)).child(dialog_editor(
                        "http-timeout",
                        &timeout_editor,
                        Some(px(80.)),
                        window,
                        cx,
                    ))),
            )
            .child(
                dialog_toggle_with("http-verify-tls", verify, "Verify TLS", None, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.http_verify_tls = !this.http_verify_tls;
                        this.persist_http_settings(cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_http_response(
        &mut self,
        response: &HttpProbeResponse,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors().clone();
        let status_color = response_status_color(response.status);
        let content_type = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone());
        let body_text = if self.http_response_pretty {
            pretty_body(&response.body, content_type.as_deref())
        } else {
            response.body.clone()
        };
        let mut header_rows = response
            .headers
            .iter()
            .map(|(name, value)| {
                h_flex()
                    .w_full()
                    .flex_none()
                    .h(px(22.))
                    .gap_2()
                    .child(
                        Label::new(name.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted)
                            .flex_none(),
                    )
                    .child(
                        Label::new(value.clone())
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .truncate(),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        if header_rows.is_empty() {
            header_rows.push(
                Label::new("no response headers")
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted)
                    .into_any_element(),
            );
        }
        let body_active = self.http_response_tab == HttpResponseTab::Body;
        let headers_active = self.http_response_tab == HttpResponseTab::Headers;
        let panel = cx.entity().downgrade();
        let pretty_toggle = Checkbox::new(
            "http-response-pretty",
            if self.http_response_pretty {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            },
        )
        .label("Pretty")
        .on_click(move |_state, _window, cx| {
            if let Some(panel) = panel.upgrade() {
                let _ = panel.update(cx, |this, cx| {
                    this.http_response_pretty = !this.http_response_pretty;
                    cx.notify();
                });
            }
        });

        v_flex()
            .id("http-response")
            .w_full()
            .flex_none()
            .border_t_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .id("http-response-status")
                    .w_full()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .child(
                        div()
                            .flex_none()
                            .px_1()
                            .rounded_sm()
                            .bg(colors.element_hover)
                            .child(
                                Label::new(format!("HTTP {}", response.status))
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(status_color),
                            ),
                    )
                    .child(
                        Label::new(format!("{} ms", response.time_ms))
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .child(
                        Label::new(format!("{} B", response.body.len()))
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    )
                    .child(div().flex_1())
                    .child(pretty_toggle),
            )
            .child(
                h_flex()
                    .id("http-response-tabs")
                    .w_full()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(
                        h_flex()
                            .id("http-response-tab-body")
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .when(body_active, |this| {
                                this.border_b_2().border_color(colors.border_focused)
                            })
                            .child(
                                Label::new("Body")
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(if body_active {
                                        Color::Default
                                    } else {
                                        Color::Muted
                                    }),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.http_response_tab = HttpResponseTab::Body;
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .id("http-response-tab-headers")
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .when(headers_active, |this| {
                                this.border_b_2().border_color(colors.border_focused)
                            })
                            .child(
                                Label::new("Headers")
                                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                    .color(if headers_active {
                                        Color::Default
                                    } else {
                                        Color::Muted
                                    }),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.http_response_tab = HttpResponseTab::Headers;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("http-response-content")
                    .w_full()
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .bg(colors.editor_background)
                    .px_2()
                    .py_1()
                    .children(if body_active {
                        vec![
                            Label::new(body_text)
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Default)
                                .into_any_element(),
                        ]
                    } else {
                        header_rows
                    }),
            )
            .into_any_element()
    }

    fn send_http_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.http_builder_busy {
            return;
        }
        self.ensure_http_editors(window, cx);
        self.sync_http_state_from_editors(cx);
        if self.http_url_from_rows {
            self.schedule_http_editor_sync(window, cx);
        }
        let url = self.current_http_url(cx).trim().to_string();
        if url.is_empty() {
            self.saved_message = Some("failed: URL is empty".to_string());
            cx.notify();
            return;
        }
        let method_text = self.http_method.clone();
        let boundary = multipart_boundary();
        let composed = compose_http_request(
            &url,
            &self.http_headers,
            &self.http_auth,
            self.http_body_kind,
            self.http_raw_content_type,
            &self.http_raw_body,
            &self.http_body_fields,
            &boundary,
        );
        self.persist_http_settings(cx);

        let method = method_text;
        let timeout = self.http_timeout_secs.max(1);
        let follow = self.http_follow_redirects;
        let verify = self.http_verify_tls;
        let headers = composed.headers.clone();
        let body = composed.body.clone();
        let request_url = composed.url.clone();

        self.http_builder_busy = true;
        self.http_builder_response = None;
        cx.notify();

        let task = cx.background_spawn(async move {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "phpworkbench-http-{}-{}",
                std::process::id(),
                nanos
            ));
            std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
            let file = dir.join("request.http");
            let contents = format!("### ad-hoc\n{} {}\n", method, request_url);
            std::fs::write(&file, contents).map_err(|error| error.to_string())?;
            let body_file = dir.join("body.bin");
            if let Some(body) = &body {
                std::fs::write(&body_file, body.as_bytes()).map_err(|error| error.to_string())?;
            }

            let mut command = std::process::Command::new("php-http");
            command
                .arg("run")
                .arg(&file)
                .arg("--name")
                .arg("ad-hoc")
                .arg("--json")
                .arg("--timeout")
                .arg(timeout.to_string());
            if follow {
                command.arg("--follow");
            }
            if !verify {
                command.arg("--insecure");
            }
            for (name, value) in &headers {
                command.arg("--header").arg(format!("{name}: {value}"));
            }
            if body.is_some() {
                command.arg("--raw").arg(&body_file);
            }

            let started = std::time::Instant::now();
            let output = command.output();
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let _ = std::fs::remove_dir_all(&dir);
            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let value: Value = serde_json::from_str(&stdout)
                        .map_err(|_| {
                            first_non_empty_line(stdout.lines())
                                .unwrap_or_else(|| "invalid JSON response".to_string())
                        })?;
                    let status = value
                        .get("status")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u16;
                    let body = value
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let headers = value
                        .get("headers")
                        .and_then(Value::as_object)
                        .map(|object| {
                            object
                                .iter()
                                .map(|(name, value)| {
                                    (
                                        name.clone(),
                                        value.as_str().unwrap_or_default().to_string(),
                                    )
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    Ok(HttpProbeResponse {
                        status,
                        time_ms: elapsed_ms,
                        headers,
                        body,
                    })
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Err(first_non_empty_line(stderr.lines().chain(stdout.lines()))
                        .unwrap_or_else(|| "request failed".to_string()))
                }
                Err(error) => Err(error.to_string()),
            }
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.http_builder_busy = false;
                match result {
                    Ok(response) => {
                        let status = response.status;
                        if let Some(request) = this.http_active_request.as_ref() {
                            this.http_status
                                .insert(http_request_key(request), HttpStatus::Response(response.clone()));
                        }
                        this.http_builder_response = Some(response);
                        this.saved_message = if (200..400).contains(&status) {
                            Some(format!("request finished: HTTP {status}"))
                        } else {
                            Some(format!("failed: HTTP {status}"))
                        };
                    }
                    Err(message) => {
                        this.saved_message = Some(format!("failed: {message}"));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequestDetail {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn parse_http_request_detail(contents: &str, name: &str) -> Option<HttpRequestDetail> {
    let mut section_name: Option<String> = None;
    let mut section_lines: Vec<&str> = Vec::new();
    let mut found: Option<Vec<&str>> = None;
    for line in contents.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("###") {
            if let Some(current) = section_name.take() {
                if current.trim() == name {
                    found = Some(std::mem::take(&mut section_lines));
                    break;
                }
            }
            section_name = Some(rest.trim().to_string());
            section_lines.clear();
        } else {
            section_lines.push(line);
        }
    }
    if found.is_none() {
        if let Some(current) = section_name {
            if current.trim() == name {
                found = Some(section_lines);
            }
        }
    }
    let lines = found?;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("//")
            || line.starts_with('@')
        {
            index += 1;
            continue;
        }
        break;
    }
    let (method, url) = parse_request_line(lines.get(index)?.trim())?;
    index += 1;
    let mut headers = Vec::new();
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() {
            index += 1;
            break;
        }
        if line.starts_with('#') || line.starts_with("//") {
            index += 1;
            continue;
        }
        match line.split_once(':') {
            Some((key, value)) if !key.trim().is_empty() => {
                headers.push((key.trim().to_string(), value.trim().to_string()));
                index += 1;
            }
            _ => break,
        }
    }
    let body = lines[index..].join("\n").trim_end().to_string();
    Some(HttpRequestDetail {
        method,
        url,
        headers,
        body,
    })
}

fn url_encode_component(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn url_decode_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &input[i + 1..i + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(value) => {
                        out.push(value);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_url_query(url: &str) -> (String, Vec<KeyValueRow>) {
    let (before_fragment, fragment) = match url.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (url, None),
    };
    let (base, query) = match before_fragment.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (before_fragment, None),
    };
    let mut rows = Vec::new();
    if let Some(query) = query {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            rows.push(KeyValueRow::new(
                url_decode_component(key),
                url_decode_component(value),
            ));
        }
    }
    let base = match fragment {
        Some(fragment) => format!("{base}#{fragment}"),
        None => base.to_string(),
    };
    (base, rows)
}

fn serialize_url_query(base: &str, rows: &[KeyValueRow]) -> String {
    let (before_fragment, fragment) = match base.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (base, None),
    };
    let base = before_fragment.split('?').next().unwrap_or(before_fragment);
    let pairs = rows
        .iter()
        .filter(|row| row.enabled && !row.key.trim().is_empty())
        .map(|row| {
            format!(
                "{}={}",
                url_encode_component(row.key.trim()),
                url_encode_component(&row.value)
            )
        })
        .collect::<Vec<_>>();
    let mut url = base.to_string();
    if !pairs.is_empty() {
        url.push('?');
        url.push_str(&pairs.join("&"));
    }
    if let Some(fragment) = fragment {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

fn auth_headers(auth: &HttpAuthSettings) -> Vec<(String, String)> {
    use base64::Engine as _;
    match auth.kind {
        HttpAuthType::ApiKey
            if auth.api_key_in == ApiKeyLocation::Header && !auth.api_key_name.trim().is_empty() =>
        {
            vec![(
                auth.api_key_name.trim().to_string(),
                auth.api_key_value.clone(),
            )]
        }
        HttpAuthType::Basic => {
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", auth.username, auth.password));
            vec![("Authorization".to_string(), format!("Basic {encoded}"))]
        }
        HttpAuthType::Bearer if !auth.token.trim().is_empty() => vec![(
            "Authorization".to_string(),
            format!("Bearer {}", auth.token.trim()),
        )],
        _ => Vec::new(),
    }
}

fn auth_query(auth: &HttpAuthSettings) -> Option<(String, String)> {
    if auth.kind == HttpAuthType::ApiKey
        && auth.api_key_in == ApiKeyLocation::Query
        && !auth.api_key_name.trim().is_empty()
    {
        Some((
            auth.api_key_name.trim().to_string(),
            auth.api_key_value.clone(),
        ))
    } else {
        None
    }
}

fn encode_urlencoded(rows: &[KeyValueRow]) -> String {
    rows.iter()
        .filter(|row| row.enabled && !row.key.trim().is_empty())
        .map(|row| {
            format!(
                "{}={}",
                url_encode_component(row.key.trim()),
                url_encode_component(&row.value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn build_multipart(rows: &[KeyValueRow], boundary: &str) -> String {
    let mut out = String::new();
    for row in rows
        .iter()
        .filter(|row| row.enabled && !row.key.trim().is_empty())
    {
        out.push_str("--");
        out.push_str(boundary);
        out.push_str("\r\n");
        out.push_str(&format!(
            "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
            row.key.trim()
        ));
        out.push_str(&row.value);
        out.push_str("\r\n");
    }
    out.push_str("--");
    out.push_str(boundary);
    out.push_str("--\r\n");
    out
}

fn multipart_boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("----phpworkbench-{nanos:x}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComposedRequest {
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    content_type: Option<String>,
}

fn compose_http_request(
    url: &str,
    headers: &[KeyValueRow],
    auth: &HttpAuthSettings,
    body_kind: HttpBodyKind,
    raw_content_type: RawContentType,
    raw_body: &str,
    body_fields: &[KeyValueRow],
    boundary: &str,
) -> ComposedRequest {
    let mut composed_url = url.to_string();
    if let Some((key, value)) = auth_query(auth) {
        let separator = if composed_url.contains('?') { '&' } else { '?' };
        composed_url.push(separator);
        composed_url.push_str(&url_encode_component(&key));
        composed_url.push('=');
        composed_url.push_str(&url_encode_component(&value));
    }

    let mut composed_headers = headers
        .iter()
        .filter(|row| row.enabled && !row.key.trim().is_empty())
        .map(|row| (row.key.trim().to_string(), row.value.clone()))
        .collect::<Vec<_>>();
    composed_headers.extend(auth_headers(auth));

    let (body, content_type) = match body_kind {
        HttpBodyKind::None => (None, None),
        HttpBodyKind::Raw => {
            let body = (!raw_body.is_empty()).then(|| raw_body.to_string());
            (body, Some(raw_content_type.mime().to_string()))
        }
        HttpBodyKind::FormData => (
            Some(build_multipart(body_fields, boundary)),
            Some(format!("multipart/form-data; boundary={boundary}")),
        ),
        HttpBodyKind::UrlEncoded => (
            Some(encode_urlencoded(body_fields)),
            Some("application/x-www-form-urlencoded".to_string()),
        ),
    };

    let has_content_type = composed_headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-type"));
    if !has_content_type {
        if let Some(content_type) = &content_type {
            composed_headers.push(("Content-Type".to_string(), content_type.clone()));
        }
    }

    ComposedRequest {
        url: composed_url,
        headers: composed_headers,
        body,
        content_type,
    }
}

fn http_settings_path(root: &Path) -> PathBuf {
    root.join(".zed/http-settings.json")
}

fn load_http_settings(root: &Path) -> HttpSettingsFile {
    std::fs::read_to_string(http_settings_path(root))
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_http_settings(root: &Path, settings: &HttpSettingsFile) -> Result<()> {
    let path = http_settings_path(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(settings)?;
    std::fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn response_status_color(status: u16) -> Color {
    match status {
        200..=299 => Color::Success,
        300..=399 => Color::Accent,
        400..=499 => Color::Warning,
        _ => Color::Error,
    }
}

fn pretty_body(body: &str, content_type: Option<&str>) -> String {
    let looks_json = content_type
        .map(|content_type| content_type.to_ascii_lowercase().contains("json"))
        .unwrap_or(false)
        || body.trim_start().starts_with('{')
        || body.trim_start().starts_with('[');
    if looks_json {
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            return serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_string());
        }
    }
    body.to_string()
}

const HTTP_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

fn select_tab_cyclically(
    window: &mut Window,
    cx: &mut Context<Workspace>,
    next: fn(PanelTab) -> PanelTab,
) {
    let Some(panel) = cx
        .global::<DeployShared>()
        .panel
        .clone()
        .and_then(|panel| panel.upgrade())
    else {
        return;
    };
    panel.update(cx, |panel, cx| {
        panel.select_tab(next(panel.active_tab), cx);
    });
    let focus_handle = panel.read(cx).focus_handle.clone();
    window.focus(&focus_handle, cx);
}

fn render_tab_bar(active: PanelTab, cx: &Context<PhpPanel>) -> gpui::AnyElement {
    h_flex()
        .id("php-panel-tabs")
        .w_full()
        .flex_none()
        .border_b_1()
        .border_color(cx.theme().colors().border)
        .children(PanelTab::VISIBLE.into_iter().map(|tab| {
            let selected = tab == active;
            h_flex()
                .id(("php-panel-tab", tab as usize))
                .flex_1()
                .justify_center()
                .py_1()
                .cursor_pointer()
                .when(selected, |this| {
                    this.border_b_2()
                        .border_color(cx.theme().colors().border_focused)
                })
                .child(
                    Label::new(tab.label())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(if selected {
                            Color::Default
                        } else {
                            Color::Muted
                        })
                        .truncate(),
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.select_tab(tab, cx);
                }))
                .into_any_element()
        }))
        .into_any_element()
}

fn render_form(form: &FormState) -> gpui::AnyElement {
    let mut rows = Vec::new();
    let title = match form.kind {
        FormKind::Sql => "sql console",
    };
    rows.push(
        h_flex()
            .px_3()
            .py_1()
            .child(
                Label::new(title)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            )
            .into_any_element(),
    );

    for (index, label) in form.labels().iter().enumerate() {
        let active = index == form.active;
        let value = form.fields.get(index).cloned().unwrap_or_default();
        let empty = value.is_empty();
        let placeholder = form.placeholder(index);
        let display = if empty {
            match placeholder {
                Some(placeholder) if active => format!("{placeholder}|"),
                Some(placeholder) => placeholder.to_string(),
                None if active => "|".to_string(),
                None => "—".to_string(),
            }
        } else if active {
            format!("{value}|")
        } else {
            value
        };
        let value_label = Label::new(display)
            .size(LabelSize::Custom(PANEL_TEXT_REMS))
            .truncate();
        let value_label = if empty && placeholder.is_some() {
            value_label.color(Color::Placeholder)
        } else if active {
            value_label.color(Color::Accent).underline()
        } else if empty {
            value_label.color(Color::Placeholder)
        } else {
            value_label
        };
        rows.push(
            h_flex()
                .px_3()
                .py_1()
                .gap_2()
                .child(
                    Label::new(format!("{label}:"))
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted)
                        .flex_none(),
                )
                .child(value_label.flex_1())
                .into_any_element(),
        );
    }

    let footer = match form.kind {
        FormKind::Sql => "Enter run · Esc cancel",
    };
    rows.push(
        h_flex()
            .px_3()
            .py_1()
            .child(
                Label::new(footer)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            )
            .into_any_element(),
    );
    if let Some(message) = &form.message {
        rows.push(
            h_flex()
                .px_3()
                .py_1()
                .child(
                    Label::new(message.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Error),
                )
                .into_any_element(),
        );
    }

    v_flex().children(rows).into_any_element()
}

fn remote_protocol_badge(protocol: &str, cx: &App) -> gpui::AnyElement {
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .bg(cx.theme().colors().element_hover)
        .child(
            Label::new(protocol.to_string())
                .size(LabelSize::Custom(Rems(0.625)))
                .color(Color::Muted),
        )
        .into_any_element()
}

fn remote_root_badge(server: &Option<RemoteServer>) -> String {
    server
        .as_ref()
        .and_then(|s| s.protocol.clone())
        .filter(|protocol| !protocol.is_empty())
        .unwrap_or_else(|| "server".to_string())
}

fn driver_badge(chip: &str, color: Color, cx: &App) -> gpui::AnyElement {
    dialog_badge(chip, color, cx)
}

fn empty_row(message: &'static str) -> gpui::AnyElement {
    h_flex()
        .px_3()
        .py_1()
        .child(
            Label::new(message)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(Color::Muted),
        )
        .into_any_element()
}

fn empty_state(
    icon: IconName,
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    _cx: &App,
) -> gpui::AnyElement {
    let title: SharedString = title.into();
    let hint: SharedString = hint.into();
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .gap_1()
        .py_8()
        .px_3()
        .child(Icon::new(icon).size(IconSize::XLarge).color(Color::Muted))
        .child(
            Label::new(title)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(Color::Default),
        )
        .when(!hint.is_empty(), |this| {
            this.child(
                Label::new(hint)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            )
        })
        .into_any_element()
}

pub struct DeployServerIndicator {
    _marker: (),
}

impl DeployServerIndicator {
    fn new(_cx: &mut gpui::Context<Self>) -> Self {
        Self { _marker: () }
    }
}

impl Render for DeployServerIndicator {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_global::<DeployShared>() {
            return div().into_any_element();
        }
        let shared = cx.global::<DeployShared>();
        if shared.servers.is_empty() {
            return div().into_any_element();
        }
        let active = shared
            .active
            .clone()
            .or_else(|| shared.servers.first().cloned());

        let trigger = Button::new(
            "deploy-server-indicator",
            active.unwrap_or_else(|| "no server".to_string()),
        )
        .size(ButtonSize::Compact)
        .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
        .tooltip(Tooltip::text("Deployment server"));

        PopoverMenu::new("deploy-server-menu")
            .menu(|window, cx| {
                let shared = if cx.has_global::<DeployShared>() {
                    cx.global::<DeployShared>()
                } else {
                    return None;
                };
                let servers = shared.servers.clone();
                let active = shared.active.clone();
                let panel = shared.panel.clone();
                let root = shared.root.clone();
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    for server in servers.iter() {
                        let is_active = active.as_deref() == Some(server.as_str());
                        let server_name = server.clone();
                        let menu_panel = panel.clone();
                        let entry = ui::ContextMenuEntry::new(server.clone())
                            .toggleable(ui::IconPosition::Start, is_active)
                            .handler(move |_window: &mut Window, cx: &mut App| {
                                if let Some(panel) =
                                    menu_panel.clone().and_then(|panel| panel.upgrade())
                                {
                                    panel.update(cx, |panel, cx| {
                                        panel.set_active_server_from_status_bar(
                                            Some(server_name.clone()),
                                            cx,
                                        );
                                    });
                                }
                            });
                        menu = menu.item(entry);
                    }
                    menu = menu.separator();
                    let clear_panel = panel.clone();
                    menu = menu.entry("<no default server>", None, move |_window, cx| {
                        if let Some(panel) = clear_panel.clone().and_then(|panel| panel.upgrade()) {
                            panel.update(cx, |panel, cx| {
                                let root = panel.worktree_root(cx);
                                let servers = panel
                                    .remote_servers
                                    .iter()
                                    .filter(|server| server.visible)
                                    .cloned()
                                    .collect::<Vec<_>>();
                                if let Some(root) = root {
                                    let _ = save_deploy_config(&root, &servers, None);
                                }
                                panel.set_active_server_from_status_bar(None, cx);
                            });
                        }
                    });
                    let _ = root;
                    menu
                }))
            })
            .anchor(Anchor::BottomRight)
            .trigger(trigger)
            .into_any_element()
    }
}

impl StatusItemView for DeployServerIndicator {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn workspace::ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _cx: &App) -> Option<workspace::HideStatusItem> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("php_panel_{name}_{unique}"));
        std::fs::create_dir_all(&root).expect("create temporary project root");
        root
    }

    fn fields(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn test_openssh_host(
        alias: &str,
        host_name: &str,
        user: Option<&str>,
        port: Option<&str>,
        identity_file: Option<&str>,
    ) -> OpenSshHost {
        OpenSshHost {
            alias: alias.to_string(),
            host_name: host_name.to_string(),
            user: user.map(str::to_string),
            port: port.map(str::to_string),
            identity_file: identity_file.map(str::to_string),
            proxy_jump: None,
            proxy_command: None,
        }
    }

    #[test]
    fn form_defaults_match_field_labels() {
        let form = FormState::sql("app".to_string());
        assert_eq!(form.kind, FormKind::Sql);
        assert_eq!(form.fields.len(), form.labels().len());
        assert_eq!(form.active, 0);
        assert_eq!(form.labels(), &["sql"]);
        assert_eq!(form.placeholder(0), Some("select * from users limit 20"));
        assert_eq!(form.connection.as_deref(), Some("app"));
    }

    #[test]
    fn form_key_sequence_matches_typing_model() {
        let mut sql = FormState::sql("app".to_string());
        assert_eq!(sql.fields.len(), 1);

        for character in "my query".chars() {
            let character = character.to_string();
            assert_eq!(sql.handle_key("", Some(&character)), FormKeyOutcome::Continue);
        }
        assert_eq!(sql.fields[0], "my query");
        sql.handle_key("space", None);
        assert_eq!(sql.fields[0], "my query ");
        sql.handle_key("backspace", None);
        assert_eq!(sql.fields[0], "my query");
        sql.handle_key("tab", None);
        assert_eq!(sql.active, 0);
        assert_eq!(sql.handle_key("enter", None), FormKeyOutcome::Save);
        assert_eq!(sql.handle_key("escape", None), FormKeyOutcome::Cancel);
    }

    #[test]
    fn save_server_creates_config_with_numeric_port() {
        let root = temp_root("server_create");
        let name = save_server_config(
            &root,
            &fields(&[
                "dev",
                "sftp",
                "example.test",
                "2222",
                "deploy",
                "/var/www",
                ".",
            ]),
        )
        .expect("save server");
        assert_eq!(name, "dev");

        let path = root.join(".zed/deploy.json");
        let contents = std::fs::read_to_string(&path).expect("read deploy.json");
        assert!(contents.starts_with("{\n  "));
        assert!(contents.ends_with("}\n"));
        let config = read_json(&path).expect("parse deploy.json");
        let server = &config["servers"][0];
        assert_eq!(server["name"], json!("dev"));
        assert_eq!(server["protocol"], json!("sftp"));
        assert_eq!(server["host"], json!("example.test"));
        assert_eq!(server["port"], json!(2222));
        assert_eq!(server["user"], json!("deploy"));
        assert_eq!(server["remote_root"], json!("/var/www"));
        assert_eq!(server["local_root"], json!("."));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn save_server_upserts_and_preserves_other_fields() {
        let root = temp_root("server_upsert");
        let zed = root.join(".zed");
        std::fs::create_dir_all(&zed).expect("create .zed");
        std::fs::write(
            zed.join("deploy.json"),
            r#"{
  "default": "local",
  "servers": [
    {
      "name": "local",
      "protocol": "local",
      "remote_root": "/tmp/old",
      "exclude": ["vendor/"],
      "upload_on_save": true
    },
    { "name": "staging", "protocol": "sftp", "host": "127.0.0.1", "remote_root": "/srv" }
  ]
}"#,
        )
        .expect("seed deploy.json");

        save_server_config(
            &root,
            &fields(&["local", "local", "", "", "", "/tmp/new", "."]),
        )
        .expect("update local");

        let config = read_json(&zed.join("deploy.json")).expect("parse deploy.json");
        let servers = config["servers"].as_array().expect("servers array");
        assert_eq!(servers.len(), 2);
        let local = &servers[0];
        assert_eq!(local["remote_root"], json!("/tmp/new"));
        assert_eq!(local["exclude"], json!(["vendor/"]));
        assert_eq!(local["upload_on_save"], json!(true));
        assert!(local.get("host").is_none());
        assert!(local.get("port").is_none());
        assert_eq!(servers[1]["name"], json!("staging"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn active_server_resolution_prefers_active_then_default_then_first() {
        let names = vec!["local".to_string(), "sftp".to_string()];
        assert_eq!(
            resolve_active_server(Some("sftp".to_string()), Some("local".to_string()), &names),
            Some("sftp".to_string())
        );
        assert_eq!(
            resolve_active_server(
                Some("missing".to_string()),
                Some("sftp".to_string()),
                &names
            ),
            Some("sftp".to_string())
        );
        assert_eq!(
            resolve_active_server(None, None, &names),
            Some("local".to_string())
        );
        assert_eq!(resolve_active_server(None, None, &[]), None);
    }

    #[test]
    fn persist_active_server_preserves_other_fields() {
        let root = temp_root("server_active");
        let zed = root.join(".zed");
        std::fs::create_dir_all(&zed).expect("create .zed");
        std::fs::write(
            zed.join("deploy.json"),
            r#"{"default":"local","servers":[{"name":"local","protocol":"local","remote_root":"remote"},{"name":"sftp","protocol":"sftp","host":"127.0.0.1","remote_root":"/var/www/app"}],"extra":{"keep":true}}"#,
        )
        .expect("seed deploy.json");

        persist_active_server(
            &root,
            &[
                RemoteServer {
                    name: "local".to_string(),
                    protocol: Some("local".to_string()),
                    host: None,
                    port: None,
                    user: None,
                    password: None,
                    password_session: false,
                    ssh_config: None,
                    remote_root: Some("remote".to_string()),
                    local_root: None,
                    web_url: None,
                    excluded: Vec::new(),
                    upload_on_save: false,
                    use_rsync: false,
                    use_sudo: false,
                    visible: true,
                    passive_mode: true,
                    show_hidden: false,
                    list_compat: false,
                    use_mlsd: false,
                    max_connections: ADVANCED_DEFAULT_MAX_CONNECTIONS,
                    keep_alive_enabled: true,
                    keep_alive_seconds: ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS,
                    encoding: ADVANCED_DEFAULT_ENCODING.to_string(),
                    root: root.clone(),
                },
                RemoteServer {
                    name: "sftp".to_string(),
                    protocol: Some("sftp".to_string()),
                    host: Some("127.0.0.1".to_string()),
                    port: Some("2222".to_string()),
                    user: Some("deploy".to_string()),
                    password: None,
                    password_session: false,
                    ssh_config: None,
                    remote_root: Some("/var/www/app".to_string()),
                    local_root: Some(".".to_string()),
                    web_url: None,
                    excluded: Vec::new(),
                    upload_on_save: false,
                    use_rsync: false,
                    use_sudo: false,
                    visible: true,
                    passive_mode: true,
                    show_hidden: false,
                    list_compat: false,
                    use_mlsd: false,
                    max_connections: ADVANCED_DEFAULT_MAX_CONNECTIONS,
                    keep_alive_enabled: true,
                    keep_alive_seconds: ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS,
                    encoding: ADVANCED_DEFAULT_ENCODING.to_string(),
                    root: root.clone(),
                },
            ],
            "sftp",
        )
        .expect("persist active server");

        let contents = std::fs::read_to_string(zed.join("deploy.json")).expect("read config");
        assert!(contents.starts_with("{\n  "));
        assert!(contents.ends_with("}\n"));
        let config = read_json(&zed.join("deploy.json")).expect("parse config");
        assert_eq!(config["active"], json!("sftp"));
        assert_eq!(config["default"], json!("local"));
        assert_eq!(config["extra"], json!({"keep": true}));
        assert_eq!(config["servers"].as_array().expect("servers").len(), 2);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn save_server_validates_without_writing() {
        let root = temp_root("server_validate");
        let path = root.join(".zed/deploy.json");

        let error = save_server_config(&root, &fields(&["", "local", "", "", "", "/tmp", "."]))
            .expect_err("empty name rejected");
        assert_eq!(error, "name is required");
        let error = save_server_config(&root, &fields(&["dev", "ftp", "", "", "", "/tmp", "."]))
            .expect_err("unknown protocol rejected");
        assert_eq!(error, "protocol must be local or sftp");
        let error = save_server_config(
            &root,
            &fields(&["dev", "sftp", "host", "not-a-port", "", "/tmp", "."]),
        )
        .expect_err("non numeric port rejected");
        assert_eq!(error, "port must be a number");
        assert!(!path.exists());

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn parse_test_list_reads_files() {
        let files = parse_test_files(
            r#"{"files":["tests/CalculatorTest.php","tests/FailingTest.php"],"tool":"phpunit"}"#,
        )
        .expect("parse test list");
        assert_eq!(
            files,
            vec!["tests/CalculatorTest.php", "tests/FailingTest.php"]
        );
    }

    #[test]
    fn parse_test_run_reads_statuses_and_summary() {
        let output = r#"{"tool":"phpunit","tests":[{"name":"testAddsNumbers","class":"App\\Tests\\CalculatorTest","file":"tests/CalculatorTest.php","status":"pass","message":""},{"name":"testFailsOnPurpose","class":"App\\Tests\\FailingTest","file":"tests/FailingTest.php","status":"fail","message":"Failed asserting that 5 is identical to 4."},{"name":"testIsSkipped","class":"App\\Tests\\FailingTest","file":"tests/FailingTest.php","status":"skip","message":""}],"summary":{"total":3,"passed":1,"failed":1,"errors":0,"skipped":1}}"#;
        let result = parse_test_run(output).expect("parse test run");
        assert_eq!(result.tests.len(), 3);
        assert_eq!(result.tests[0].status, TestStatus::Pass);
        assert_eq!(result.tests[1].status, TestStatus::Fail);
        assert_eq!(
            result.tests[1].message,
            "Failed asserting that 5 is identical to 4."
        );
        assert_eq!(result.tests[2].status, TestStatus::Skip);
        assert_eq!(result.summary.total, 3);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.summary.text(), "1 passed · 1 failed · 1 skipped");
    }

    #[test]
    fn parse_deploy_server_reads_new_fields() {
        let root = Path::new("/tmp");
        let numeric = r#"{"name":"sftp","protocol":"sftp","host":"127.0.0.1","port":2222,"user":"deploy","remote_root":"/var/www/app","local_root":".","web_url":"https://app.test","exclude":["vendor/","node_modules/"],"upload_on_save":true}"#;
        let value: Value = serde_json::from_str(numeric).expect("parse config");
        let server = parse_deploy_server(&value, root).expect("parse server");
        assert_eq!(server.port.as_deref(), Some("2222"));
        assert_eq!(server.local_root.as_deref(), Some("."));
        assert_eq!(server.web_url.as_deref(), Some("https://app.test"));
        assert_eq!(
            server.excluded,
            vec!["vendor/".to_string(), "node_modules/".to_string()]
        );
        assert!(server.upload_on_save);

        let string = r#"{"name":"stage","port":"2200"}"#;
        let value: Value = serde_json::from_str(string).expect("parse config");
        let server = parse_deploy_server(&value, root).expect("parse server");
        assert_eq!(server.port.as_deref(), Some("2200"));

        let alias = r#"{"name":"alias","excluded":["vendor/","tmp/"]}"#;
        let value: Value = serde_json::from_str(alias).expect("parse config");
        let server = parse_deploy_server(&value, root).expect("parse server");
        assert_eq!(
            server.excluded,
            vec!["vendor/".to_string(), "tmp/".to_string()]
        );
    }

    #[test]
    fn save_deploy_config_round_trips_servers_and_preserves_other_keys() {
        let root = temp_root("deploy_round_trip");
        let zed = root.join(".zed");
        std::fs::create_dir_all(&zed).expect("create .zed");
        std::fs::write(
            zed.join("deploy.json"),
            r#"{
  "default": "local",
  "active": "sftp",
  "extra": {"keep": true},
  "servers": [
    {
      "name": "sftp",
      "protocol": "sftp",
      "host": "127.0.0.1",
      "port": 2222,
      "user": "deploy",
      "remote_root": "/var/www/app",
      "local_root": ".",
      "web_url": "https://app.test",
      "exclude": ["vendor/", "node_modules/"]
    },
    {
      "name": "local",
      "protocol": "local",
      "local_root": ".",
      "remote_root": "remote",
      "exclude": []
    }
  ]
}"#,
        )
        .expect("seed deploy.json");

        let first = read_json(&zed.join("deploy.json")).expect("read config");
        let servers = first["servers"]
            .as_array()
            .expect("servers array")
            .iter()
            .filter_map(|server| parse_deploy_server(server, &root))
            .collect::<Vec<_>>();

        save_deploy_config(&root, &servers, Some("sftp")).expect("save config");

        let second = read_json(&zed.join("deploy.json")).expect("read config");
        let reparsed = second["servers"]
            .as_array()
            .expect("servers array")
            .iter()
            .filter_map(|server| parse_deploy_server(server, &root))
            .collect::<Vec<_>>();
        assert_eq!(reparsed.len(), 2);

        let sftp = reparsed
            .iter()
            .find(|server| server.name == "sftp")
            .expect("sftp preserved");
        assert_eq!(sftp.protocol.as_deref(), Some("sftp"));
        assert_eq!(sftp.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(sftp.port.as_deref(), Some("2222"));
        assert_eq!(sftp.user.as_deref(), Some("deploy"));
        assert_eq!(sftp.remote_root.as_deref(), Some("/var/www/app"));
        assert_eq!(sftp.local_root.as_deref(), Some("."));
        assert_eq!(sftp.web_url.as_deref(), Some("https://app.test"));
        assert_eq!(
            sftp.excluded,
            vec!["vendor/".to_string(), "node_modules/".to_string()]
        );

        let local = reparsed
            .iter()
            .find(|server| server.name == "local")
            .expect("local preserved");
        assert_eq!(local.protocol.as_deref(), Some("local"));
        assert!(local.excluded.is_empty());

        assert_eq!(second["active"], json!("sftp"));
        assert_eq!(second["default"], json!("local"));
        assert_eq!(second["extra"], json!({"keep": true}));

        save_deploy_config(&root, &reparsed, None).expect("clear active");
        let cleared = read_json(&zed.join("deploy.json")).expect("read config");
        assert!(cleared.get("active").is_none());
        assert_eq!(cleared["default"], json!("local"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn dialog_staging_converts_fields_into_servers() {
        let root = temp_root("deploy_staging");

        let initial = vec![blank_server(root.clone())];
        let buffer = DialogBuffer::load(initial.first());
        assert!(buffer.name.is_empty());

        let mut buffer = DialogBuffer {
            name: "web1".to_string(),
            host: "127.0.0.1".to_string(),
            port: "2222".to_string(),
            user: "deploy".to_string(),
            remote_root: "/tmp/web1".to_string(),
            excluded: String::new(),
            ..DialogBuffer::default()
        };
        let mut servers = vec![blank_server(root.clone())];
        buffer.apply(servers.get_mut(0).expect("server"), "local");
        let staged = servers.first().cloned().expect("staged server");
        assert_eq!(staged.name, "web1");
        assert_eq!(staged.protocol.as_deref(), Some("local"));
        assert_eq!(staged.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(staged.port.as_deref(), Some("2222"));
        assert_eq!(staged.user.as_deref(), Some("deploy"));
        assert_eq!(staged.remote_root.as_deref(), Some("/tmp/web1"));
        assert!(staged.excluded.is_empty());

        // mapping edits staged afterwards keep the connection fields
        buffer.local_root = ".".to_string();
        buffer.web_url = "http://localhost:8000".to_string();
        buffer.apply(servers.get_mut(0).expect("server"), "local");
        let staged = servers.first().cloned().expect("staged server");
        assert_eq!(staged.local_root.as_deref(), Some("."));
        assert_eq!(staged.remote_root.as_deref(), Some("/tmp/web1"));
        assert_eq!(staged.web_url.as_deref(), Some("http://localhost:8000"));
        assert_eq!(staged.protocol.as_deref(), Some("local"));

        // excluded paths staged through the buffer
        buffer.excluded = "vendor/, dist/".to_string();
        buffer.apply(servers.get_mut(0).expect("server"), "local");
        let staged = servers.first().cloned().expect("staged server");
        assert_eq!(
            staged.excluded,
            vec!["vendor/".to_string(), "dist/".to_string()]
        );

        // + appends a blank local server; - removes it again
        let mut next_number = dialog_next_number(&servers);
        dialog_add_server(&mut servers, &root, &mut next_number);
        assert_eq!(servers.len(), 2);
        assert_eq!(
            servers.last().map(|server| server.protocol.clone()),
            Some(Some("local".to_string()))
        );
        servers.remove(1);
        assert_eq!(servers.len(), 1);

        // round trip: loading a staged server back into a fresh buffer preserves it
        let mut server = servers.first().cloned().expect("server");
        let mut reloaded = DialogBuffer::load(Some(&server));
        reloaded.apply(&mut server, "sftp");
        assert_eq!(server.name, "web1");
        assert_eq!(server.port.as_deref(), Some("2222"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn excluded_paths_convert_between_text_and_list() {
        assert_eq!(
            parse_excluded("vendor/, node_modules/ ,tmp"),
            vec![
                "vendor/".to_string(),
                "node_modules/".to_string(),
                "tmp".to_string()
            ]
        );
        assert_eq!(parse_excluded(""), Vec::<String>::new());
        assert_eq!(excluded_text(Some(&parse_excluded("a,b"))), "a, b");
        assert_eq!(excluded_text(None), "");

        let mut server = blank_server(PathBuf::from("/tmp"));
        server.excluded = vec!["vendor/".to_string()];
        assert_eq!(excluded_text(Some(server.excluded.as_slice())), "vendor/");
        apply_excluded_field(&mut server, &fields(&["src/, dist/"]));
        assert_eq!(
            server.excluded,
            vec!["src/".to_string(), "dist/".to_string()]
        );
        apply_excluded_field(&mut server, &fields(&[""]));
        assert_eq!(server.excluded, Vec::<String>::new());
    }

    #[test]
    fn parse_remote_entries_builds_relative_paths() {
        let root = r#"{"server":"local","path":".","entries":[{"name":"subdir","type":"dir","size":96},{"name":"README.md","type":"file","size":66}]}"#;
        let entries = parse_remote_entries(root, ".").expect("parse root listing");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "subdir");
        assert!(entries[0].is_directory);
        assert_eq!(entries[0].size, Some(96));
        assert!(!entries[1].is_directory);

        let nested = r#"{"server":"local","path":"subdir","entries":[{"name":"nested.php","type":"file","size":23}]}"#;
        let entries = parse_remote_entries(nested, "subdir").expect("parse nested listing");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "subdir/nested.php");
        assert_eq!(entries[0].size, Some(23));
    }

    fn connection_entry(name: &str) -> DatabaseEntry {
        DatabaseEntry {
            name: name.to_string(),
            driver: None,
            target: None,
            user: None,
            host: None,
            root: PathBuf::from("/tmp"),
            read_only: false,
        }
    }

    #[test]
    fn database_connection_label_prefers_sqlite_file_name() {
        let mut entry = connection_entry("app");
        entry.driver = Some("sqlite".to_string());
        entry.target = Some("data/demo.sqlite".to_string());
        assert_eq!(database_connection_label(&entry), "demo.sqlite");

        let mut pg = connection_entry("pg");
        pg.driver = Some("postgres".to_string());
        pg.target = Some("postgres://app@localhost/app".to_string());
        assert_eq!(database_connection_label(&pg), "pg");
    }

    #[test]
    fn driver_badge_mapping_covers_all_drivers() {
        assert_eq!(driver_badge_short("postgres"), "pg");
        assert_eq!(driver_badge_short("mysql"), "my");
        assert_eq!(driver_badge_short("mariadb"), "mar");
        assert_eq!(driver_badge_short("sqlite"), "lite");
        assert_eq!(driver_badge_short("sqlserver"), "msr");
        assert_eq!(driver_badge_short("oracle"), "orc");
        assert_eq!(driver_badge_short("mongodb"), "mdb");
        assert_eq!(driver_badge_short("redis"), "sh");
        assert_eq!(driver_badge_short("unknown"), "?");
        for driver in DATABASE_DRIVERS {
            assert_ne!(driver_badge_short(driver), "?");
            assert!(!driver_display_name(driver).is_empty());
        }
        assert_eq!(driver_display_name("sqlserver"), "SQL Server");
        assert_eq!(driver_display_name("custom"), "custom");
    }

    #[test]
    fn parse_database_names_reads_list_json() {
        let names = parse_database_names(
            r#"{"connection":"pg","driver":"postgres","databases":["app","postgres"]}"#,
        )
        .expect("parse names");
        assert_eq!(names, vec!["app", "postgres"]);
        assert!(parse_database_names("{}").is_err());
        assert!(parse_database_names("not json").is_err());
    }

    #[test]
    fn parse_database_views_reads_table_shaped_json() {
        let views = parse_database_views(
            r#"{"connection":"app","driver":"sqlite","views":[{"name":"active_orders","columns":[{"name":"id","type":"INTEGER","primary_key":true}]}]}"#,
        )
        .expect("parse views");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "active_orders");
        assert_eq!(views[0].columns.len(), 1);
        assert!(views[0].columns[0].primary_key);
        assert!(parse_database_views(r#"{"views":[]}"#).is_ok());
    }

    #[test]
    fn http_query_rows_round_trip_through_url() {
        let url = "https://example.test/search?q=hello%20world&tag=a%26b&empty=";
        let (base, rows) = parse_url_query(url);
        assert_eq!(base, "https://example.test/search");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].key, "q");
        assert_eq!(rows[0].value, "hello world");
        assert_eq!(rows[1].value, "a&b");
        let rebuilt = serialize_url_query(&base, &rows);
        let (base2, rows2) = parse_url_query(&rebuilt);
        assert_eq!(base, base2);
        assert_eq!(rows, rows2);
    }

    #[test]
    fn http_query_serialization_skips_disabled_and_empty_keys() {
        let rows = vec![
            KeyValueRow::new("a", "1"),
            KeyValueRow {
                enabled: false,
                key: "b".to_string(),
                value: "2".to_string(),
            },
            KeyValueRow::new("", "ignored"),
        ];
        assert_eq!(
            serialize_url_query("https://x.test/path", &rows),
            "https://x.test/path?a=1"
        );
        assert_eq!(serialize_url_query("https://x.test/path", &[]), "https://x.test/path");
    }

    #[test]
    fn http_auth_maps_to_headers_and_query() {
        let basic = HttpAuthSettings {
            kind: HttpAuthType::Basic,
            username: "user".to_string(),
            password: "pass".to_string(),
            ..HttpAuthSettings::default()
        };
        assert_eq!(
            auth_headers(&basic),
            vec![(
                "Authorization".to_string(),
                "Basic dXNlcjpwYXNz".to_string()
            )]
        );
        let bearer = HttpAuthSettings {
            kind: HttpAuthType::Bearer,
            token: "tok".to_string(),
            ..HttpAuthSettings::default()
        };
        assert_eq!(
            auth_headers(&bearer),
            vec![("Authorization".to_string(), "Bearer tok".to_string())]
        );
        let api_key_header = HttpAuthSettings {
            kind: HttpAuthType::ApiKey,
            api_key_name: "X-Api-Key".to_string(),
            api_key_value: "secret".to_string(),
            api_key_in: ApiKeyLocation::Header,
            ..HttpAuthSettings::default()
        };
        assert_eq!(
            auth_headers(&api_key_header),
            vec![("X-Api-Key".to_string(), "secret".to_string())]
        );
        let api_key_query = HttpAuthSettings {
            api_key_in: ApiKeyLocation::Query,
            ..api_key_header
        };
        assert_eq!(
            auth_query(&api_key_query),
            Some(("X-Api-Key".to_string(), "secret".to_string()))
        );
        assert!(auth_headers(&api_key_query).is_empty());
    }

    #[test]
    fn http_body_modes_serialize() {
        let fields = vec![KeyValueRow::new("a b", "1+2"), KeyValueRow::new("x", "y")];
        assert_eq!(encode_urlencoded(&fields), "a%20b=1%2B2&x=y");
        let multipart = build_multipart(&fields, "BOUNDARY");
        assert!(multipart.contains("Content-Disposition: form-data; name=\"a b\""));
        assert!(multipart.contains("--BOUNDARY--\r\n"));
        let raw = compose_http_request(
            "https://x.test",
            &[],
            &HttpAuthSettings::default(),
            HttpBodyKind::Raw,
            RawContentType::Json,
            "{\"a\":1}",
            &[],
            "B",
        );
        assert_eq!(raw.body.as_deref(), Some("{\"a\":1}"));
        assert!(raw
            .headers
            .contains(&("Content-Type".to_string(), "application/json".to_string())));
        let none = compose_http_request(
            "https://x.test",
            &[],
            &HttpAuthSettings::default(),
            HttpBodyKind::None,
            RawContentType::Json,
            "ignored",
            &[],
            "B",
        );
        assert!(none.body.is_none());
    }

    #[test]
    fn http_compose_applies_api_key_query_and_keeps_explicit_content_type() {
        let auth = HttpAuthSettings {
            kind: HttpAuthType::ApiKey,
            api_key_name: "key".to_string(),
            api_key_value: "v".to_string(),
            api_key_in: ApiKeyLocation::Query,
            ..HttpAuthSettings::default()
        };
        let headers = vec![KeyValueRow::new("Content-Type", "text/custom")];
        let composed = compose_http_request(
            "https://x.test/p?a=1",
            &headers,
            &auth,
            HttpBodyKind::Raw,
            RawContentType::Json,
            "{}",
            &[],
            "B",
        );
        assert_eq!(composed.url, "https://x.test/p?a=1&key=v");
        assert_eq!(
            composed
                .headers
                .iter()
                .filter(|(name, _)| name == "Content-Type")
                .count(),
            1
        );
    }

    #[test]
    fn http_settings_round_trip_through_json() {
        let mut file = HttpSettingsFile::default();
        let mut settings = HttpRequestSettings {
            method: "POST".to_string(),
            url: "https://x.test?a=1".to_string(),
            subtab: Some(HttpSubTab::Body),
            params: vec![KeyValueRow::new("a", "1")],
            auth: HttpAuthSettings {
                kind: HttpAuthType::Bearer,
                token: "t".to_string(),
                ..HttpAuthSettings::default()
            },
            headers: vec![KeyValueRow::new("Accept", "application/json")],
            body_kind: HttpBodyKind::Raw,
            raw_content_type: RawContentType::Json,
            raw_body: "{\"a\":1}".to_string(),
            body_fields: vec![KeyValueRow::new("f", "v")],
            follow_redirects: true,
            timeout_secs: 12,
            verify_tls: false,
        };
        file.requests.insert(
            http_settings_key("requests.http", "Create"),
            settings.clone(),
        );
        let json = serde_json::to_string(&file).expect("serialize settings");
        let decoded: HttpSettingsFile = serde_json::from_str(&json).expect("deserialize settings");
        assert_eq!(decoded.requests.len(), 1);
        assert_eq!(
            decoded
                .requests
                .get(&http_settings_key("requests.http", "Create")),
            Some(&settings)
        );
        assert_eq!(settings.subtab, Some(HttpSubTab::Body));
        assert!(!settings.verify_tls);
    }

    #[test]
    fn http_settings_save_and_load_round_trip() {
        let root = temp_root("http_settings_round_trip");
        let mut file = HttpSettingsFile::default();
        file.requests.insert(
            http_settings_key("a.http", "R"),
            HttpRequestSettings {
                url: "http://127.0.0.1/".to_string(),
                ..HttpRequestSettings::default()
            },
        );
        save_http_settings(&root, &file).expect("save settings");
        let loaded = load_http_settings(&root);
        assert_eq!(loaded, file);
        std::fs::write(http_settings_path(&root), "not json").expect("break settings");
        assert!(load_http_settings(&root).requests.is_empty());
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn http_response_status_and_pretty_body() {
        assert!(matches!(response_status_color(204), Color::Success));
        assert!(matches!(response_status_color(301), Color::Accent));
        assert!(matches!(response_status_color(404), Color::Warning));
        assert!(matches!(response_status_color(500), Color::Error));
        assert_eq!(
            pretty_body("{\"b\":2,\"a\":1}", Some("application/json")),
            "{\n  \"b\": 2,\n  \"a\": 1\n}"
        );
        assert_eq!(pretty_body("plain", Some("text/plain")), "plain");
    }

    #[test]
    fn http_request_detail_parses_headers_and_body() {
        let contents = "\
### Create
POST https://example.test/users
Content-Type: application/json
X-Test: 1

{\"name\":\"Ada\"}
";
        let detail = parse_http_request_detail(contents, "Create").expect("detail");
        assert_eq!(detail.method, "POST");
        assert_eq!(detail.url, "https://example.test/users");
        assert_eq!(
            detail.headers,
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                ("X-Test".to_string(), "1".to_string())
            ]
        );
        assert_eq!(detail.body, "{\"name\":\"Ada\"}");
        assert!(parse_http_request_detail(contents, "Missing").is_none());
    }

    #[test]
    fn first_tab_is_remote_and_cycling_wraps() {
        assert_eq!(PanelTab::VISIBLE.first(), Some(&PanelTab::Remote));
        assert_eq!(PanelTab::VISIBLE.len(), 3);
        assert!(!PanelTab::VISIBLE.contains(&PanelTab::Tests));
        assert!(!PanelTab::VISIBLE.contains(&PanelTab::Collections));
        assert_eq!(next_tab(PanelTab::Remote, 1), PanelTab::Databases);
        assert_eq!(next_tab(PanelTab::Remote, -1), PanelTab::Http);
        assert_eq!(next_tab(PanelTab::Http, 1), PanelTab::Remote);
        assert_eq!(next_tab(PanelTab::Databases, -1), PanelTab::Remote);
        for tab in PanelTab::VISIBLE {
            assert_eq!(next_tab(tab, 3), tab);
        }
    }

    #[test]
    fn mask_password_shows_dots_only_when_set() {
        assert_eq!(mask_password(""), "");
        assert_eq!(mask_password("s3cret"), "•••••");
    }

    #[test]
    fn driver_support_hint_covers_all_drivers() {
        for driver in DATABASE_DRIVERS {
            assert!(!driver_support_hint(driver).is_empty());
            assert!(driver_support_hint(driver).contains("supports"));
        }
        assert_eq!(driver_support_hint("unknown"), "");
    }

    #[test]
    fn data_source_general_fields_matrix_matches_drivers() {
        assert_eq!(data_source_general_fields("sqlite"), &[]);
        for driver in ["postgres", "mysql", "mariadb", "sqlserver", "oracle"] {
            assert_eq!(
                data_source_general_fields(driver),
                &[
                    DsGeneralField::Host,
                    DsGeneralField::Port,
                    DsGeneralField::User,
                    DsGeneralField::Password,
                    DsGeneralField::Database,
                ]
            );
        }
        assert_eq!(
            data_source_general_fields("mongodb"),
            &[
                DsGeneralField::Host,
                DsGeneralField::Port,
                DsGeneralField::User,
                DsGeneralField::Password,
                DsGeneralField::Database,
            ]
        );
        assert_eq!(
            data_source_general_fields("redis"),
            &[
                DsGeneralField::Host,
                DsGeneralField::Port,
                DsGeneralField::Password,
            ]
        );
        // unknown drivers keep the server shape (matches the old non-sqlite columns)
        assert!(data_source_general_fields("custom").contains(&DsGeneralField::Host));
    }

    #[test]
    fn driver_default_port_matrix_prefills_only_server_drivers() {
        assert!(driver_default_port("sqlite").is_none());
        assert!(driver_default_port("unknown").is_none());
        assert_eq!(driver_default_port("postgres"), Some("5432"));
        assert_eq!(driver_default_port("mysql"), Some("3306"));
        assert_eq!(driver_default_port("mariadb"), Some("3306"));
        assert_eq!(driver_default_port("sqlserver"), Some("1433"));
        assert_eq!(driver_default_port("oracle"), Some("1521"));
        assert_eq!(driver_default_port("mongodb"), Some("27017"));
        assert_eq!(driver_default_port("redis"), Some("6379"));
    }

    #[test]
    fn ssh_store_round_trip_tolerates_missing_and_broken_file() {
        let root = temp_root("ssh_store_round_trip");
        assert!(load_ssh_configs(&root).is_empty());

        let mut agent = SshEntry::blank();
        agent.name = "agent".to_string();
        agent.user = "deploy".to_string();
        agent.host = "127.0.0.1".to_string();
        agent.port = "2222".to_string();
        agent.auth = SshAuthMode::Agent;
        agent.keep_alive = 10;
        agent.strict_host_key = true;
        let mut key = SshEntry::blank();
        key.name = "key".to_string();
        key.user = "ci".to_string();
        key.host = "box.local".to_string();
        key.auth = SshAuthMode::Key;
        key.key_path = "/home/ci/id_ed25519".to_string();

        save_ssh_configs(&root, &[agent, key]).expect("save ssh configs");
        let configs = load_ssh_configs(&root);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].name, "agent");
        assert_eq!(configs[0].port, "2222");
        assert_eq!(configs[0].auth, SshAuthMode::Agent);
        assert_eq!(configs[0].keep_alive, 10);
        assert!(configs[0].strict_host_key);
        assert_eq!(configs[1].auth, SshAuthMode::Key);
        assert_eq!(configs[1].key_path, "/home/ci/id_ed25519");
        assert_eq!(configs[1].keep_alive, 15);

        std::fs::write(root.join(".zed/ssh.json"), "not json").expect("break ssh.json");
        assert!(load_ssh_configs(&root).is_empty());
        std::fs::write(
            root.join(".zed/ssh.json"),
            r#"{"configs":[{"name":"legacy"}]}"#,
        )
        .expect("legacy ssh.json");
        let configs = load_ssh_configs(&root);
        assert_eq!(configs.len(), 1);
        assert_eq!(ssh_entry_port(&configs[0]), 22);
        assert_eq!(configs[0].auth, SshAuthMode::Password);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn save_ssh_configs_rejects_empty_and_duplicate_names() {
        let root = temp_root("ssh_store_validation");
        let mut blank = SshEntry::blank();
        blank.name = "  ".to_string();
        assert_eq!(
            save_ssh_configs(&root, &[blank]),
            Err("configuration name is required".to_string())
        );

        let mut first = SshEntry::blank();
        first.name = "same".to_string();
        let mut second = SshEntry::blank();
        second.name = "same".to_string();
        assert_eq!(
            save_ssh_configs(&root, &[first, second]),
            Err("duplicate configuration name: same".to_string())
        );

        let mut key = SshEntry::blank();
        key.name = "key".to_string();
        key.auth = SshAuthMode::Key;
        assert_eq!(
            save_ssh_configs(&root, &[key]),
            Err("key: key path is required".to_string())
        );
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn ssh_auth_menu_rows_are_ordered_with_expected_labels() {
        let labels = SSH_AUTH_OPTIONS.map(|mode| mode.label());
        assert_eq!(
            labels,
            [
                "Password",
                "Key pair",
                "OpenSSH config and authentication agent",
                "OpenSSH config file (host alias)",
            ]
        );
        let keys = SSH_AUTH_OPTIONS.map(|mode| mode.key());
        assert_eq!(keys, ["password", "key", "agent", "config"]);
        assert_eq!(
            SSH_AUTH_OPTIONS.map(|mode| format!("ssh-auth-row-{}", mode.key())),
            [
                "ssh-auth-row-password",
                "ssh-auth-row-key",
                "ssh-auth-row-agent",
                "ssh-auth-row-config",
            ]
        );
        assert_eq!(SshAuthMode::parse("key"), SshAuthMode::Key);
        assert_eq!(SshAuthMode::parse("agent"), SshAuthMode::Agent);
        assert_eq!(SshAuthMode::parse("password"), SshAuthMode::Password);
        assert_eq!(SshAuthMode::parse("config"), SshAuthMode::Config);
        assert_eq!(
            SshAuthMode::parse("OpenSSH config file (host alias)"),
            SshAuthMode::Config
        );
        assert_eq!(SshAuthMode::parse("gibberish"), SshAuthMode::Password);
    }

    #[test]
    fn parse_openssh_config_reads_basic_blocks_in_file_order() {
        let text = "\
Host web1
  HostName 10.1.2.3
  User deploy
  Port 2222
  IdentityFile ~/.ssh/id_test
  ProxyJump bastion

Host db-tunnel
  HostName jump.example.com
  ProxyCommand ssh -W %h:%p proxy

Host *
  User root
";
        let hosts = parse_openssh_config(text);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "web1");
        assert_eq!(hosts[0].host_name, "10.1.2.3");
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].port.as_deref(), Some("2222"));
        assert_eq!(hosts[0].proxy_jump.as_deref(), Some("bastion"));
        assert_eq!(hosts[1].alias, "db-tunnel");
        assert_eq!(hosts[1].host_name, "jump.example.com");
        assert_eq!(
            hosts[1].proxy_command.as_deref(),
            Some("ssh -W %h:%p proxy")
        );
        // `Host *` is a wildcard block and is skipped.
        assert!(hosts.iter().all(|host| host.alias != "*"));
    }

    #[test]
    fn parse_openssh_config_strips_quotes_and_expands_tilde() {
        let home = std::env::var("HOME").expect("HOME is set");
        let text = "\
Host 'web one'
  User \"deploy user\"
  IdentityFile \"~/.ssh/my key\"
";
        let hosts = parse_openssh_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "web one");
        assert_eq!(hosts[0].user.as_deref(), Some("deploy user"));
        assert_eq!(
            hosts[0].identity_file.as_deref(),
            Some(format!("{home}/.ssh/my key").as_str())
        );
        assert_eq!(
            expand_home_path("~/keys/id", Some("/home/ci")),
            "/home/ci/keys/id"
        );
        assert_eq!(expand_home_path("~other/id", Some("/home/ci")), "~other/id");
    }

    #[test]
    fn parse_openssh_config_defaults_hostname_to_alias() {
        let hosts = parse_openssh_config("Host web1\n  User deploy\n");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_name, "web1");
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
    }

    #[test]
    fn parse_openssh_config_skips_wildcard_and_negated_patterns() {
        let text = "\
Host *
  User root

Host web1 !blocked *.example
  User deploy
  Port 2200
";
        let hosts = parse_openssh_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].alias, "web1");
        assert_eq!(hosts[0].user.as_deref(), Some("deploy"));
        assert_eq!(hosts[0].port.as_deref(), Some("2200"));
    }

    #[test]
    fn parse_openssh_config_duplicate_alias_keeps_first_block() {
        let text = "\
Host web1
  HostName first.example.com

Host web1
  HostName second.example.com
";
        let hosts = parse_openssh_config(text);
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].host_name, "first.example.com");
    }

    #[test]
    fn parse_openssh_config_follows_include_globs() {
        let root = temp_root("openssh_include_glob");
        std::fs::create_dir_all(root.join("conf.d")).expect("create conf.d");
        std::fs::write(
            root.join("conf.d/one.conf"),
            "Host included-host\n  HostName 10.9.9.1\n",
        )
        .expect("write one");
        std::fs::write(
            root.join("conf.d/two.conf"),
            "Host included-host2\n  HostName 10.9.9.2\n",
        )
        .expect("write two");
        let config = root.join("config");
        std::fs::write(
            &config,
            "Host web1\n  HostName 10.1.2.3\nInclude conf.d/*.conf\n",
        )
        .expect("write config");

        let hosts = load_openssh_hosts_from(&config);
        let aliases: Vec<&str> = hosts.iter().map(|host| host.alias.as_str()).collect();
        assert_eq!(aliases, vec!["web1", "included-host", "included-host2"]);
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn parse_openssh_config_limits_nested_include_depth() {
        let root = temp_root("openssh_include_depth");
        let config = root.join("config");
        std::fs::write(
            &config,
            "Host top\n  HostName top.example\nInclude a.conf\n",
        )
        .expect("write config");
        std::fs::write(root.join("a.conf"), "Host level-a\nInclude b.conf\n").expect("write a");
        std::fs::write(root.join("b.conf"), "Host level-b\nInclude c.conf\n").expect("write b");
        std::fs::write(root.join("c.conf"), "Host level-c\n").expect("write c");

        let hosts = load_openssh_hosts_from(&config);
        let aliases: Vec<&str> = hosts.iter().map(|host| host.alias.as_str()).collect();
        assert_eq!(aliases, vec!["top", "level-a", "level-b"]);
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn resolve_ssh_marker_fills_server_and_tunnel_fields() {
        let hosts = vec![test_openssh_host(
            "web1",
            "10.1.2.3",
            Some("deploy"),
            Some("2222"),
            Some("/home/deploy/.ssh/id_test"),
        )];
        let mut server = serde_json::json!({
            "name": "web",
            "ssh_config": "openssh:web1",
        });
        resolve_ssh_into_with(&mut server, &[], &hosts).expect("resolve marker");
        assert_eq!(server["host"], json!("10.1.2.3"));
        assert_eq!(server["user"], json!("deploy"));
        assert_eq!(server["port"], json!(2222));
        assert_eq!(server["ssh_auth"], json!("key"));
        assert_eq!(server["ssh_key_path"], json!("/home/deploy/.ssh/id_test"));

        let mut connection = serde_json::json!({
            "name": "pg",
            "options": {"ssh_config": "openssh:web1"},
            "ssh_tunnel": {"enabled": true, "local_port": 15432},
        });
        resolve_ssh_tunnel_into_with(&mut connection, &[], &hosts).expect("resolve tunnel marker");
        assert_eq!(connection["ssh_tunnel"]["host"], json!("10.1.2.3"));
        assert_eq!(connection["ssh_tunnel"]["user"], json!("deploy"));
        assert_eq!(connection["ssh_tunnel"]["port"], json!(2222));
        assert_eq!(connection["ssh_tunnel"]["auth"], json!("key"));
        assert_eq!(
            connection["ssh_tunnel"]["key_path"],
            json!("/home/deploy/.ssh/id_test")
        );
        assert_eq!(connection["ssh_tunnel"]["local_port"], json!(15432));
        assert!(connection["ssh_tunnel"].get("name").is_none());
        // The marker survives so the selection can be restored after restart.
        assert_eq!(connection["options"]["ssh_config"], json!("openssh:web1"));
    }

    #[test]
    fn resolve_ssh_marker_missing_alias_reports_error() {
        let mut server = serde_json::json!({"name": "web", "ssh_config": "openssh:gone"});
        assert_eq!(
            resolve_ssh_into_with(&mut server, &[], &[]),
            Err("alias gone not found in ssh config".to_string())
        );
        let mut connection = serde_json::json!({
            "name": "pg",
            "options": {"ssh_config": "openssh:gone"},
            "ssh_tunnel": {"enabled": true},
        });
        assert_eq!(
            resolve_ssh_tunnel_into_with(&mut connection, &[], &[]),
            Err("alias gone not found in ssh config".to_string())
        );
    }

    #[test]
    fn resolve_config_auth_entry_maps_to_concrete_auth_for_engines() {
        let mut entry = SshEntry::blank();
        entry.name = "alias".to_string();
        entry.auth = SshAuthMode::Config;
        entry.ssh_alias = Some("web1".to_string());
        entry.host = "10.1.2.3".to_string();
        entry.user = "deploy".to_string();
        entry.port = "2222".to_string();
        entry.key_path = "/home/deploy/.ssh/id_test".to_string();

        let resolved = resolve_openssh_entry(
            &entry,
            &[test_openssh_host(
                "web1",
                "10.1.2.3",
                Some("deploy"),
                Some("2222"),
                Some("/home/deploy/.ssh/id_test"),
            )],
        )
        .expect("resolve entry");
        assert_eq!(resolved.auth, SshAuthMode::Key);
        assert_eq!(resolved.key_path, "/home/deploy/.ssh/id_test");

        let mut server = serde_json::json!({"ssh_config": "alias"});
        resolve_ssh_into_with(&mut server, &[entry], &[]).expect("resolve config entry");
        // php-deploy only accepts concrete auth modes.
        assert_eq!(server["ssh_auth"], json!("key"));
        assert_eq!(server["ssh_key_path"], json!("/home/deploy/.ssh/id_test"));
    }

    #[test]
    fn ssh_store_round_trips_config_auth_alias() {
        let root = temp_root("ssh_store_config_alias");
        let mut entry = SshEntry::blank();
        entry.name = "web1-alias".to_string();
        entry.host = "10.1.2.3".to_string();
        entry.user = "deploy".to_string();
        entry.port = "2222".to_string();
        entry.auth = SshAuthMode::Config;
        entry.ssh_alias = Some("web1".to_string());
        entry.key_path = "/home/deploy/.ssh/id_test".to_string();
        save_ssh_configs(&root, &[entry]).expect("save config-auth entry");

        let stored = read_json(&root.join(".zed/ssh.json")).expect("read ssh.json");
        assert_eq!(stored["configs"][0]["auth"], json!("config"));
        assert_eq!(stored["configs"][0]["ssh_alias"], json!("web1"));
        let loaded = load_ssh_configs(&root);
        assert_eq!(loaded[0].auth, SshAuthMode::Config);
        assert_eq!(loaded[0].ssh_alias.as_deref(), Some("web1"));

        let mut missing = SshEntry::blank();
        missing.name = "missing".to_string();
        missing.auth = SshAuthMode::Config;
        assert_eq!(
            save_ssh_configs(&root, &[missing]),
            Err("missing: ssh config alias is required".to_string())
        );
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn resolve_ssh_into_fills_server_fields_and_auth() {
        let mut config = SshEntry::blank();
        config.name = "bastion".to_string();
        config.user = "deploy".to_string();
        config.host = "10.0.0.5".to_string();
        config.port = "2222".to_string();
        config.auth = SshAuthMode::Password;
        config.password = "secret".to_string();
        config.keep_alive = 30;
        config.strict_host_key = true;
        let configs = vec![config];

        let mut server = serde_json::json!({
            "name": "web1",
            "host": "old.host",
            "user": "old",
            "port": 21,
            "ssh_config": "bastion",
        });
        resolve_ssh_into(&mut server, &configs).expect("resolve server");
        assert_eq!(server["host"], json!("10.0.0.5"));
        assert_eq!(server["user"], json!("deploy"));
        assert_eq!(server["port"], json!(2222));
        assert_eq!(server["ssh_auth"], json!("password"));
        assert_eq!(server["ssh_password"], json!("secret"));
        assert_eq!(server["ssh_strict_host_key"], json!(true));
        assert_eq!(server["ssh_keep_alive"], json!(30));

        let mut tunnelled = serde_json::json!({
            "name": "pg",
            "ssh_tunnel": {"enabled": true, "name": "bastion", "local_port": 15432, "port": 1},
        });
        resolve_ssh_tunnel_into(&mut tunnelled, &configs).expect("resolve tunnel");
        assert_eq!(tunnelled["ssh_tunnel"]["host"], json!("10.0.0.5"));
        assert_eq!(tunnelled["ssh_tunnel"]["port"], json!(2222));
        assert_eq!(tunnelled["ssh_tunnel"]["user"], json!("deploy"));
        assert_eq!(tunnelled["ssh_tunnel"]["local_port"], json!(15432));
        assert!(tunnelled["ssh_tunnel"].get("name").is_none());
        assert_eq!(tunnelled["ssh_tunnel"]["keep_alive"], json!(30));

        let mut disabled = serde_json::json!({
            "ssh_tunnel": {"enabled": false, "name": "bastion"},
        });
        resolve_ssh_tunnel_into(&mut disabled, &configs).expect("disabled tunnel untouched");
        assert!(disabled["ssh_tunnel"].get("host").is_none());

        let mut unnamed = serde_json::json!({
            "ssh_tunnel": {"enabled": true, "host": "manual.host"},
        });
        resolve_ssh_tunnel_into(&mut unnamed, &configs).expect("unnamed tunnel untouched");
        assert_eq!(unnamed["ssh_tunnel"]["host"], json!("manual.host"));
    }

    #[test]
    fn resolve_ssh_into_missing_config_reports_name() {
        let configs = Vec::new();

        let mut tunnelled = serde_json::json!({"ssh_tunnel": {"enabled": true, "name": "gone"}});
        assert_eq!(
            resolve_ssh_tunnel_into(&mut tunnelled, &configs),
            Err("ssh configuration 'gone' not found".to_string())
        );

        let mut server = serde_json::json!({"ssh_config": "gone"});
        assert_eq!(
            resolve_ssh_into(&mut server, &configs),
            Err("ssh configuration 'gone' not found".to_string())
        );
    }

    #[test]
    fn unique_ssh_config_name_avoids_collisions() {
        assert_eq!(unique_ssh_config_name("config", &[]), "config");
        let mut existing = SshEntry::blank();
        existing.name = "config".to_string();
        let configs = vec![existing];
        assert_eq!(unique_ssh_config_name("config", &configs), "config (1)");
    }

    #[test]
    fn ssh_user_host_text_lists_user_host_port() {
        let mut config = SshEntry::blank();
        config.user = "deploy".to_string();
        config.host = "bastion.local".to_string();
        config.port = "2222".to_string();
        assert_eq!(ssh_user_host_text(&config), "deploy@bastion.local:2222");
        config.user = String::new();
        assert_eq!(ssh_user_host_text(&config), "bastion.local:2222");
        config.port = String::new();
        assert_eq!(ssh_user_host_text(&config), "bastion.local");
    }

    #[test]
    fn deploy_server_entry_round_trips_ssh_config() {
        let root = temp_root("deploy_ssh_config_field");
        let mut server = blank_server(root.clone());
        server.name = "web1".to_string();
        server.protocol = Some("sftp".to_string());
        server.ssh_config = Some("bastion".to_string());
        let entry = deploy_server_entry(&server);
        assert_eq!(entry["ssh_config"], json!("bastion"));
        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert_eq!(parsed.ssh_config.as_deref(), Some("bastion"));

        server.ssh_config = None;
        let entry = deploy_server_entry(&server);
        assert!(entry.get("ssh_config").is_none());
        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert_eq!(parsed.ssh_config, None);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn data_source_entry_stores_ssh_tunnel_name() {
        let mut source = DataSource::blank(PathBuf::from("/tmp"));
        source.name = "pg".to_string();
        source.driver = "postgres".to_string();
        source.ssh_enabled = true;
        source.ssh_name = "bastion".to_string();
        source.ssh_local_port = "15432".to_string();
        let entry = data_source_entry(&source);
        // staged tunnel is name+local_port only; the NAME is panel-side state,
        // php-db denies unknown fields so the serialized tunnel carries the
        // resolved host/user/port instead (written by resolve_ssh_tunnel_into)
        assert!(entry["ssh_tunnel"].get("name").is_none());
        assert_eq!(entry["ssh_tunnel"]["local_port"], json!(15432));
        assert_eq!(entry["ssh_tunnel"]["enabled"], json!(true));
    }

    #[test]
    fn data_source_options_rows_roundtrip_sorted_and_skip_blank() {
        let mut options = serde_json::Map::new();
        options.insert("b_key".to_string(), json!("2"));
        options.insert("a_key".to_string(), json!(1));
        let rows = options_to_rows(&options);
        assert_eq!(
            rows,
            vec![
                ("a_key".to_string(), "1".to_string()),
                ("b_key".to_string(), "2".to_string())
            ]
        );

        let staged = vec![
            ("z_key".to_string(), "true".to_string()),
            (String::new(), "skipped".to_string()),
            ("a_key".to_string(), "text".to_string()),
        ];
        let options = rows_to_options(&staged);
        let keys = options.keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys, vec!["a_key".to_string(), "z_key".to_string()]);
        assert_eq!(options["a_key"], json!("text"));
        assert_eq!(options["z_key"], json!("true"));
    }

    #[test]
    fn data_source_excluded_schemas_roundtrip() {
        let value = json!(["demo.sqlite", "other"]);
        assert_eq!(
            parse_excluded_schemas(Some(&value)),
            vec!["demo.sqlite".to_string(), "other".to_string()]
        );
        assert!(parse_excluded_schemas(None).is_empty());

        let excluded = vec!["demo.sqlite".to_string()];
        assert!(!schema_checked(&excluded, "demo.sqlite"));
        assert!(schema_checked(&excluded, "other"));
        let mut toggled = excluded.clone();
        toggled.retain(|name| name != "demo.sqlite");
        assert!(schema_checked(&toggled, "demo.sqlite"));
    }

    #[test]
    fn data_source_test_config_serializes_only_selected_connection() {
        let mut source = DataSource::blank(PathBuf::from("/tmp"));
        source.name = "app".to_string();
        source.path = "demo.sqlite".to_string();
        source.read_only = true;
        let config = data_source_test_config(&source);
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0]["name"], json!("_test"));
        assert_eq!(connections[0]["path"], json!("demo.sqlite"));
        assert_eq!(connections[0]["read_only"], json!(true));
    }

    #[test]
    fn sqlite_file_name_detector_matches_only_sqlite_extensions() {
        for name in [
            "demo.sqlite",
            "backup.db",
            "dump.sqlite3",
            "notes.DB",
            "x.SQLite",
        ] {
            assert!(is_sqlite_file_name(name), "{name} should match");
        }
        for name in [
            "photo.png",
            "local.php",
            "README",
            "notes.sqlite.bak",
            "archive.sql",
            ".db",
        ] {
            assert!(!is_sqlite_file_name(name), "{name} should not match");
        }
    }

    #[test]
    fn sqlite_data_source_from_file_derives_name_and_connection() {
        let root = PathBuf::from("/tmp/project");
        let source = sqlite_data_source_from_file("demo.sqlite", "data/demo.sqlite", &root);
        assert_eq!(source.name, "demo.sqlite");
        assert!(source.is_sqlite());
        assert_eq!(source.path, "data/demo.sqlite");
        assert_eq!(source.root, root);
        assert!(source.host.is_empty());

        let entry = data_source_entry(&source);
        assert_eq!(entry["name"], json!("demo.sqlite"));
        assert_eq!(entry["driver"], json!("sqlite"));
        assert_eq!(entry["path"], json!("data/demo.sqlite"));
        assert!(entry.get("host").is_none());
        assert!(entry.get("database").is_none());
        assert!(entry.get("url").is_none());
    }

    #[test]
    fn upsert_sqlite_data_source_preserves_other_connections() {
        let root = temp_root("sqlite_data_source_upsert");
        std::fs::create_dir_all(root.join(".zed")).expect("create .zed");
        std::fs::write(
            root.join(".zed/databases.json"),
            r#"{"connections":[{"name":"pg","driver":"postgres","url":"postgres://localhost/app","password_env":"PGPASSWORD"}]}"#,
        )
        .expect("seed databases.json");

        let name = upsert_sqlite_data_source(&root, "data/demo.sqlite").expect("upsert");
        assert_eq!(name, "demo.sqlite");

        let config = read_json(&root.join(".zed/databases.json")).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 2);
        let pg = &connections[0];
        assert_eq!(pg["name"], json!("pg"));
        assert_eq!(pg["url"], json!("postgres://localhost/app"));
        assert_eq!(pg["password_env"], json!("PGPASSWORD"));
        let added = &connections[1];
        assert_eq!(added["name"], json!("demo.sqlite"));
        assert_eq!(added["driver"], json!("sqlite"));
        assert_eq!(added["path"], json!("data/demo.sqlite"));
        assert!(added.get("host").is_none());

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn upsert_sqlite_data_source_uniquifies_repeated_names() {
        let root = temp_root("sqlite_data_source_unique");
        std::fs::create_dir_all(root.join(".zed")).expect("create .zed");
        std::fs::write(
            root.join(".zed/databases.json"),
            r#"{"connections":[{"name":"demo.sqlite","driver":"sqlite","path":"old.sqlite"}]}"#,
        )
        .expect("seed databases.json");

        let name = upsert_sqlite_data_source(&root, "data/demo.sqlite").expect("upsert");
        assert_eq!(name, "demo.sqlite (1)");

        let repeated = upsert_sqlite_data_source(&root, "data/demo.sqlite").expect("upsert");
        assert_eq!(repeated, "demo.sqlite (2)");

        let config = read_json(&root.join(".zed/databases.json")).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 3);
        let existing = &connections[0];
        assert_eq!(existing["name"], json!("demo.sqlite"));
        assert_eq!(existing["path"], json!("old.sqlite"));
        let first = &connections[1];
        assert_eq!(first["name"], json!("demo.sqlite (1)"));
        assert_eq!(first["path"], json!("data/demo.sqlite"));
        let second = &connections[2];
        assert_eq!(second["name"], json!("demo.sqlite (2)"));
        assert_eq!(second["path"], json!("data/demo.sqlite"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn find_data_source_by_path_reuses_existing_connection() {
        let root = temp_root("sqlite_data_source_reuse");
        std::fs::create_dir_all(root.join(".zed")).expect("create .zed");
        std::fs::write(
            root.join(".zed/databases.json"),
            r#"{"connections":[{"name":"app","driver":"sqlite","path":"demo.sqlite"},{"name":"other","driver":"sqlite","path":"other.sqlite"}]}"#,
        )
        .expect("seed databases.json");

        let sources = load_data_sources(&root);
        assert_eq!(
            find_data_source_by_path(&sources, "demo.sqlite").as_deref(),
            Some("app")
        );
        assert_eq!(find_data_source_by_path(&sources, "missing.sqlite"), None);

        // Opening the same file must reuse "app" and leave the file untouched.
        let name = ensure_sqlite_data_source(&root, "demo.sqlite").expect("reuse");
        assert_eq!(name, "app");

        let config = read_json(&root.join(".zed/databases.json")).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["name"], json!("app"));
        assert_eq!(connections[1]["name"], json!("other"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn remove_connection_removes_only_named_entry() {
        let mut connections = serde_json::json!([
            {"name": "app", "driver": "sqlite", "path": "demo.sqlite"},
            {
                "name": "app-ro",
                "driver": "sqlite",
                "path": "demo.sqlite",
                "options": {"busy_timeout": "5000", "custom_unknown": "kept"},
                "ssh_tunnel": {"enabled": false, "host": "bastion.example.com"}
            },
            {"name": "pg", "driver": "postgres", "url": "postgres://app@localhost/app"}
        ])
        .as_array()
        .cloned()
        .expect("connections array");

        assert!(remove_connection(&mut connections, "app-ro"));
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["name"], json!("app"));
        assert_eq!(connections[1]["name"], json!("pg"));
        // untouched entries keep unknown/nested fields verbatim
        let mut untouched = serde_json::json!([
            {"name": "app-ro", "options": {"custom_unknown": "kept"},
             "ssh_tunnel": {"enabled": false, "host": "bastion.example.com"}}
        ])
        .as_array()
        .cloned()
        .expect("array");
        remove_connection(&mut untouched, "missing");
        assert_eq!(untouched[0]["options"]["custom_unknown"], json!("kept"));
        assert_eq!(
            untouched[0]["ssh_tunnel"]["host"],
            json!("bastion.example.com")
        );
    }

    #[test]
    fn remove_connection_missing_name_is_noop() {
        let original = serde_json::json!([
            {"name": "app", "driver": "sqlite", "path": "demo.sqlite"},
            {"name": "pg", "driver": "postgres", "url": "postgres://app@localhost/app"}
        ]);
        let mut connections = original.as_array().cloned().expect("array");

        assert!(!remove_connection(&mut connections, "absent"));
        assert_eq!(Value::Array(connections), original);
    }

    #[test]
    fn delete_data_source_preserves_other_connections_and_top_level_keys() {
        let root = temp_root("delete_data_source");
        std::fs::create_dir_all(root.join(".zed")).expect("create .zed");
        std::fs::write(
            root.join(".zed/databases.json"),
            r#"{
                "version": 7,
                "connections": [
                    {"name":"app","driver":"sqlite","path":"demo.sqlite"},
                    {"name":"demo2","driver":"sqlite","path":"demo.sqlite"},
                    {"name":"app-ro","driver":"sqlite","path":"demo.sqlite","read_only":true,
                     "options":{"busy_timeout":"5000","custom_unknown":"kept-but-ignored"},
                     "ssh_tunnel":{"enabled":false,"host":"bastion.example.com","port":22}}
                ]
            }"#,
        )
        .expect("seed databases.json");

        assert!(delete_data_source(&root, "demo2").expect("delete"));
        let config = read_json(&root.join(".zed/databases.json")).expect("parse databases.json");
        assert_eq!(config["version"], json!(7));
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["name"], json!("app"));
        assert_eq!(connections[1]["name"], json!("app-ro"));
        assert_eq!(connections[1]["read_only"], json!(true));
        assert_eq!(
            connections[1]["options"]["custom_unknown"],
            json!("kept-but-ignored")
        );
        assert_eq!(
            connections[1]["ssh_tunnel"]["host"],
            json!("bastion.example.com")
        );
        assert_eq!(connections[1]["ssh_tunnel"]["port"], json!(22));

        // Deleting a name that is not there leaves the file semantically unchanged.
        let before = read_json(&root.join(".zed/databases.json")).expect("parse");
        assert!(!delete_data_source(&root, "absent").expect("no-op"));
        let after = read_json(&root.join(".zed/databases.json")).expect("parse");
        assert_eq!(before, after);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn data_source_entry_and_parse_roundtrip() {
        let mut source = DataSource::blank(PathBuf::from("/tmp"));
        source.name = "pg".to_string();
        source.driver = "postgres".to_string();
        source.host = "localhost".to_string();
        source.port = "5432".to_string();
        source.user = "app".to_string();
        source.password = "secret".to_string();
        source.database = "app".to_string();
        source.read_only = true;
        source.ssh_enabled = true;
        source.ssh_name = "bastion".to_string();
        source.ssh_local_port = "15432".to_string();
        source.excluded_schemas = vec!["public".to_string()];
        source.options = vec![("connect_timeout".to_string(), "5".to_string())];

        let entry = data_source_entry(&source);
        assert_eq!(entry["port"], json!(5432));
        assert_eq!(entry["read_only"], json!(true));
        // staged tunnel is name+local_port only; host/port/user resolved at apply/test
        assert!(entry["ssh_tunnel"].get("name").is_none());
        assert_eq!(entry["ssh_tunnel"]["local_port"], json!(15432));
        assert!(entry["ssh_tunnel"].get("host").is_none());
        assert!(entry["ssh_tunnel"].get("port").is_none());
        assert!(entry["ssh_tunnel"].get("user").is_none());
        assert_eq!(entry["excluded_schemas"], json!(["public"]));
        assert_eq!(entry["options"]["connect_timeout"], json!("5"));

        let parsed = parse_data_source(&entry, Path::new("/tmp")).expect("parse entry");
        assert_eq!(parsed.name, "pg");
        assert_eq!(parsed.port, "5432");
        assert!(parsed.read_only);
        assert!(parsed.ssh_enabled);
        assert!(parsed.ssh_name.is_empty());
        assert_eq!(parsed.ssh_local_port, "15432");
        assert_eq!(parsed.excluded_schemas, vec!["public".to_string()]);
        assert_eq!(
            parsed.options,
            vec![("connect_timeout".to_string(), "5".to_string())]
        );
    }

    #[test]
    fn data_source_problems_list_validation_errors() {
        let mut source = DataSource::blank(PathBuf::from("/tmp"));
        source.name = "app".to_string();
        source.driver = "sqlite".to_string();
        assert!(data_source_problems(&source).contains(&"file path is required".to_string()));
        source.path = "demo.sqlite".to_string();
        assert!(data_source_problems(&source).is_empty());
        source.driver = "postgres".to_string();
        source.url = "postgres://app@localhost:5432/app".to_string();
        assert!(data_source_problems(&source).is_empty());
        source.url = String::new();
        let problems = data_source_problems(&source);
        for expected in [
            "host is required",
            "port is required",
            "user is required",
            "database is required",
        ] {
            assert!(
                problems.contains(&expected.to_string()),
                "missing {expected}"
            );
        }
        source.ssh_enabled = true;
        assert!(data_source_problems(&source).contains(&"ssh host is required".to_string()));
    }

    #[test]
    fn save_data_sources_preserves_unknown_keys_and_drops_removed() {
        let root = temp_root("data_sources_save");
        let zed = root.join(".zed");
        std::fs::create_dir_all(&zed).expect("create .zed");
        std::fs::write(
            zed.join("databases.json"),
            r#"{
  "connections": [
    { "name": "app", "driver": "sqlite", "path": "old.sqlite", "password_env": "PGPASSWORD" },
    { "name": "stale", "driver": "sqlite", "path": "gone.sqlite" }
  ]
}"#,
        )
        .expect("seed databases.json");

        let mut app = DataSource::blank(root.clone());
        app.name = "app".to_string();
        app.driver = "sqlite".to_string();
        app.path = "demo.sqlite".to_string();
        app.read_only = true;
        let mut pg = DataSource::blank(root.clone());
        pg.name = "pg".to_string();
        pg.driver = "postgres".to_string();
        pg.url = "postgres://app@localhost:5432/app".to_string();

        save_data_sources(&root, &[app, pg]).expect("save data sources");

        let config = read_json(&zed.join("databases.json")).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["path"], json!("demo.sqlite"));
        assert_eq!(connections[0]["read_only"], json!(true));
        assert_eq!(connections[0]["password_env"], json!("PGPASSWORD"));
        assert_eq!(
            connections[1]["url"],
            json!("postgres://app@localhost:5432/app")
        );

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn empty_data_sources_seed_one_selected_blank_source() {
        let root = PathBuf::from("/tmp");
        let mut sources = Vec::new();
        let mut selected = None;
        seed_data_sources_if_empty(&mut sources, &mut selected, &root);

        assert_eq!(sources.len(), 1);
        assert_eq!(selected, Some(0));
        assert_eq!(sources[0].name, "datasource");
        assert!(sources[0].driver.is_empty(), "seeded source must await a driver");
        assert_eq!(sources[0].root, root);

        // Re-seeding an already populated dialog must not add another source.
        seed_data_sources_if_empty(&mut sources, &mut selected, &root);
        assert_eq!(sources.len(), 1);
        assert_eq!(selected, Some(0));
    }

    #[test]
    fn driver_picker_lists_all_drivers_for_blank_source() {
        let rows = driver_picker_rows("");
        assert_eq!(rows.len(), DATABASE_DRIVERS.len());
        assert_eq!(rows.len(), 8);
        assert!(rows.iter().all(|(_, is_current)| !is_current));
        assert_eq!(
            rows.iter().map(|(driver, _)| *driver).collect::<Vec<_>>(),
            DATABASE_DRIVERS.to_vec()
        );

        let rows = driver_picker_rows("postgres");
        assert_eq!(rows.iter().filter(|(_, is_current)| *is_current).count(), 1);
        assert!(rows
            .iter()
            .any(|(driver, is_current)| *driver == "postgres" && *is_current));

        let rows = driver_picker_rows("custom");
        assert!(rows.iter().all(|(_, is_current)| !is_current));
    }

    #[test]
    fn select_driver_falls_back_to_first_source() {
        let sources = vec![DataSource::blank(PathBuf::from("/tmp"))];
        assert_eq!(resolve_selected_source(None, &sources), Some(0));
        assert_eq!(resolve_selected_source(Some(0), &sources), Some(0));
        assert_eq!(resolve_selected_source(Some(7), &sources), Some(0));
        assert_eq!(resolve_selected_source(None, &[]), None);
    }

    #[test]
    fn next_data_source_name_prefills_unique_names() {
        let mut blank = DataSource::blank(PathBuf::from("/tmp"));
        blank.name = "datasource".to_string();
        assert_eq!(next_data_source_name("datasource", &[]), "datasource");
        let sources = vec![blank.clone()];
        assert_eq!(
            next_data_source_name("datasource", &sources),
            "datasource (1)"
        );
        let mut copy = blank.clone();
        copy.name = "datasource copy".to_string();
        blank.name = "a".to_string();
        let sources = vec![blank, copy];
        assert_eq!(next_data_source_name("a copy", &sources), "a copy");
    }

    #[test]
    fn create_data_source_menu_lists_drivers_in_phpstorm_order() {
        let menu = create_data_source_menu(false);
        let drivers = menu
            .iter()
            .filter_map(|row| match row.action {
                CreateDataSourceAction::Driver(driver) => Some(driver),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(drivers, CREATE_DATA_SOURCE_DRIVERS.to_vec());
        assert_eq!(
            drivers,
            vec![
                "postgres",
                "mysql",
                "sqlserver",
                "mariadb",
                "sqlite",
                "oracle",
                "mongodb",
                "redis",
            ]
        );
        assert_eq!(
            create_data_source_driver_label("sqlserver"),
            "Microsoft SQL Server"
        );
        assert!(menu
            .iter()
            .filter(|row| matches!(row.action, CreateDataSourceAction::Driver(_)))
            .all(|row| row.enabled));
    }

    #[test]
    fn create_data_source_menu_places_all_sources_after_drivers() {
        let menu = create_data_source_menu(false);
        let all_index = menu
            .iter()
            .position(|row| row.action == CreateDataSourceAction::AllDataSources)
            .expect("All Data Sources row");
        assert_eq!(all_index, CREATE_DATA_SOURCE_DRIVERS.len());
        assert!(menu[all_index].enabled);
        assert_eq!(
            menu.iter()
                .filter(|row| matches!(row.action, CreateDataSourceAction::Demo(_)))
                .count(),
            3
        );
        assert_eq!(menu.len(), CREATE_DATA_SOURCE_DRIVERS.len() + 1 + 3);
    }

    #[test]
    fn create_data_source_menu_demo_enablement_requires_sqlite_file() {
        let disabled = create_data_source_menu(false);
        assert!(disabled
            .iter()
            .filter(|row| matches!(row.action, CreateDataSourceAction::Demo(_)))
            .all(|row| !row.enabled));

        let enabled = create_data_source_menu(true);
        for row in enabled {
            if let CreateDataSourceAction::Demo(demo) = row.action {
                assert_eq!(row.enabled, demo == DemoDatabase::Sqlite);
                if !row.enabled {
                    assert_eq!(demo.disabled_reason(), Some("requires a local server"));
                }
            }
        }
        assert_eq!(
            DemoDatabase::Sqlite.disabled_reason(),
            Some("demo.sqlite not found in project")
        );
    }

    #[test]
    fn create_data_source_name_derives_numbered_unique_names() {
        let root = PathBuf::from("/tmp");
        assert_eq!(create_data_source_name("postgres", &[]), "postgres1");
        let mut first = DataSource::blank(root.clone());
        first.name = "postgres1".to_string();
        let mut second = DataSource::blank(root.clone());
        second.name = "postgres2".to_string();
        let sources = vec![first, second];
        assert_eq!(create_data_source_name("postgres", &sources), "postgres3");
        assert_eq!(create_data_source_name("mysql", &sources), "mysql1");
    }

    #[test]
    fn seeded_data_source_preselects_driver_and_defaults() {
        let root = PathBuf::from("/tmp/project");
        let postgres = seeded_data_source("postgres", None, &root, &[]);
        assert_eq!(postgres.name, "postgres1");
        assert_eq!(postgres.driver, "postgres");
        assert_eq!(postgres.port, "5432");
        assert_eq!(postgres.root, root);
        assert!(postgres.path.is_empty());

        let mysql = seeded_data_source("mysql", None, &root, &[]);
        assert_eq!(mysql.name, "mysql1");
        assert_eq!(mysql.port, "3306");

        let sqlite = seeded_data_source("sqlite", Some("demo.sqlite"), &root, &[]);
        assert_eq!(sqlite.name, "sqlite1");
        assert_eq!(sqlite.path, "demo.sqlite");
        assert!(sqlite.port.is_empty());

        let existing = vec![postgres];
        assert_eq!(
            seeded_data_source("postgres", None, &root, &existing).name,
            "postgres2"
        );
    }

    #[test]
    fn demo_seed_never_fabricates_a_connection() {
        let sqlite = DataSourceSeed::demo(DemoDatabase::Sqlite);
        assert_eq!(sqlite.driver, "sqlite");
        assert_eq!(sqlite.path.as_deref(), Some("demo.sqlite"));
        let postgres = DataSourceSeed::demo(DemoDatabase::Postgres);
        assert_eq!(postgres.driver, "postgres");
        assert!(postgres.path.is_none());
        assert!(!DemoDatabase::Postgres.enabled(true));
        assert!(!DemoDatabase::MySql.enabled(true));
        assert!(!DemoDatabase::Sqlite.enabled(false));
        assert!(DemoDatabase::Sqlite.enabled(true));
    }

    #[test]
    fn deploy_type_badges_normalize_aliases() {
        assert_eq!(deploy_type_canonical("SSH"), Some("sftp"));
        assert_eq!(deploy_type_canonical("inplace"), Some("in-place"));
        assert_eq!(deploy_type_canonical("dav+"), Some("webdavs"));
        assert_eq!(deploy_type_canonical("ftp-ssl"), Some("ftps"));
        assert_eq!(deploy_type_canonical("gopher"), None);

        let expected = [
            ("local", "local"),
            ("in-place", "dl"),
            ("sftp", "ssh"),
            ("ftp", "ftp"),
            ("ftps", "ftp"),
            ("webdav", "dav"),
            ("webdavs", "dav+"),
        ];
        for (kind, badge) in expected {
            assert_eq!(deploy_type_badge_short(kind), badge);
        }
        assert_eq!(deploy_type_badge_short("ssh"), "ssh");
        assert_eq!(deploy_type_badge_short("gopher"), "?");

        assert_eq!(deploy_type_display_name("webdavs"), "WebDAV over HTTPS");
        assert_eq!(deploy_type_display_name("in-place"), "In place");
        assert_eq!(deploy_type_display_name("local"), "Local or mounted folder");
    }

    #[test]
    fn deploy_type_dropdown_options_exact_order_and_labels() {
        assert_eq!(
            deploy_type_options(),
            &[
                ("in-place", "In place"),
                ("local", "Local or mounted folder"),
                ("ftp", "FTP"),
                ("ftps", "FTPS"),
                ("sftp", "SFTP"),
                ("webdav", "WebDAV"),
                ("webdavs", "WebDAV over HTTPS"),
            ]
        );
    }

    #[test]
    fn deploy_type_dropdown_labels_map_to_canonical_protocols() {
        for (canonical, label) in deploy_type_options() {
            assert_eq!(deploy_type_canonical_from_label(label), Some(*canonical));
            assert_eq!(deploy_type_canonical(canonical), Some(*canonical));
        }
        assert_eq!(deploy_type_canonical_from_label("sftp"), None);
        assert_eq!(deploy_type_canonical_from_label("WebDAV (HTTPS)"), None);
        assert_eq!(deploy_type_canonical_from_label("SSH"), None);
    }

    #[test]
    fn deploy_mapping_labels_match_phpstorm() {
        assert_eq!(
            deploy_mapping_labels(),
            ["Local path:", "Deployment path:", "Web path:"]
        );
    }

    #[test]
    fn deploy_connection_fields_matrix_matches_types() {
        assert_eq!(
            deploy_connection_fields("sftp"),
            &[
                DeployField::SshConfig,
                DeployField::TestConnection,
                DeployField::RootPath,
                DeployField::WebUrl,
                DeployField::RsyncToggle,
                DeployField::SudoToggle,
            ]
        );
        for kind in ["ftp", "ftps"] {
            assert_eq!(
                deploy_connection_fields(kind),
                &[
                    DeployField::HostPort,
                    DeployField::UserAnonymous,
                    DeployField::Password,
                    DeployField::TestConnection,
                    DeployField::RootPath,
                    DeployField::WebUrl,
                    DeployField::RsyncToggle,
                ]
            );
        }
        for kind in ["webdav", "webdavs"] {
            assert_eq!(
                deploy_connection_fields(kind),
                &[
                    DeployField::HostPort,
                    DeployField::UserAnonymous,
                    DeployField::Password,
                    DeployField::TestConnection,
                    DeployField::RootPath,
                    DeployField::WebUrl,
                ]
            );
        }
        assert_eq!(
            deploy_connection_fields("local"),
            &[DeployField::Folder, DeployField::WebUrl]
        );
        assert_eq!(deploy_connection_fields("in-place"), &[DeployField::WebUrl]);

        assert_eq!(deploy_default_port("ftp"), Some("21"));
        assert_eq!(deploy_default_port("ftps"), Some("990"));
        assert_eq!(deploy_default_port("webdav"), Some("80"));
        assert_eq!(deploy_default_port("webdavs"), Some("443"));
        assert_eq!(deploy_default_port("sftp"), Some("22"));
    }

    #[test]
    fn deploy_type_switch_prefills_default_ports() {
        assert_eq!(deploy_default_port("webdavs"), Some("443"));
        assert_eq!(deploy_default_port("webdav"), Some("80"));
        assert_eq!(deploy_default_port("ftp"), Some("21"));
        assert_eq!(deploy_default_port("ftps"), Some("990"));
        assert_eq!(deploy_default_port("sftp"), Some("22"));
        assert_eq!(deploy_default_port("local"), None);
        assert_eq!(deploy_default_port("in-place"), None);
    }

    #[test]
    fn deploy_type_options_render_badges_and_display_names() {
        for (canonical, label) in deploy_type_options() {
            assert_ne!(deploy_type_badge_short(canonical), "?");
            assert_eq!(deploy_type_display_name(canonical), *label);
        }
        assert_eq!(deploy_type_badge_short("webdavs"), "dav+");
        assert_eq!(deploy_type_badge_short("sftp"), "ssh");
    }

    #[test]
    fn use_sudo_round_trips_through_config() {
        let root = temp_root("deploy_use_sudo");
        let mut server = blank_server(root.clone());
        server.name = "web1".to_string();
        server.protocol = Some("sftp".to_string());
        server.use_sudo = true;

        let entry = deploy_server_entry(&server);
        assert_eq!(entry["use_sudo"], json!(true));
        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert!(parsed.use_sudo);

        let mut plain = blank_server(root.clone());
        plain.name = "web2".to_string();
        plain.protocol = Some("sftp".to_string());
        let entry = deploy_server_entry(&plain);
        assert!(entry.get("use_sudo").is_none());
        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert!(!parsed.use_sudo);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn parse_validate_output_reads_engine_shapes() {
        let per_server: Value = serde_json::json!({
            "servers": [
                {"name": "sftp", "warnings": ["SFTP host is not specified"]},
                {"name": "web1", "warnings": [{"message": "Root path is not specified"}]}
            ]
        });
        let parsed = parse_validate_output(&per_server);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "sftp");
        assert_eq!(parsed[0].1, vec!["SFTP host is not specified".to_string()]);
        assert_eq!(parsed[1].1, vec!["Root path is not specified".to_string()]);

        let mapped: Value = serde_json::json!({
            "warnings": {"web1": ["no root found"]}
        });
        let parsed = parse_validate_output(&mapped);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "web1");
        assert_eq!(parsed[0].1, vec!["no root found".to_string()]);

        let flat: Value = serde_json::json!({"warnings": ["Folder is not specified"]});
        let parsed = parse_validate_output(&flat);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].0.is_empty());
        assert_eq!(parsed[0].1, vec!["Folder is not specified".to_string()]);

        assert!(parse_validate_output(&serde_json::json!({})).is_empty());
        assert!(parse_validate_output(&serde_json::json!({"servers": [{"name": "x"}]})).is_empty());
    }

    #[test]
    fn parse_autodetect_root_reads_engine_shapes() {
        assert_eq!(
            parse_autodetect_root(&serde_json::json!({"root": "/var/www/app"})),
            Some("/var/www/app".to_string())
        );
        assert_eq!(
            parse_autodetect_root(&serde_json::json!({"path": "/srv"})),
            Some("/srv".to_string())
        );
        assert_eq!(
            parse_autodetect_root(&serde_json::json!({"result": {"found": "/www"}})),
            Some("/www".to_string())
        );
        assert_eq!(
            parse_autodetect_root(&serde_json::json!({"root": ""})),
            None
        );
        assert_eq!(
            parse_autodetect_root(&serde_json::json!({"entries": []})),
            None
        );
    }

    #[test]
    fn deploy_server_entry_serializes_new_fields() {
        let root = temp_root("deploy_new_fields");
        let mut server = blank_server(root.clone());
        server.name = "web1".to_string();
        server.protocol = Some("sftp".to_string());
        server.use_rsync = true;
        server.visible = false;
        server.password = Some("secret".to_string());

        let entry = deploy_server_entry(&server);
        assert_eq!(entry["use_rsync"], json!(true));
        assert_eq!(entry["visible"], json!(false));
        assert_eq!(entry["password"], json!("secret"));

        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert!(parsed.use_rsync);
        assert!(!parsed.visible);
        assert_eq!(parsed.password.as_deref(), Some("secret"));

        let mut quiet = blank_server(root.clone());
        quiet.name = "local".to_string();
        let entry = deploy_server_entry(&quiet);
        assert!(entry.get("use_rsync").is_none());
        assert_eq!(entry["visible"], json!(true));
        assert!(entry.get("password").is_none());
        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert!(!parsed.use_rsync);
        assert!(parsed.visible);
        assert_eq!(parsed.password, None);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn advanced_defaults_parse_from_missing_fields() {
        let root = Path::new("/tmp");
        let value: Value =
            serde_json::from_str(r#"{"name":"ftp1","protocol":"ftp"}"#).expect("parse config");
        let server = parse_deploy_server(&value, root).expect("parse server");
        assert!(server.passive_mode);
        assert!(!server.show_hidden);
        assert!(!server.list_compat);
        assert!(!server.use_mlsd);
        assert_eq!(server.max_connections, ADVANCED_DEFAULT_MAX_CONNECTIONS);
        assert!(server.keep_alive_enabled);
        assert_eq!(
            server.keep_alive_seconds,
            ADVANCED_DEFAULT_KEEP_ALIVE_SECONDS
        );
        assert_eq!(server.encoding, "UTF-8");
        assert!(!server.password_session);
    }

    #[test]
    fn advanced_round_trips_explicit_values() {
        let root = Path::new("/tmp");
        let value: Value = serde_json::from_str(
            r#"{"name":"ftp1","protocol":"ftps","passive_mode":false,"show_hidden":true,"list_compat":true,"use_mlsd":true,"max_connections":9,"keep_alive_enabled":false,"keep_alive_seconds":120,"encoding":"CP1251"}"#,
        )
        .expect("parse config");
        let server = parse_deploy_server(&value, root).expect("parse server");
        assert!(!server.passive_mode);
        assert!(server.show_hidden);
        assert!(server.list_compat);
        assert!(server.use_mlsd);
        assert_eq!(server.max_connections, 9);
        assert!(!server.keep_alive_enabled);
        assert_eq!(server.keep_alive_seconds, 120);
        assert_eq!(server.encoding, "CP1251");

        let entry = deploy_server_entry(&server);
        assert_eq!(entry["passive_mode"], json!(false));
        assert_eq!(entry["show_hidden"], json!(true));
        assert_eq!(entry["list_compat"], json!(true));
        assert_eq!(entry["use_mlsd"], json!(true));
        assert_eq!(entry["max_connections"], json!(9));
        assert_eq!(entry["keep_alive_enabled"], json!(false));
        assert_eq!(entry["keep_alive_seconds"], json!(120));
        assert_eq!(entry["encoding"], json!("CP1251"));
    }

    #[test]
    fn advanced_fields_serialize_only_for_ftp_family() {
        let root = temp_root("advanced_family");
        for protocol in ["ftp", "ftps", "webdav", "webdavs"] {
            let mut server = blank_server(root.clone());
            server.protocol = Some(protocol.to_string());
            let entry = deploy_server_entry(&server);
            assert_eq!(entry["passive_mode"], json!(true), "{protocol}");
            assert_eq!(entry["max_connections"], json!(5), "{protocol}");
            assert_eq!(entry["encoding"], json!("UTF-8"), "{protocol}");
        }
        for protocol in ["sftp", "local", "in-place"] {
            let mut server = blank_server(root.clone());
            server.protocol = Some(protocol.to_string());
            server.passive_mode = false;
            server.max_connections = 9;
            let entry = deploy_server_entry(&server);
            assert!(entry.get("passive_mode").is_none(), "{protocol}");
            assert!(entry.get("max_connections").is_none(), "{protocol}");
            assert!(entry.get("encoding").is_none(), "{protocol}");
        }
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn deploy_type_has_advanced_only_for_server_types() {
        assert!(deploy_type_has_advanced("ftp"));
        assert!(deploy_type_has_advanced("ftps"));
        assert!(deploy_type_has_advanced("webdav"));
        assert!(deploy_type_has_advanced("webdavs"));
        assert!(!deploy_type_has_advanced("sftp"));
        assert!(!deploy_type_has_advanced("local"));
        assert!(!deploy_type_has_advanced("in-place"));
        assert!(!deploy_type_has_advanced("nonsense"));
    }

    #[test]
    fn picker_query_matches_filters_names() {
        assert!(picker_query_matches("", "anything"));
        assert!(picker_query_matches("   ", "anything"));
        assert!(picker_query_matches("pub", "public"));
        assert!(picker_query_matches("PUB", "public"));
        assert!(picker_query_matches("src", "public/src"));
        assert!(!picker_query_matches("zzz", "public"));
        assert!(picker_query_matches("vendor", "vendor"));
    }

    #[test]
    fn parse_local_dir_entries_sorts_dirs_first() {
        let entries = parse_local_dir_entries(vec![
            ("README.md".to_string(), false),
            ("vendor".to_string(), true),
            ("app.log".to_string(), false),
            ("public".to_string(), true),
        ]);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["public", "vendor", "app.log", "README.md"]);
        assert!(entries[0].is_directory);
        assert_eq!(entries[0].path, "public");
        assert!(!entries[2].is_directory);
        assert_eq!(entries[2].size, None);
    }

    #[test]
    fn password_session_skips_serialization_in_deploy_entry() {
        let root = temp_root("password_session");
        let mut server = blank_server(root.clone());
        server.name = "ftp1".to_string();
        server.protocol = Some("ftp".to_string());
        server.password = Some("secret".to_string());
        let entry = deploy_server_entry(&server);
        assert_eq!(entry["password"], json!("secret"));

        server.password_session = true;
        let entry = deploy_server_entry(&server);
        assert!(entry.get("password").is_none());
        assert_eq!(entry["passive_mode"], json!(true));

        let parsed = parse_deploy_server(&entry, &root).expect("parse entry");
        assert_eq!(parsed.password, None);
        assert!(!parsed.password_session);

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn ssh_entry_session_skips_password_in_store() {
        let mut config = SshEntry::blank();
        config.name = "bastion".to_string();
        config.host = "10.0.0.5".to_string();
        config.password = "secret".to_string();
        let entry = ssh_store_entry(&config);
        assert_eq!(entry["password"], json!("secret"));

        config.password_session = true;
        let entry = ssh_store_entry(&config);
        assert!(entry.get("password").is_none());
        assert_eq!(entry["auth"], json!("password"));
        assert_eq!(entry["keep_alive"], json!(15));
    }

    #[test]
    fn deploy_temp_config_content_shape() {
        let entries = vec![
            serde_json::json!({"name": "web1", "protocol": "sftp"}),
            serde_json::json!({"name": "local", "protocol": "local"}),
        ];
        let content = deploy_temp_config_content(&entries, "web1");
        assert_eq!(content["active"], json!("web1"));
        let servers = content["servers"].as_array().expect("servers array");
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0]["name"], json!("web1"));
        assert_eq!(servers[1]["protocol"], json!("local"));
    }

    #[test]
    fn dialog_buffer_advanced_defaults_for_blank_server() {
        let buffer = DialogBuffer::load(None);
        assert_eq!(buffer.max_connections, "5");
        assert_eq!(buffer.keep_alive_seconds, "300");
        assert_eq!(buffer.encoding, "UTF-8");
        assert!(!buffer.password_session);

        let mut server = blank_server(Path::new("/tmp/advanced-buffer").to_path_buf());
        server.max_connections = 12;
        server.keep_alive_seconds = 60;
        server.encoding = String::new();
        let mut buffer = DialogBuffer::load(Some(&server));
        assert_eq!(buffer.max_connections, "12");
        assert_eq!(buffer.keep_alive_seconds, "60");
        assert_eq!(buffer.encoding, "");

        let mut staged = blank_server(Path::new("/tmp/advanced-buffer2").to_path_buf());
        buffer.apply(&mut staged, "ftp");
        assert_eq!(staged.max_connections, 12);
        assert_eq!(staged.keep_alive_seconds, 60);
        assert_eq!(staged.encoding, "UTF-8");
        assert_eq!(staged.password, None);
    }

    #[test]
    fn save_password_cycle_order_and_hints() {
        let mut state = SavePassword::Forever;
        assert_eq!(state.label(), "Forever");
        assert_eq!(state.hint(), "Password saved permanently");
        state = state.next();
        assert_eq!(state, SavePassword::Session);
        assert_eq!(state.label(), "Session");
        assert_eq!(state.hint(), "Password saved for the current session");
        state = state.next();
        assert_eq!(state, SavePassword::Forever);
    }

    #[test]
    fn dialog_buffer_applies_password_session_flag() {
        let mut buffer = DialogBuffer::load(None);
        buffer.name = "ftp1".to_string();
        buffer.password = "secret".to_string();
        buffer.password_session = true;
        let mut staged = blank_server(Path::new("/tmp/pw-session").to_path_buf());
        staged.password = Some("old".to_string());
        buffer.apply(&mut staged, "ftp");
        assert_eq!(staged.password.as_deref(), Some("secret"));
        assert!(staged.password_session);

        let entry = deploy_server_entry(&staged);
        assert!(entry.get("password").is_none());
    }

    #[test]
    fn reset_password_clears_staged_server_password() {
        let mut buffer = DialogBuffer::load(None);
        buffer.password = "secret".to_string();
        let mut staged = blank_server(Path::new("/tmp/pw-reset").to_path_buf());
        buffer.apply(&mut staged, "ftp");
        assert_eq!(staged.password.as_deref(), Some("secret"));

        buffer.password = String::new();
        buffer.password_session = false;
        buffer.apply(&mut staged, "ftp");
        assert_eq!(staged.password, None);
        assert!(!staged.password_session);
        let entry = deploy_server_entry(&staged);
        assert!(entry.get("password").is_none());
    }

    #[test]
    fn resolved_tunnel_round_trips_into_temp_config_for_php_db() {
        // databases.json cannot store the config name (php-db denies unknown
        // fields) - resolution must produce an engine-clean tunnel object with
        // the resolved host/user/port/auth keys
        let mut config = SshEntry::blank();
        config.name = "local-ssh".to_string();
        config.host = "127.0.0.1".to_string();
        config.user = "admin".to_string();
        config.port = "22".to_string();
        config.auth = SshAuthMode::Password;
        config.password = "secret".to_string();
        config.keep_alive = 15;
        let configs = vec![config];

        let mut connection = serde_json::json!({"ssh_tunnel": {"enabled": true, "name": "local-ssh", "local_port": 15432}});
        resolve_ssh_tunnel_into(&mut connection, &configs).expect("resolve tunnel");
        let tunnel = &connection["ssh_tunnel"];
        assert_eq!(tunnel["host"], json!("127.0.0.1"));
        assert_eq!(tunnel["user"], json!("admin"));
        assert_eq!(tunnel["port"], json!(22));
        assert_eq!(tunnel["auth"], json!("password"));
        assert_eq!(tunnel["password"], json!("secret"));
        assert_eq!(tunnel["strict_host_key"], json!(false));
        assert_eq!(tunnel["keep_alive"], json!(15));
        assert_eq!(tunnel["local_port"], json!(15432));
        for key in ["name", "config_name", "x_marker"] {
            assert!(tunnel.get(key).is_none(), "{key} must be stripped");
        }

        // missing config -> the Problems marker string
        let mut gone =
            serde_json::json!({"ssh_tunnel": {"enabled": true, "name": "gone", "local_port": 1}});
        let error = resolve_ssh_tunnel_into(&mut gone, &configs).unwrap_err();
        assert_eq!(error, "ssh configuration 'gone' not found");
    }
    #[test]
    fn deploy_copy_name_skips_existing_suffixes() {
        let root = temp_root("deploy_copy_name");
        let mut servers = Vec::new();
        for name in ["sftp", "sftp1", "sftp3"] {
            let mut server = blank_server(root.clone());
            server.name = name.to_string();
            servers.push(server);
        }
        assert_eq!(
            deploy_copy_name(&servers, "sftp", 1),
            ("sftp2".to_string(), 3)
        );
        assert_eq!(
            deploy_copy_name(&servers, "sftp", 3),
            ("sftp4".to_string(), 5)
        );
        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn focus_cursor_wraps_forward_and_backward() {
        let mut cursor = FocusCursor::default();
        assert_eq!(cursor.next(3), Some(1));
        assert_eq!(cursor.next(3), Some(2));
        assert_eq!(cursor.next(3), Some(0));
        assert_eq!(cursor.prev(3), Some(2));
        assert_eq!(cursor.prev(3), Some(1));
        assert_eq!(cursor.next(0), None);
        assert_eq!(cursor.prev(0), None);
    }

    #[test]
    fn focus_cursor_clamps_and_resets_on_rebuild() {
        let mut cursor = FocusCursor::default();
        assert_eq!(cursor.focus(5, 99), Some(4));
        assert_eq!(cursor.focus(5, 2), Some(2));
        assert_eq!(cursor.rebase(2, false), Some(1));
        assert_eq!(cursor.rebase(4, true), Some(0));
        assert_eq!(cursor.rebase(0, true), None);
        assert_eq!(cursor.focus(0, 3), None);

        // Entering the order from outside lands on the remembered anchor
        // (the first slot for a freshly opened dialog) instead of skipping it.
        let mut fresh = FocusCursor::default();
        assert_eq!(fresh.focus(3, fresh.active), Some(0));
    }

    #[test]
    fn deploy_focus_order_tracks_protocol_fields() {
        let sftp = deploy_focus_slots("sftp", DeployTab::Connection);
        assert_eq!(sftp.first(), Some(&DeployFocusSlot::ConnectionTab));
        assert!(sftp.contains(&DeployFocusSlot::SshConfig));
        assert!(sftp.contains(&DeployFocusSlot::Sudo));
        assert!(!sftp.contains(&DeployFocusSlot::Host));
        assert!(!sftp.contains(&DeployFocusSlot::Advanced));

        let ftp = deploy_focus_slots("ftp", DeployTab::Connection);
        assert!(ftp.contains(&DeployFocusSlot::Host));
        assert!(ftp.contains(&DeployFocusSlot::Port));
        assert!(ftp.contains(&DeployFocusSlot::PasswordReset));
        assert!(ftp.contains(&DeployFocusSlot::Advanced));
        assert!(!ftp.contains(&DeployFocusSlot::SshConfig));

        let local = deploy_focus_slots("local", DeployTab::Connection);
        assert!(local.contains(&DeployFocusSlot::LocalRoot));
        assert!(!local.contains(&DeployFocusSlot::RemoteRoot));
        assert_eq!(local.last(), Some(&DeployFocusSlot::Ok));

        for slots in [&sftp, &ftp, &local] {
            assert!(slots.contains(&DeployFocusSlot::Cancel));
            assert!(slots.contains(&DeployFocusSlot::ExcludedTab));
        }
    }

    #[test]
    fn deploy_focus_order_switches_sub_order_per_tab() {
        let connection = deploy_focus_slots("local", DeployTab::Connection);
        let mappings = deploy_focus_slots("local", DeployTab::Mappings);
        let excluded = deploy_focus_slots("local", DeployTab::Excluded);

        // First three slots are always the tab strip.
        assert_eq!(connection[3], DeployFocusSlot::Visibility);
        assert_eq!(mappings[3], DeployFocusSlot::MappingLocal);
        assert_eq!(excluded[3], DeployFocusSlot::ExcludedPaths);

        assert!(connection.contains(&DeployFocusSlot::Visibility));
        assert!(!mappings.contains(&DeployFocusSlot::Visibility));
        assert!(mappings.contains(&DeployFocusSlot::MappingRemote));
        assert!(!excluded.contains(&DeployFocusSlot::WebUrl));
    }

    #[test]
    fn data_source_focus_order_follows_active_tab() {
        let general = data_source_focus_slots("postgres", DataSourceTab::General, false, None, 0);
        assert_eq!(general[0], DataSourceFocusSlot::GeneralTab);
        assert_eq!(general[5], DataSourceFocusSlot::Name);
        assert_eq!(general[6], DataSourceFocusSlot::Driver);
        assert!(general.contains(&DataSourceFocusSlot::Database));
        assert!(general.contains(&DataSourceFocusSlot::Url));
        assert!(general.contains(&DataSourceFocusSlot::TestConnection));

        let sqlite = data_source_focus_slots("sqlite", DataSourceTab::General, false, None, 0);
        assert!(!sqlite.contains(&DataSourceFocusSlot::Host));
        assert!(!sqlite.contains(&DataSourceFocusSlot::Url));

        let ssh = data_source_focus_slots("postgres", DataSourceTab::Ssh, true, None, 0);
        // The driver picker is only on screen for the General tab.
        assert!(!ssh.contains(&DataSourceFocusSlot::Driver));
        assert!(ssh.contains(&DataSourceFocusSlot::SshToggle));
        assert!(ssh.contains(&DataSourceFocusSlot::SshConfig));
        assert!(ssh.contains(&DataSourceFocusSlot::SshLocalPort));
        assert!(!ssh.contains(&DataSourceFocusSlot::Host));

        let ssh_off = data_source_focus_slots("postgres", DataSourceTab::Ssh, false, None, 0);
        assert!(!ssh_off.contains(&DataSourceFocusSlot::SshConfig));

        // Idle / loading / failed schema states render no schema rows.
        let schemas_loading =
            data_source_focus_slots("postgres", DataSourceTab::Schemas, false, None, 0);
        assert!(!schemas_loading.contains(&DataSourceFocusSlot::SchemasAll));

        let schemas = data_source_focus_slots("postgres", DataSourceTab::Schemas, false, Some(2), 0);
        assert!(schemas.contains(&DataSourceFocusSlot::SchemasAll));
        assert!(schemas.contains(&DataSourceFocusSlot::Schema(1)));
        assert!(!schemas.contains(&DataSourceFocusSlot::Schema(2)));

        let advanced =
            data_source_focus_slots("postgres", DataSourceTab::Advanced, false, None, 2);
        assert!(advanced.contains(&DataSourceFocusSlot::AdvancedKey(1)));
        assert!(advanced.contains(&DataSourceFocusSlot::AdvancedValue(1)));
        assert!(advanced.contains(&DataSourceFocusSlot::AdvancedAdd));
        assert_eq!(advanced.last(), Some(&DataSourceFocusSlot::Ok));
    }

    #[test]
    fn ssh_focus_order_follows_auth_mode() {
        let password = ssh_focus_slots(SshAuthMode::Password, true);
        assert!(password.contains(&SshFocusSlot::Password));
        assert!(password.contains(&SshFocusSlot::SavePassword));
        assert!(!password.contains(&SshFocusSlot::KeyPath));
        assert!(password.contains(&SshFocusSlot::KeepAliveSeconds));

        let key = ssh_focus_slots(SshAuthMode::Key, false);
        assert!(key.contains(&SshFocusSlot::KeyPath));
        assert!(!key.contains(&SshFocusSlot::Password));
        assert!(!key.contains(&SshFocusSlot::KeepAliveSeconds));

        let agent = ssh_focus_slots(SshAuthMode::Agent, true);
        assert!(!agent.contains(&SshFocusSlot::Password));
        assert!(!agent.contains(&SshFocusSlot::KeyPath));
        assert_eq!(agent.first(), Some(&SshFocusSlot::Name));
        assert_eq!(agent.last(), Some(&SshFocusSlot::Ok));

        let config = ssh_focus_slots(SshAuthMode::Config, true);
        assert!(config.contains(&SshFocusSlot::Alias));
        assert!(config.contains(&SshFocusSlot::KeyPath));
        assert!(!config.contains(&SshFocusSlot::Password));
        assert_eq!(config.last(), Some(&SshFocusSlot::Ok));
    }
}
