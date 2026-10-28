use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;
use gpui::{
    Action, Anchor, App, AppContext, AsyncWindowContext, ClickEvent, Context, ElementId, Entity,
    EventEmitter, TextStyle, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, StatefulInteractiveElement, Styled, Subscription,
    WeakEntity, Window, AnyWindowHandle, SharedString, TitlebarOptions,
    WindowBounds, WindowOptions, actions, px, size,
};
use editor::{Editor, EditorElement, EditorStyle, actions as editor_actions};
use project::{Project, ProjectPath};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use file_icons::FileIcons;
use ui::{
    Button, ContextMenu, IconButtonShape, Label, LabelSize, ListItem, PopoverMenu, Tooltip,
    prelude::*,
};
use settings::Settings as _;
use util::ResultExt as _;

use workspace::dock::{DockPosition, PanelEvent};
use workspace::{OpenOptions, Panel, StatusItemView, Workspace};

actions!(
    php_panel,
    [ToggleFocus, OpenDeploymentDialog, OpenDataSourcesDialog, OpenSshConfigsDialog, BrowseRemoteHost, SelectNextTab, SelectPrevTab]
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

fn next_tab(active: PanelTab, offset: isize) -> PanelTab {
    let len = PanelTab::ALL.len() as isize;
    let index = PanelTab::ALL
        .iter()
        .position(|tab| *tab == active)
        .unwrap_or(0) as isize;
    PanelTab::ALL[((index + offset).rem_euclid(len)) as usize]
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

const DEPLOY_TYPES: [&str; 7] = ["local", "in-place", "sftp", "ftp", "ftps", "webdav", "webdavs"];

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

fn next_deploy_type(current: &str) -> &'static str {
    let canonical = deploy_type_canonical(current).unwrap_or("local");
    let position = DEPLOY_TYPES
        .iter()
        .position(|known| *known == canonical)
        .unwrap_or(0);
    DEPLOY_TYPES[(position + 1) % DEPLOY_TYPES.len()]
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
        Some("webdavs") => "WebDAV (HTTPS)".to_string(),
        _ => protocol.to_string(),
    }
}

fn deploy_default_port(protocol: &str) -> Option<&'static str> {
    match deploy_type_canonical(protocol) {
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
    Host,
    Port,
    User,
    AnonToggle,
    Password,
    Folder,
    RootPath,
    WebUrl,
    TestConnection,
    RsyncToggle,
}

fn deploy_connection_fields(protocol: &str) -> &'static [DeployField] {
    match deploy_type_canonical(protocol) {
        Some("sftp") => &[
            DeployField::SshConfig,
            DeployField::RsyncToggle,
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::TestConnection,
        ],
        Some("ftp") | Some("ftps") | Some("webdav") | Some("webdavs") => &[
            DeployField::Host,
            DeployField::Port,
            DeployField::User,
            DeployField::AnonToggle,
            DeployField::Password,
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::TestConnection,
        ],
        Some("local") => &[
            DeployField::Folder,
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::TestConnection,
        ],
        Some("in-place") => &[DeployField::WebUrl, DeployField::TestConnection],
        _ => &[
            DeployField::RootPath,
            DeployField::WebUrl,
            DeployField::TestConnection,
        ],
    }
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind {
    Database,
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
    fn new(kind: FormKind) -> Self {
        let fields = match kind {
            FormKind::Database => vec![
                String::new(),
                "sqlite".to_string(),
                String::new(),
                String::new(),
                String::new(),
            ],
            FormKind::Sql => vec![String::new()],
        };
        Self {
            kind,
            fields,
            active: 0,
            message: None,
            connection: None,
        }
    }

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
            FormKind::Database => &["name", "driver", "target", "user", "database"],
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
    ssh_menu_open: bool,
    ssh_configs: Vec<SshEntry>,
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
            ssh_menu_open: false,
            ssh_configs: Vec::new(),
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
            _subscriptions: Vec::new(),
        };
        dialog.seed_from_disk(window, cx);
        dialog
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers = load_deploy_servers(&self.root);
        self.ssh_configs = load_ssh_configs(&self.root);
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
            .or(if self.servers.is_empty() { None } else { Some(0) });
        if cx.has_global::<DeployShared>() {
            self.panel = cx.global::<DeployShared>().panel.clone();
        }
        self.update_next_number();
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
            deploy_default_port(&self.protocol).unwrap_or_default().to_string()
        } else {
            buffer.port.clone()
        };
        set(&self.port_editor, port);        set(&self.user_editor, buffer.user.clone());
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
        self.ssh_menu_open = false;
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

    fn cycle_deploy_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        self.protocol = next_deploy_type(&self.protocol).to_string();
        if let Some(index) = self.selected {
            if let Some(server) = self.servers.get_mut(index) {
                server.protocol = Some(self.protocol.clone());
            }
        }
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
        if let Err(error) = std::fs::create_dir_all(picker_path.parent().expect("picker config parent"))
            .and_then(|_| {
                std::fs::write(
                    &picker_path,
                    serde_json::to_string(&content).expect("serialize picker config"),
                )
            })
        {
            self.message = Some((format!("cannot write picker config: {error}"), Color::Error));
            cx.notify();
            return;
        }
        let local_base = match deploy_type_canonical(&server.protocol.clone().unwrap_or_default())
        {
            Some("local") => Some(match server.local_root.as_deref() {
                Some(local) if !local.trim().is_empty() => self.root.join(local.trim()),
                _ => self.root.clone(),
            }),
            _ => None,
        };
        let host_display = server.host.clone().unwrap_or_else(|| {
            local_base_display(&local_base, &self.root)
        });
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
            dialog_toggle_row(
                "deploy-advanced-passive",
                passive,
                "Passive mode",
                cx,
            )
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
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_advanced_flag(|server| &mut server.show_hidden, cx);
            }))
            .into_any_element(),
        );
        rows.push(
            dialog_row(
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
            ),
        );
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
                            .size(LabelSize::Small)
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
        self.load_editors(window, cx);
        self.schedule_validate(cx);
        cx.notify();
    }

    fn open_ssh_configs(&mut self, cx: &mut Context<Self>) {
        self.close_ssh_menu(cx);
        open_ssh_configs_window(self.root.clone(), cx);
    }

    fn run_autodetect(&mut self, window: &mut Window, cx: &mut Context<Self>) {        let Some(selected) = self.selected else {
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
        if std::fs::write(&autodetect_path, serde_json::to_string(&content).expect("serialize autodetect config")).is_err() {
            self.autodetect_status = Some(("cannot write autodetect config".to_string(), Color::Error));
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
        let written = std::fs::create_dir_all(validate_path.parent().expect("validate config parent"))
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
        let configured = read_json(&self.root.join(".zed/deploy.json"))
            .and_then(|config| {
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

fn dialog_text_style(cx: &App) -> TextStyle {
    let settings = theme_settings::ThemeSettings::get_global(cx);
    let font = settings.ui_font.clone();
    TextStyle {
        color: cx.theme().colors().text,
        font_family: font.family.clone(),
        font_features: font.features.clone(),
        font_fallbacks: font.fallbacks.clone(),
        font_size: rems(0.75).into(),
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

const DIALOG_LABEL_WIDTH: Pixels = px(110.);
const DIALOG_ROW_HEIGHT: Pixels = px(27.);
const DIALOG_CONTROL_HEIGHT: Pixels = px(24.);
const DIALOG_CONTROL_INDENT: Pixels = px(126.);

fn dialog_label(label: impl Into<SharedString>) -> Label {
    Label::new(label).size(LabelSize::Small).color(Color::Muted)
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
        .rounded_xs()
        .px_2()
        .cursor_text()
        .child(dialog_editor_element(editor, cx));
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
                .size(LabelSize::XSmall)
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
            Label::new(text)
                .size(LabelSize::XSmall)
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
    dialog_toggle_with(id, checked, label, None, cx).w_full()
}

fn dialog_toggle_row_with(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    trailing: impl IntoElement,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    dialog_toggle_with(id, checked, label, Some(trailing.into_any_element()), cx).w_full()
}

fn dialog_toggle_with(
    id: impl Into<ElementId>,
    checked: bool,
    label: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(id)
        .flex_none()
        .items_center()
        .gap_1()
        .h(DIALOG_CONTROL_HEIGHT)
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().colors().element_hover))
        .child(
            Label::new(if checked { "[x]" } else { "[ ]" })
                .size(LabelSize::Small)
                .color(if checked { Color::Accent } else { Color::Muted }),
        )
        .child(
            Label::new(label)
                .size(LabelSize::Small)
                .color(if checked { Color::Default } else { Color::Muted }),
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
            button = button.style(ButtonStyle::Subtle);
        }
        DialogButtonVariant::Link => {
            button = button
                .style(ButtonStyle::Subtle)
                .color(Color::Muted);
        }
    }
    button
        .label_size(LabelSize::Small)
        .size(ButtonSize::Default)
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
            Label::new(format!("{} ", chip))
                .size(LabelSize::XSmall)
                .color(color),
        )
        .into_any_element()
}

impl Render for DeploymentDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let protocol = self.protocol.clone();
        let message = self.message.clone();

        let mut server_rows = Vec::new();
        for (index, server) in self.servers.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            let badge = deploy_type_badge_short(
                server.protocol.as_deref().unwrap_or("server"),
            );
            let name_element: AnyElement = if is_selected {
                dialog_editor("deploy-name-editor", &self.name_editor, None, window, cx)
            } else {
                Label::new(server.name.clone())
                    .size(LabelSize::XSmall)
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
                    .child(driver_badge(badge, deploy_type_badge_color(server.protocol.as_deref().unwrap_or("server")), cx))
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
        for tab in [DeployTab::Connection, DeployTab::Mappings, DeployTab::Excluded] {
            let selected = tab == self.tab;
            tab_rows.push(
                h_flex()
                    .id(("deploy-tab", tab as usize))
                    .flex_none()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .child(
                        Label::new(tab.label())
                            .size(LabelSize::Small)
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

        let type_button = h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(dialog_badge(
                deploy_type_badge_short(&protocol),
                deploy_type_badge_color(&protocol),
                cx,
            ))
            .child(
                dialog_button(
                    "deploy-type",
                    deploy_type_display_name(&protocol),
                    DialogButtonVariant::Secondary,
                )
                .end_icon(
                    Icon::new(IconName::ChevronDown)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.cycle_deploy_type(window, cx);
                })),
            )
            .into_any_element();

        let visible_state = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.visible)
            .unwrap_or(true);
        let visible_button = dialog_button(
            "deploy-visible",
            if visible_state {
                "visible only for this project"
            } else {
                "global"
            },
            DialogButtonVariant::Secondary,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
            this.toggle_visible(cx);
        }));

        let rsync_state = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.use_rsync)
            .unwrap_or(false);
        let anonymous_state = self
            .user_editor
            .read(cx)
            .text(cx)
            .trim()
            .eq("anonymous");

        let local_folder_empty = deploy_type_canonical(&protocol) == Some("local")
            && self
                .local_root_editor
                .read(cx)
                .text(cx)
                .trim()
                .is_empty();

        let mut connection_rows: Vec<AnyElement> = Vec::new();
        for field in deploy_connection_fields(&protocol) {
            connection_rows.push(match field {
                DeployField::SshConfig => {
                    let mut rows: Vec<AnyElement> = Vec::new();
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
                                        if self.ssh_config_name.is_empty() {
                                            "<select configuration>"
                                        } else {
                                            self.ssh_config_name.as_str()
                                        },
                                        if self.ssh_config_name.is_empty() {
                                            DialogButtonVariant::Link
                                        } else {
                                            DialogButtonVariant::Primary
                                        },
                                    )
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
                                        .icon_size(IconSize::XSmall)
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
                                            .size(LabelSize::XSmall)
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(ssh_user_host_text(config))
                                            .size(LabelSize::XSmall)
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
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.open_ssh_configs(cx);
                                }))
                                .into_any_element(),
                        );
                    }
                    h_flex()
                        .w_full()
                        .flex_none()
                        .flex_col()
                        .gap_0p5()
                        .children(rows)
                        .into_any_element()
                }
                DeployField::Host => dialog_row(
                    "Host:",
                    dialog_editor("deploy-host-editor", &self.host_editor, None, window, cx),
                ),
                DeployField::Port => dialog_row(
                    "Port:",
                    dialog_editor("deploy-port-editor", &self.port_editor, Some(px(60.)), window, cx),
                ),
                DeployField::User => dialog_row(
                    "Username:",
                    dialog_editor("deploy-user-editor", &self.user_editor, None, window, cx),
                ),
                DeployField::AnonToggle => dialog_toggle_row(
                    "deploy-anon",
                    anonymous_state,
                    "Login as anonymous",
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.toggle_anonymous(window, cx);
                }))
                .into_any_element(),
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
                            .child(
                                dialog_button(
                                    "deploy-save-password",
                                    format!("Save: {}", self.save_password.label()),
                                    DialogButtonVariant::Secondary,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.cycle_save_password(cx);
                                })),
                            )
                            .child(dialog_static(self.save_password.hint(), Color::Muted))
                            .child(
                                dialog_button(
                                    "deploy-reset-password",
                                    "Reset",
                                    DialogButtonVariant::Secondary,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.reset_password(window, cx);
                                })),
                            ),
                    )
                    .into_any_element(),
                DeployField::Folder => dialog_row(
                    "Folder:",
                    dialog_editor(
                        "deploy-local-root-editor",
                        &self.local_root_editor,
                        None,
                        window,
                        cx,
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
                                dialog_button(
                                    "deploy-autodetect",
                                    "Autodetect",
                                    DialogButtonVariant::Secondary,
                                )
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
                    dialog_editor("deploy-url-editor", &self.web_url_editor, None, window, cx),
                ),
                DeployField::TestConnection => {
                    let test_button = dialog_button(
                        "deploy-test",
                        "Test Connection",
                        DialogButtonVariant::Link,
                    )
                    .disabled(local_folder_empty)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.test_connection(cx);
                    }));
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
                                    .size(LabelSize::Small)
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
                DeployField::RsyncToggle => dialog_toggle_row(
                    "deploy-rsync",
                    rsync_state,
                    "Use rsync for download/upload/sync",
                    cx,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.toggle_rsync(cx);
                }))
                .into_any_element(),
            });
        }

        let tab_content: AnyElement = match self.tab {
            DeployTab::Connection => v_flex()
                .id("deploy-tab-connection")
                .w_full()
                .children({
                    let mut rows: Vec<AnyElement> = Vec::new();
                    rows.push(dialog_row(
                        "Type:",
                        type_button,
                    ));
                    rows.push(dialog_row(
                        "Visibility:",
                        visible_button,
                    ));
                    rows.extend(connection_rows);
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
                            .hover(|style| style.bg(cx.theme().colors().element_hover))
                            .child(
                                Label::new(if self.advanced_open {
                                    "▾ Advanced"
                                } else {
                                    "▸ Advanced"
                                })
                                .size(LabelSize::Small)
                                .color(Color::Default),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_advanced(cx);
                            }))
                            .into_any_element(),
                    );
                    if self.advanced_open {
                        if deploy_type_has_advanced(&self.protocol) {
                            let selected = self
                                .selected
                                .and_then(|index| self.servers.get(index))
                                .cloned();
                            rows.extend(self.render_advanced_rows(selected.as_ref(), window, cx));
                        } else {
                            rows.push(dialog_static("advanced: none", Color::Muted));
                        }
                    }
                    rows
                })
                .into_any_element(),
            DeployTab::Mappings => v_flex()
                .id("deploy-tab-mappings")
                .w_full()
                .children(vec![
                    dialog_row(
                        "Local path:",
                        dialog_editor(
                            "deploy-local-root-editor",
                            &self.local_root_editor,
                            None,
                            window,
                            cx,
                        ),
                    ),
                    dialog_row(
                        "Deployment path:",
                        dialog_editor(
                            "deploy-deploy-path-editor",
                            &self.remote_root_editor,
                            None,
                            window,
                            cx,
                        ),
                    ),
                    dialog_row(
                        "Web path:",
                        dialog_editor(
                            "deploy-web-path-editor",
                            &self.web_url_editor,
                            None,
                            window,
                            cx,
                        ),
                    ),
                ])
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
                        .size(LabelSize::XSmall)
                        .color(*color)
                        .truncate(),
                )
                    .into_any_element()
        });

        let selected_name = self
            .selected
            .and_then(|index| self.servers.get(index))
            .map(|server| server.name.clone());
        let warning_lines = self
            .validate_warnings
            .iter()
            .filter(|(name, _)| name.is_empty() || Some(name) == selected_name.as_ref())
            .flat_map(|(_, warnings)| warnings.iter())
            .map(|warning| {
                h_flex()
                    .flex_none()
                    .px_3()
                    .py_0p5()
                    .child(
                        Label::new(warning.clone())
                            .size(LabelSize::XSmall)
                            .color(Color::Warning)
                            .truncate(),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        v_flex()
            .id("deploy-dialog")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
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
                            .size(LabelSize::Small)
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
                            .w(px(150.))
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
                                        IconButton::new("deploy-add", IconName::Plus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::XSmall)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.add_server(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("deploy-remove", IconName::SquareMinus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::XSmall)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.remove_server(window, cx);
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
            .children(warning_lines)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        dialog_button(
                            "deploy-cancel",
                            "Cancel",
                            DialogButtonVariant::Secondary,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.cancel(window, cx);
                        })),
                    )
                    .child(
                        dialog_button("deploy-ok", "OK", DialogButtonVariant::Primary).on_click(
                            cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            }),
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

    let window_bounds = WindowBounds::centered(size(px(660.), px(460.)), cx);
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
    source.read_only = value.get("read_only").and_then(Value::as_bool).unwrap_or(false);
    if let Some(tunnel) = value.get("ssh_tunnel") {
        source.ssh_enabled = tunnel.get("enabled").and_then(Value::as_bool).unwrap_or(false);
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
    source.options = value
        .get("options")
        .and_then(Value::as_object)
        .map(options_to_rows)
        .unwrap_or_default();
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
    if !source.options.is_empty() {
        entry.insert(
            "options".to_string(),
            Value::Object(rows_to_options(&source.options)),
        );
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
        let merged = match merged { Value::Object(map) => map, _ => serde_json::Map::new() };
        connections.push(Value::Object(merged));
    }
    config.insert("connections".to_string(), Value::Array(connections));
    write_config_object(&path, &config)
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
}

impl SshAuthMode {
    fn label(self) -> &'static str {
        match self {
            SshAuthMode::Password => "Password",
            SshAuthMode::Key => "Key pair",
            SshAuthMode::Agent => "OpenSSH config and authentication agent",
        }
    }

    fn key(self) -> &'static str {
        match self {
            SshAuthMode::Password => "password",
            SshAuthMode::Key => "key",
            SshAuthMode::Agent => "agent",
        }
    }

    fn parse(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "key" | "key pair" => SshAuthMode::Key,
            "agent" | "openssh" => SshAuthMode::Agent,
            _ => SshAuthMode::Password,
        }
    }
}

fn next_ssh_auth(current: SshAuthMode) -> SshAuthMode {
    match current {
        SshAuthMode::Password => SshAuthMode::Key,
        SshAuthMode::Key => SshAuthMode::Agent,
        SshAuthMode::Agent => SshAuthMode::Password,
    }
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
            keep_alive: 15,
            strict_host_key: false,
        }
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
    entry.insert("auth".to_string(), Value::String(config.auth.key().to_string()));
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
    while configs
        .iter()
        .any(|config| config.name.trim() == name)
    {
        index += 1;
        name = format!("{base} ({index})");
    }
    name
}

fn ssh_apply_auth_to_tunnel(
    tunnel: &mut serde_json::Map<String, Value>,
    config: &SshEntry,
) {
    tunnel.insert("auth".to_string(), Value::String(config.auth.key().to_string()));
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

fn ssh_apply_auth_to_server(
    server: &mut serde_json::Map<String, Value>,
    config: &SshEntry,
) {
    server.insert(
        "ssh_auth".to_string(),
        Value::String(config.auth.key().to_string()),
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
    server.insert(
        "ssh_keep_alive".to_string(),
        Value::from(config.keep_alive),
    );
}

fn resolve_ssh_into(
    server: &mut Value,
    configs: &[SshEntry],
) -> std::result::Result<(), String> {
    let Some(name) = string_field(server, "ssh_config").filter(|name| !name.is_empty()) else {
        return Ok(());
    };
    let Some(config) = configs.iter().find(|config| config.name == name) else {
        return Err(format!("ssh configuration '{name}' not found"));
    };
    let Some(map) = server.as_object_mut() else {
        return Ok(());
    };
    map.insert("host".to_string(), Value::String(config.host.trim().to_string()));
    map.insert("user".to_string(), Value::String(config.user.trim().to_string()));
    map.insert("port".to_string(), Value::from(ssh_entry_port(config)));
    ssh_apply_auth_to_server(map, config);
    Ok(())
}

fn resolve_ssh_tunnel_into(
    connection: &mut Value,
    configs: &[SshEntry],
) -> std::result::Result<(), String> {
    let Some(Value::Object(tunnel)) = connection.get_mut("ssh_tunnel") else {
        return Ok(());
    };
    let enabled = tunnel.get("enabled").and_then(Value::as_bool).unwrap_or(false);
    if !enabled {
        return Ok(());
    }
    let Some(name) = tunnel
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
    else {
        return Ok(());
    };
    let Some(config) = configs.iter().find(|config| config.name == name) else {
        return Err(format!("ssh configuration '{name}' not found"));
    };
    // the marker is transient: php-db denies unknown fields, so strip it now
    // that the resolved host/user/port/auth keys carry the connection data
    tunnel.remove("name");
    tunnel.insert("host".to_string(), Value::String(config.host.trim().to_string()));
    tunnel.insert("user".to_string(), Value::String(config.user.trim().to_string()));
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
    advanced_rows: Vec<(Entity<Editor>, Entity<Editor>)>,
}

impl Focusable for DataSourcesDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_editor.focus_handle(cx)
    }
}

impl DataSourcesDialog {
    fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            advanced_rows: Vec::new(),
        };
        dialog.seed_from_disk(window, cx);
        dialog
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sources = load_data_sources(&self.root);
        self.ssh_configs = load_ssh_configs(&self.root);
        self.restore_ssh_config_names();
        self.ssh_menu_open = false;
        self.selected = if self.sources.is_empty() {
            None
        } else {
            Some(0)
        };
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

    fn resume_with_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.root = root;
        self.seed_from_disk(window, cx);
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
        set(&self.ssh_local_port_editor, source.ssh_local_port.clone());
        self.ssh_local_port_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text("<Dynamic>", window, cx);
        });        self.advanced_rows = source
            .options
            .iter()
            .map(|(key, value)| {
                let key_editor = cx.new(|cx| Editor::single_line(window, cx));
                key_editor.update(cx, |editor, cx| {
                    editor.set_text(key.clone(), window, cx)
                });
                let value_editor = cx.new(|cx| Editor::single_line(window, cx));
                value_editor.update(cx, |editor, cx| {
                    editor.set_text(value.clone(), window, cx)
                });
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

    fn toggle_driver_menu(&mut self, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        self.driver_menu_open = !self.driver_menu_open;
        cx.notify();
    }

    fn switch_driver(&mut self, driver: String, window: &mut Window, cx: &mut Context<Self>) {
        self.driver_menu_open = false;
        let Some(index) = self.selected else {
            return;
        };
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
        source.ssh_user = config.user.trim().to_string();
        source.ssh_host = config.host.trim().to_string();
        source.ssh_port = ssh_entry_port(&config).to_string();
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
            if !source.ssh_enabled || !source.ssh_name.trim().is_empty() {
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
    save_password: SavePassword,
    keep_alive_enabled: bool,
    strict: bool,
    name_editor: Entity<Editor>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    user_editor: Entity<Editor>,
    password_editor: Entity<Editor>,
    key_path_editor: Entity<Editor>,
    keep_alive_editor: Entity<Editor>,
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
            save_password: SavePassword::Forever,
            keep_alive_enabled: true,
            strict: false,
            name_editor: cx.new(|cx| Editor::single_line(window, cx)),
            host_editor: cx.new(|cx| Editor::single_line(window, cx)),
            port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            user_editor: cx.new(|cx| Editor::single_line(window, cx)),
            password_editor,
            key_path_editor: cx.new(|cx| Editor::single_line(window, cx)),
            keep_alive_editor: cx.new(|cx| Editor::single_line(window, cx)),
        };
        dialog.seed_from_disk(window, cx);
        dialog
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
        self.save_password = if config.password_session {
            SavePassword::Session
        } else {
            SavePassword::Forever
        };
        self.keep_alive_enabled = config.keep_alive > 0;
        self.strict = config.strict_host_key;
        let mut set = |editor: &Entity<Editor>, value: String| {
            editor.update(cx, |editor, cx| editor.set_text(value, window, cx));
        };
        set(&self.name_editor, config.name.clone());
        set(&self.host_editor, config.host.clone());
        set(&self.port_editor, config.port.clone());
        set(&self.user_editor, config.user.clone());
        set(&self.password_editor, config.password.clone());
        set(&self.key_path_editor, config.key_path.clone());
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
        config.password_session = self.save_password == SavePassword::Session;
        config.strict_host_key = self.strict;
        config.keep_alive = if self.keep_alive_enabled {
            text(&self.keep_alive_editor).trim().parse::<u16>().unwrap_or(15)
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

    fn cycle_auth(&mut self, cx: &mut Context<Self>) {
        self.stage_current(cx);
        self.auth = next_ssh_auth(self.auth);
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

        let mut config_rows = Vec::new();
        for (index, config) in self.configs.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            config_rows.push(
                h_flex()
                    .id(("ssh-config-row", index))
                    .w_full()
                    .flex_none()
                    .px_1()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(colors.element_selected))
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        Label::new(config.name.clone())
                            .size(LabelSize::XSmall)
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

        let auth_button = dialog_button("ssh-auth-type", self.auth.label(), DialogButtonVariant::Secondary)
            .end_icon(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.cycle_auth(cx);
            }));

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
                .child(dialog_editor("ssh-host-editor", &self.host_editor, None, window, cx))
                .child(
                    div().flex_none().w(px(35.)).child(
                        dialog_label("Port:"),
                    ),
                )
                .child(dialog_editor("ssh-port-editor", &self.port_editor, Some(px(60.)), window, cx)),
        ));
        rows.push(dialog_row(
            "Username:",
            dialog_editor("ssh-user-editor", &self.user_editor, None, window, cx),
        ));
        rows.push(dialog_row("Authentication type:", auth_button));
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
                            dialog_editor("ssh-password-editor", &self.password_editor, None, window, cx),
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
                                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                        this.cycle_save_password(cx);
                                    })),
                                )
                                .child(dialog_static(self.save_password.hint(), Color::Muted))
                                .child(
                                    dialog_button(
                                        "ssh-reset-password",
                                        "Reset",
                                        DialogButtonVariant::Secondary,
                                    )
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.reset_password(window, cx);
                                    })),
                                ),
                        )
                        .into_any_element(),
                );
            }
            SshAuthMode::Key => {
                rows.push(dialog_row(
                    "Private key file:",
                    dialog_editor("ssh-key-path-editor", &self.key_path_editor, None, window, cx),
                ));
            }
            SshAuthMode::Agent => {}
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
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                cx,
            )
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
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_strict(cx);
            }))
            .into_any_element(),
        );
        rows.push(dialog_static("proxy: global settings only", Color::Muted));

        let test_button = dialog_button("ssh-test", "Test Connection", DialogButtonVariant::Link)
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.test_connection(cx);
            }));

        v_flex()
            .id("ssh-dialog")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
            }))
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
                                            .icon_size(IconSize::XSmall)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, window, cx| {
                                                    this.add_config(window, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        IconButton::new("ssh-remove", IconName::SquareMinus)
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::XSmall)
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
                            .size(LabelSize::XSmall)
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
                                    .size(LabelSize::Small)
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
                                dialog_button(
                                    "ssh-cancel",
                                    "Cancel",
                                    DialogButtonVariant::Secondary,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel(window, cx);
                                })),
                            )
                            .child(
                                dialog_button("ssh-apply", "Apply", DialogButtonVariant::Secondary)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.apply(window, cx);
                                    })),
                            )
                            .child(
                                dialog_button("ssh-ok", "OK", DialogButtonVariant::Primary)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.confirm(window, cx);
                                    })),
                            ),
                    ),
            )
    }
}

fn ssh_probe_command(
    config: &SshEntry,
) -> std::result::Result<util::command::Command, String> {
    let port = config.port.trim().to_string();
    let port = if port.is_empty() { "22".to_string() } else { port };
    if !port.chars().all(|character| character.is_ascii_digit())
        || port.parse::<u16>().is_err()
    {
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

    let window_bounds = WindowBounds::centered(size(px(700.), px(480.)), cx);
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
        };
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
                            let is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "ls command failed".to_string()),
                    )
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
                .px_2()
                .py_0p5()
                .rounded_sm()
                .cursor_pointer()
                .when(self.selected == Some(".".to_string()), |this| {
                    this.bg(colors.element_selected)
                })
                .hover(|style| style.bg(colors.element_hover))
                .child(
                    Label::new(if self.expanded.contains(".") { "▾" } else { "▸" })
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(
                    Label::new(self.root_label())
                        .size(LabelSize::XSmall)
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
                                        .size(LabelSize::XSmall)
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
                                .size(LabelSize::XSmall)
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
                            .size(LabelSize::XSmall)
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
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .child(
                        div()
                            .id("rp-query-box")
                            .flex_1()
                            .min_w_0()
                            .bg(colors.editor_background)
                            .border_1()
                            .border_color(colors.border_variant)
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
                        Button::new("rp-cancel", "Cancel")
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Small)
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.cancel(window, cx);
                            })),
                    )
                    .child(
                        Button::new("rp-ok", "OK")
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Small)
                            .style(ButtonStyle::Filled)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.confirm(window, cx);
                            })),
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
            let descendant_matches = query.trim().is_empty() || match children.as_ref() {
                Some(Ok(children)) => {
                    expanded
                        && (children
                            .iter()
                            .any(|child| picker_query_matches(query, &child.name))
                            || children.iter().any(|child| {
                                child.is_directory && self.descendant_matches(query, &child.path)
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
                                    .size(LabelSize::XSmall)
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
                        .size(IconSize::XSmall)
                        .color(Color::Accent),
                    );
            } else {
                item = item
                    .child(div().flex_none().w(px(14.)))
                    .child(
                        Icon::new(IconName::File)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    );
            }
            item = item.child(
                Label::new(entry.name.clone())
                    .size(LabelSize::XSmall)
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
        children.iter().any(|child| picker_query_matches(query, &child.name))
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
    let window_bounds = WindowBounds::centered(size(px(560.), px(520.)), cx);
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
    if !cx.has_global::<DataSourcesDialogWindow>() {
        cx.set_global(DataSourcesDialogWindow(None));
    }
    let existing = cx.global::<DataSourcesDialogWindow>().0;
    if let Some(handle) = existing.and_then(|any| any.downcast::<DataSourcesDialog>()) {
        let resumed = handle.update(cx, |dialog, window, cx| {
            dialog.resume_with_root(root.clone(), window, cx);
            window.activate_window();
            cx.notify();
        });
        if resumed.is_ok() {
            return;
        }
        cx.global_mut::<DataSourcesDialogWindow>().0 = None;
    }

    let window_bounds = WindowBounds::centered(size(px(760.), px(720.)), cx);
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
                let view = cx.new(|cx| DataSourcesDialog::new(root.clone(), window, cx));
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
                    .child(driver_badge(
                        driver_badge_short(&source.driver),
                        driver_badge_color(&source.driver),
                        cx,
                    ))
                    .child(
                        Label::new(source.name.clone())
                            .size(LabelSize::XSmall)
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
                            .size(LabelSize::XSmall)
                            .color(Color::Error)
                            .truncate(),
                    )
                    .into_any_element(),
            );
        }

        let mut tab_rows = Vec::new();
        for tab in DataSourceTab::ALL {
            let selected = tab == self.tab;
            tab_rows.push(
                h_flex()
                    .id(("ds-tab", tab as usize))
                    .flex_none()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(selected, |this| {
                        this.border_b_2().border_color(colors.border_focused)
                    })
                    .child(
                        Label::new(tab.label())
                            .size(LabelSize::Small)
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

                rows.push(
                    dialog_row(
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
                            .hover(|style| style.bg(colors.element_hover))
                            .child(driver_badge(
                                driver_badge_short(&driver),
                                driver_badge_color(&driver),
                                cx,
                            ))
                            .child(
                                Label::new(driver_display_name(&driver))
                                    .size(LabelSize::Small)
                                    .truncate(),
                            )
                            .child(
                                Label::new(driver_support_hint(&driver))
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(div().flex_1().min_w_0())
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(IconSize::XSmall)
                                    .color(Color::Muted),
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_driver_menu(cx);
                            })),
                    ),
                );

                if self.driver_menu_open && selected_source.is_some() {
                    let mut menu_rows = Vec::new();
                    for (menu_index, menu_driver) in DATABASE_DRIVERS.iter().enumerate() {
                        let is_current = *menu_driver == driver;
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
                                        .size(LabelSize::Small)
                                        .color(if is_current {
                                            Color::Accent
                                        } else {
                                            Color::Default
                                        })
                                        .truncate(),
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, window, cx| {
                                        this.switch_driver(menu_driver.to_string(), window, cx);
                                    },
                                ))
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
                        .size(LabelSize::Small)
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
                            dialog_editor("ds-port-editor", &self.port_editor, Some(px(60.)), window, cx),
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
                    .size(LabelSize::Small)
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
                                            .size(LabelSize::Small)
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
                                if read_only {
                                    "read-only"
                                } else {
                                    "read-write"
                                },
                                DialogButtonVariant::Secondary,
                            )
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_read_only(cx);
                            })),
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
                    dialog_toggle_row(
                        "ds-ssh-toggle",
                        ssh_enabled,
                        "Use SSH tunnel",
                        cx,
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.toggle_ssh(cx);
                    }))
                    .into_any_element(),
                );
                if ssh_enabled {
                    let config_name = selected_source
                        .as_ref()
                        .map(|source| source.ssh_name.trim().to_string())
                        .unwrap_or_default();
                    rows.push(
                        dialog_row(
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
                                            "<select configuration>"
                                        } else {
                                            config_name.as_str()
                                        },
                                        if config_name.is_empty() {
                                            DialogButtonVariant::Link
                                        } else {
                                            DialogButtonVariant::Primary
                                        },
                                    )
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
                                        .icon_size(IconSize::XSmall)
                                        .icon_color(Color::Muted)
                                        .tooltip(Tooltip::text("Edit SSH configurations"))
                                        .on_click(cx.listener(
                                            |this, _: &ClickEvent, window, cx| {
                                                this.open_ssh_configs(window, cx);
                                            },
                                        )),
                                ),
                        ),
                    );
                    if self.ssh_menu_open {
                        for (index, config) in self.ssh_configs.iter().enumerate() {
                            let is_active = config_name == config.name;
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
                                            .size(LabelSize::XSmall)
                                            .color(if is_active {
                                                Color::Accent
                                            } else {
                                                Color::Default
                                            })
                                            .truncate(),
                                    )
                                    .child(
                                        Label::new(ssh_user_host_text(config))
                                            .size(LabelSize::XSmall)
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
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.open_ssh_configs(window, cx);
                                    },
                                ))
                                .into_any_element(),
                        );
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
                                .icon_size(IconSize::XSmall)
                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                    this.refresh_schemas(cx);
                                })),
                        )
                        .child(
                            Label::new("auto")
                                .size(LabelSize::XSmall)
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
                                    .size(LabelSize::XSmall)
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
                                    .size(LabelSize::XSmall)
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
                                    .size(LabelSize::XSmall)
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
                            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                this.toggle_all_schemas(cx);
                            }))
                            .into_any_element(),
                        );
                        for (index, name) in names.iter().enumerate() {
                            let checked = schema_checked(&excluded, name);
                            let name = name.clone();
                            rows.push(
                                dialog_toggle_row(
                                    ("ds-schema-row", index),
                                    checked,
                                    name.clone(),
                                    cx,
                                )
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _window, cx| {
                                        this.toggle_schema(name.clone(), cx);
                                    },
                                ))
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
                                .icon_size(IconSize::XSmall)
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.add_advanced_row(window, cx);
                                    },
                                )),
                        )
                        .child(
                            IconButton::new("ds-advanced-remove", IconName::SquareMinus)
                                .shape(IconButtonShape::Square)
                                .icon_size(IconSize::XSmall)
                                .on_click(cx.listener(
                                    |this, _: &ClickEvent, window, cx| {
                                        this.remove_advanced_row(window, cx);
                                    },
                                )),
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

        let test_button = dialog_button("ds-test", "Test Connection", DialogButtonVariant::Link)
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.test_connection(cx);
            }));

        let message_line: Option<AnyElement> = message.as_ref().map(|(text, color)| {
            h_flex()
                .flex_none()
                .px_3()
                .py_1()
                .child(
                    Label::new(text.clone())
                        .size(LabelSize::Small)
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
            .on_action(cx.listener(|this, _: &editor_actions::Cancel, window, cx| {
                this.cancel(window, cx);
            }))
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
                                                    .size(IconSize::XSmall)
                                                    .color(Color::Muted),
                                            )
                                            .child(
                                                Label::new("Data Sources")
                                                    .size(LabelSize::Small)
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
                                                    .icon_size(IconSize::XSmall)
                                                    .on_click(cx.listener(
                                                        |this, _: &ClickEvent, window, cx| {
                                                            this.add_source(window, cx);
                                                        },
                                                    )),
                                            )
                                            .child(
                                                IconButton::new(
                                                    "ds-remove",
                                                    IconName::SquareMinus,
                                                )
                                                .shape(IconButtonShape::Square)
                                                .icon_size(IconSize::XSmall)
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, window, cx| {
                                                        this.remove_source(window, cx);
                                                    },
                                                )),
                                            )
                                            .child(
                                                IconButton::new("ds-copy", IconName::Copy)
                                                    .shape(IconButtonShape::Square)
                                                    .icon_size(IconSize::XSmall)
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
                                            .size(LabelSize::XSmall)
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
                                    .py_1()
                                    .border_b_1()
                                    .border_color(colors.border)
                                    .child(
                                        Label::new("Name:")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(dialog_editor(
                                        "ds-name-editor",
                                        &self.name_editor,
                                        None,
                                        window,
                                        cx,
                                    ))
                                    .child(
                                        Label::new("Color")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    ),
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
                                    .size(LabelSize::Small)
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
                                dialog_button(
                                    "ds-cancel",
                                    "Cancel",
                                    DialogButtonVariant::Secondary,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.cancel(window, cx);
                                })),
                            )
                            .child(
                                dialog_button(
                                    "ds-apply",
                                    "Apply",
                                    DialogButtonVariant::Secondary,
                                )
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.apply(window, cx);
                                })),
                            )
                            .child(
                                dialog_button("ds-ok", "OK", DialogButtonVariant::Primary)
                                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                        this.confirm(window, cx);
                                    })),
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

#[derive(Clone)]
struct DatabaseColumn {
    name: String,
    column_type: String,
    primary_key: bool,
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
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid JSON from php-test".to_string())
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
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid JSON from php-test".to_string())
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

    Ok(TestRunResult { tests: cases, summary })
}

fn first_non_empty_line<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    lines
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

pub struct PhpPanel {
    focus_handle: FocusHandle,
    workspace: WeakEntity<Workspace>,
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
    database_check_status: HashMap<String, SyncStatus>,
    table_status: HashMap<(String, String), TableStatus>,
    query_status: HashMap<String, TableStatus>,
    expanded_tables: HashSet<(String, String)>,
    db_expanded_conns: HashSet<String>,
    db_expanded_views: HashSet<String>,
    db_expanded_names: HashSet<String>,
    db_names_status: HashMap<String, DbListStatus>,
    database_views_status: HashMap<String, DatabaseStatus>,
    db_menu_open: bool,
    http_status: HashMap<String, HttpStatus>,
    test_list_status: TestListStatus,
    test_files: Vec<String>,
    test_run: Option<TestRunState>,
    test_active_file: Option<String>,
    form: Option<FormState>,
    server_menu_open: bool,
    saved_message: Option<String>,
    _subscriptions: Vec<Subscription>,
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
            workspace.register_action(
                |workspace, _: &BrowseRemoteHost, window, cx| {
                    if cx.has_global::<DeployShared>() {
                        if let Some(panel) = cx.global::<DeployShared>().panel.clone() {
                            panel
                                .update(cx, |panel, cx| panel.select_tab(PanelTab::Remote, cx))
                                .ok();
                        }
                    }
                    workspace.toggle_panel_focus::<PhpPanel>(window, cx);
                },
            );
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
                    databases: Vec::new(),
                    sync_status: HashMap::new(),
                    database_status: HashMap::new(),
                    database_check_status: HashMap::new(),
                    table_status: HashMap::new(),
                    query_status: HashMap::new(),
                    expanded_tables: HashSet::new(),
                    db_expanded_conns: HashSet::new(),
                    db_expanded_views: HashSet::new(),
                    db_expanded_names: HashSet::new(),
                    db_names_status: HashMap::new(),
                    database_views_status: HashMap::new(),
                    db_menu_open: false,
                    http_status: HashMap::new(),
                    test_list_status: TestListStatus::Unloaded,
                    test_files: Vec::new(),
                    test_run: None,
                    test_active_file: None,
                    form: None,
                    server_menu_open: false,
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
                            target: string_field(connection, "path")
                                .or_else(|| string_field(connection, "url"))
                                .or_else(|| {
                                    let host = string_field(connection, "host")?;
                                    Some(
                                        match connection.get("port").and_then(Value::as_u64) {
                                            Some(port) => format!("{host}:{port}"),
                                            None => host,
                                        },
                                    )
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
        self.active_server =
            resolve_active_server(configured_active, configured_default, &names);

        cx.set_global(DeployShared {
            servers: names.clone(),
            active: self.active_server.clone(),
            panel: Some(cx.weak_entity()),
            root: self.remote_servers.first().map(|server| server.root.clone()),
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

    fn refresh_databases(&mut self, cx: &mut Context<Self>) {
        self.database_status.clear();
        self.database_check_status.clear();
        self.database_views_status.clear();
        self.db_names_status.clear();
        self.table_status.clear();
        self.expanded_tables.clear();
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
        self.remote_status.insert(key.clone(), RemoteStatus::Loading);
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "ls command failed".to_string()),
                    )
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
                        this.remote_status.insert(key, RemoteStatus::Failed(message));
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
        self.remote_status.insert(key.clone(), RemoteStatus::Loading);
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "download failed".to_string()),
                    )
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
                                    .open_abs_path(
                                        target,
                                        OpenOptions::default(),
                                        window,
                                        cx,
                                    )
                                    .detach_and_log_err(cx);
                            })
                            .log_err();
                    }
                    Err(message) => {
                        this.remote_status.insert(key, RemoteStatus::Failed(message));
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
                .size(IconSize::XSmall)
                .color(Color::Accent)
            } else {
                match FileIcons::get_icon(Path::new(&entry.name), cx) {
                    Some(icon_path) => Icon::from_path(icon_path).size(IconSize::XSmall),
                    None => Icon::new(IconName::File).size(IconSize::XSmall),
                }
            };

            let mut item = ListItem::new(("remote-entry", *line))
                .indent_level(depth + 1)
                .selectable(true)
                .toggle_state(selected)
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
                        .size(LabelSize::XSmall)
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
                        this.toggle_remote_dir(
                            server_name.clone(),
                            path.clone(),
                            root.clone(),
                            cx,
                        );
                    }
                } else if event.click_count() >= 2 {
                    this.download_remote_file(
                        server_name.clone(),
                        path.clone(),
                        root.clone(),
                        cx,
                    );
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "request failed".to_string()),
                    )
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

    fn open_form(&mut self, kind: FormKind, window: &mut Window, cx: &mut Context<Self>) {
        self.form = Some(FormState::new(kind));
        self.saved_message = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn open_sql_form(
        &mut self,
        connection: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.form = Some(FormState::sql(connection));
        self.saved_message = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn handle_form_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let outcome = self.form.as_mut().map(|form| {
            form.handle_key(
                &event.keystroke.key,
                event.keystroke.key_char.as_deref(),
            )
        });
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
        let kind = form.kind;
        let fields = form.fields.clone();
        let connection = form.connection.clone();
        let Some(root) = self.worktree_root(cx) else {
            if let Some(form) = self.form.as_mut() {
                form.message = Some("no open worktree".to_string());
            }
            cx.notify();
            return;
        };

        if kind == FormKind::Sql {
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
            return;
        }

        if kind == FormKind::Sql {
            return;
        }
        let saved = save_database_config(&root, &fields);

        match saved {
            Ok(name) => {
                self.form = None;
                self.saved_message = Some("saved".to_string());
                self.refresh(cx);
                match kind {
                    FormKind::Database => self.run_database_check(name, root, cx),
                    FormKind::Sql => {}
                }
                cx.notify();
            }
            Err(message) => {
                if let Some(form) = self.form.as_mut() {
                    form.message = Some(message);
                }
                cx.notify();
            }
        }
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

    fn run_database_check(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        self.database_check_status
            .insert(name.clone(), SyncStatus::Syncing);
        cx.notify();

        let task_name = name.clone();
        let task = cx.background_spawn(async move {
            let mut command = util::command::new_command("php-db");
            command.arg("check").arg(&task_name).current_dir(&root);
            command_result(command.output().await, "check failed")
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let status = match result {
                    Ok(()) => SyncStatus::Ok,
                    Err(message) => SyncStatus::Failed(message),
                };
                this.database_check_status.insert(name, status);
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "schema command failed".to_string()),
                    )
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

    fn open_database_form(
        &mut self,
        driver: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_form(FormKind::Database, window, cx);
        if let Some(driver) = driver {
            if let Some(form) = self.form.as_mut() {
                form.fields[1] = driver.to_string();
                form.fields[0] = next_connection_name(driver, &self.databases);
            }
        }
        self.db_menu_open = false;
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "databases command failed".to_string()),
                    )
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
                    Err(
                        first_non_empty_line(stderr.lines().chain(stdout.lines()))
                            .unwrap_or_else(|| "views command failed".to_string()),
                    )
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

    fn toggle_table_preview(
        &mut self,
        connection: String,
        table: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let key = (connection.clone(), table.clone());
        if self.expanded_tables.contains(&key) {
            self.expanded_tables.remove(&key);
            cx.notify();
            return;
        }

        self.expanded_tables.insert(key.clone());
        if matches!(self.table_status.get(&key), Some(TableStatus::Loading)) {
            cx.notify();
            return;
        }
        self.load_table_preview(connection, table, root, cx);
    }

    fn load_table_preview(
        &mut self,
        connection: String,
        table: String,
        root: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let key = (connection.clone(), table.clone());
        self.table_status.insert(key.clone(), TableStatus::Loading);
        cx.notify();

        let sql = format!("select * from \"{}\" limit 20", table.replace('"', "\"\""));
        let task = cx.background_spawn(async move {
            let output = std::process::Command::new("php-db")
                .arg("query")
                .arg(&connection)
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
                this.table_status.insert(key, status);
                cx.notify();
            })
            .log_err();
        })
        .detach();
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
    entry.insert("visible".to_string(), Value::Bool(server.visible));
    if deploy_type_has_advanced(protocol) {
        entry.insert(
            "passive_mode".to_string(),
            Value::Bool(server.passive_mode),
        );
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
    for key in ["root", "path", "remote_root", "directory", "found", "listing"] {
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
    RemoteServer {
        name: String::new(),
        protocol: Some("local".to_string()),
        host: None,
        port: None,
        user: None,
        password: None,
        password_session: false,
        ssh_config: None,
        remote_root: None,
        local_root: None,
        web_url: None,
        excluded: Vec::new(),
        upload_on_save: false,
        use_rsync: false,
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
    server.web_url = if web_url.is_empty() { None } else { Some(web_url) };
}

fn apply_excluded_field(server: &mut RemoteServer, fields: &[String]) {
    server.excluded =
        parse_excluded(fields.first().map(String::as_str).unwrap_or_default());
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

fn save_database_config(root: &Path, fields: &[String]) -> std::result::Result<String, String> {
    let name = form_field(fields, 0);
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let driver = form_field(fields, 1).to_ascii_lowercase();
    if !DATABASE_DRIVERS.contains(&driver.as_str()) {
        return Err(format!(
            "driver must be one of {}",
            DATABASE_DRIVERS.join(", ")
        ));
    }
    let target = form_field(fields, 2);
    let user = form_field(fields, 3);
    let database = form_field(fields, 4);

    let path = root.join(".zed/databases.json");
    let mut config = load_config_object(&path)?;
    let mut connections = match config.remove("connections") {
        Some(Value::Array(connections)) => connections,
        Some(_) => return Err("connections in databases.json is not an array".to_string()),
        None => Vec::new(),
    };

    upsert_config_entry(&mut connections, &name, |connection| {
        for key in ["driver", "path", "url", "host", "port", "user", "database"] {
            connection.remove(key);
        }
        connection.insert("driver".to_string(), Value::String(driver.clone()));
        if !target.is_empty() {
            if driver == "sqlite" {
                connection.insert("path".to_string(), Value::String(target.clone()));
            } else if target.contains("://") {
                connection.insert("url".to_string(), Value::String(target.clone()));
            } else if let Some((host, port)) = target.rsplit_once(':') {
                match port.parse::<u16>() {
                    Ok(port) => {
                        connection.insert("host".to_string(), Value::String(host.to_string()));
                        connection.insert("port".to_string(), Value::Number(port.into()));
                    }
                    Err(_) => {
                        connection.insert("host".to_string(), Value::String(target.clone()));
                    }
                }
            } else {
                connection.insert("host".to_string(), Value::String(target.clone()));
            }
        }
        if !user.is_empty() {
            connection.insert("user".to_string(), Value::String(user.clone()));
        }
        if !database.is_empty() {
            connection.insert("database".to_string(), Value::String(database.clone()));
        }
    });

    config.insert("connections".to_string(), Value::Array(connections));
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

fn next_connection_name(base: &str, existing: &[DatabaseEntry]) -> String {
    let mut name = base.to_string();
    let mut index = 0usize;
    while existing.iter().any(|entry| entry.name == name) {
        index += 1;
        name = format!("{base} ({index})");
    }
    name
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
        "redis" => &[DsGeneralField::Host, DsGeneralField::Port, DsGeneralField::Password],
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
        first_non_empty_line(stdout.lines())
            .unwrap_or_else(|| "invalid databases JSON".to_string())
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

fn parse_table_rows(stdout: &str) -> Result<Vec<serde_json::Map<String, Value>>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid query JSON".to_string())
    })?;
    let rows = value
        .as_array()
        .ok_or_else(|| "query JSON was not an array".to_string())?;
    Ok(rows.iter().filter_map(Value::as_object).cloned().collect())
}

fn query_columns(rows: &[serde_json::Map<String, Value>]) -> Vec<DatabaseColumn> {
    let mut seen = HashSet::new();
    let mut columns = Vec::new();
    for row in rows {
        for name in row.keys() {
            if seen.insert(name.clone()) {
                columns.push(DatabaseColumn {
                    name: name.clone(),
                    column_type: String::new(),
                    primary_key: false,
                });
            }
        }
    }
    columns
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    }
}

fn render_table_grid(
    id: &str,
    table_name: &str,
    columns: &[DatabaseColumn],
    rows: &[serde_json::Map<String, Value>],
    cx: &Context<PhpPanel>,
) -> AnyElement {
    let preview_rows = rows.iter().take(20).collect::<Vec<_>>();
    let mut headers = columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    if headers.is_empty() {
        if let Some(first) = preview_rows.first() {
            headers.extend(first.keys().cloned());
        }
    }

    let widths = headers
        .iter()
        .map(|header| {
            let mut longest = header.chars().count();
            for row in &preview_rows {
                if let Some(value) = row.get(header) {
                    longest = longest.max(display_value(value).chars().count());
                }
            }
            px(longest.clamp(8, 30) as f32 * 7.0 + 12.0)
        })
        .collect::<Vec<_>>();

    let header_row = h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(cx.theme().colors().border)
        .children(headers.iter().zip(widths.iter()).map(|(header, width)| {
            div()
                .w(*width)
                .flex_none()
                .overflow_hidden()
                .child(
                    Label::new(header.clone())
                        .size(LabelSize::XSmall)
                        .weight(gpui::FontWeight::BOLD)
                        .color(Color::Muted)
                        .buffer_font(cx)
                        .truncate(),
                )
                .into_any_element()
        }));

    let body = v_flex()
        .w_full()
        .children(preview_rows.iter().enumerate().map(|(row_index, row)| {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .when(row_index % 2 == 1, |this| {
                    this.bg(cx.theme().colors().element_background)
                })
                .hover(|style| style.bg(cx.theme().colors().element_hover))
                .children(headers.iter().zip(widths.iter()).map(|(header, width)| {
                    let value = row.get(header).map(display_value).unwrap_or_default();
                    div()
                        .w(*width)
                        .flex_none()
                        .overflow_hidden()
                        .child(
                            Label::new(value)
                                .size(LabelSize::XSmall)
                                .buffer_font(cx)
                                .truncate(),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        }));

    v_flex()
        .id(id.to_string())
        .w_full()
        .flex_none()
        .max_h(relative(0.55))
        .overflow_y_scroll()
        .border_t_1()
        .border_color(cx.theme().colors().border)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .child(
                    Label::new(table_name.to_string())
                        .size(LabelSize::XSmall)
                        .weight(gpui::FontWeight::BOLD)
                        .truncate(),
                )
                .child(
                    Label::new(format!("· {} rows", rows.len()))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(header_row)
        .child(body)
        .into_any_element()
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    .when_some(picker.as_ref().map(|(_, name)| name.clone()), |this, name| {
                        this.child(
                            Label::new(name)
                                .size(LabelSize::Small)
                                .color(Color::Default)
                                .truncate(),
                        )
                    })
                    .when(picker.is_none(), |this| {
                        this.child(
                            Label::new("no server")
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                                .truncate(),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .child(Icon::new(IconName::ChevronDown)
                        .size(IconSize::XSmall)
                        .color(Color::Muted)),
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
                    .icon_size(IconSize::XSmall)
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
                    .icon_size(IconSize::XSmall)
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
                    .icon_size(IconSize::XSmall)
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
                                .size(LabelSize::Small)
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
                            .size(LabelSize::XSmall)
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
                .size(LabelSize::XSmall)
                .color(status_color)
                .truncate()
                .into_any_element();
            let error_label = match self
                .remote_status
                .get(&(server.name.clone(), ".".to_string()))
            {
                Some(RemoteStatus::Failed(message)) => Some(
                    Label::new(message.clone())
                        .size(LabelSize::XSmall)
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
                        .size(LabelSize::XSmall)
                        .color(Color::Muted)
                        .into_any_element(),
                );
            }
            crumb_children.push(
                Label::new(part.clone())
                    .size(LabelSize::XSmall)
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
                        div()
                            .flex_none()
                            .child(Icon::new(IconName::ChevronDown)
                                .size(IconSize::XSmall)
                                .color(Color::Muted)),
                    )
                    .child(remote_protocol_badge(&remote_root_badge(&selected_remote), cx))
                    .child(
                        Label::new(server.name.clone())
                            .size(LabelSize::Small)
                            .color(Color::Default)
                            .flex_none(),
                    )
                    .child(
                        Label::new(host_part.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate(),
                    )
                    .into_any_element(),
            );
            if let Some(entries) = self.remote_entries.get(&server.name) {
                self.push_remote_rows(
                    server,
                    entries,
                    0,
                    &mut remote_line,
                    &mut remote_rows,
                    cx,
                );
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
                                .size(LabelSize::XSmall)
                                .color(color)
                                .truncate(),
                        )
                        .into_any_element(),
                );
            }
        } else {
            remote_rows.push(empty_row("no servers configured"));
        }

        let mut database_rows = Vec::new();
        let mut database_grids = Vec::new();
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
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                    .into_any_element(),
                            );
                        }
                        for (table_index, table) in tables.iter().enumerate() {
                            let table_key = (database.name.clone(), table.name.clone());
                            let preview_status = self.table_status.get(&table_key);

                            let table_expanded = self.expanded_tables.contains(&table_key);
                            let connection_name = database.name.clone();
                            let table_name = table.name.clone();
                            let table_root = database.root.clone();

                            let mut table_item =
                                ListItem::new(format!("database-table-{index}-{table_index}"))
                                    .indent_level(1)
                                    .toggle(table_expanded)
                                    .child(
                                        Label::new(format!(
                                            "{} ({} columns)",
                                            table.name,
                                            table.columns.len()
                                        ))
                                        .truncate(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _window, cx| {
                                            this.toggle_table_preview(
                                                connection_name.clone(),
                                                table_name.clone(),
                                                table_root.clone(),
                                                cx,
                                            );
                                        },
                                    ));
                            if let Some(status) = preview_status {
                                let (status_text, color) = match status {
                                    TableStatus::Loading => {
                                        ("loading…".to_string(), Color::Muted)
                                    }
                                    TableStatus::Loaded(rows) => (
                                        format!(
                                            "{} {}",
                                            rows.len(),
                                            if rows.len() == 1 { "row" } else { "rows" }
                                        ),
                                        Color::Default,
                                    ),
                                    TableStatus::Failed(message) => {
                                        (message.clone(), Color::Error)
                                    }
                                };
                                table_item = table_item.end_slot(
                                    Label::new(status_text)
                                        .size(LabelSize::XSmall)
                                        .color(color)
                                        .truncate(),
                                );
                            }
                            schema_rows.push(table_item.into_any_element());

                            if table_expanded {
                                if let Some(TableStatus::Loaded(rows)) = preview_status {
                                    database_grids.push(render_table_grid(
                                        &format!("database-grid-{index}-{table_index}"),
                                        &table.name,
                                        &table.columns,
                                        rows,
                                        cx,
                                    ));
                                }
                            }

                            for (column_index, column) in table.columns.iter().enumerate() {
                                let text = format!(
                                    "{} · {}{}",
                                    column.name,
                                    column.column_type,
                                    if column.primary_key { " [pk]" } else { "" },
                                );
                                schema_rows.push(
                                    ListItem::new(format!(
                                        "database-column-{index}-{table_index}-{column_index}"
                                    ))
                                    .indent_level(2)
                                    .child(
                                        Label::new(text)
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted)
                                            .truncate(),
                                    )
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
                                        .size(LabelSize::XSmall)
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
                                        .size(LabelSize::XSmall)
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
                                            .size(LabelSize::Small)
                                            .color(Color::Default),
                                    )
                                    .child(
                                        Label::new(format!("({})", views.len()))
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    ),
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _window, cx| {
                                    if this.db_expanded_views.remove(&views_connection) {
                                        cx.notify();
                                    } else {
                                        this.db_expanded_views
                                            .insert(views_connection.clone());
                                        cx.notify();
                                    }
                                },
                            ))
                            .into_any_element(),
                    );
                    if views_open {
                        for (view_index, view) in views.iter().enumerate() {
                            let view_key = (database.name.clone(), view.name.clone());
                            let view_connection = database.name.clone();
                            let view_name = view.name.clone();
                            let view_root = database.root.clone();
                            let view_open = self.expanded_tables.contains(&view_key);
                            let mut view_item =
                                ListItem::new(format!("database-view-{index}-{view_index}"))
                                    .indent_level(2)
                                    .toggle(view_open)
                                    .child(Label::new(view.name.clone()).truncate())
                                    .on_click(cx.listener(
                                        move |this, _: &ClickEvent, _window, cx| {
                                            this.toggle_table_preview(
                                                view_connection.clone(),
                                                view_name.clone(),
                                                view_root.clone(),
                                                cx,
                                            );
                                        },
                                    ));
                            if let Some(status) = self.table_status.get(&view_key) {
                                let (status_text, color) = match status {
                                    TableStatus::Loading => {
                                        ("loading…".to_string(), Color::Muted)
                                    }
                                    TableStatus::Loaded(rows) => (
                                        format!(
                                            "{} {}",
                                            rows.len(),
                                            if rows.len() == 1 { "row" } else { "rows" }
                                        ),
                                        Color::Default,
                                    ),
                                    TableStatus::Failed(message) => {
                                        (message.clone(), Color::Error)
                                    }
                                };
                                view_item = view_item.end_slot(
                                    Label::new(status_text)
                                        .size(LabelSize::XSmall)
                                        .color(color)
                                        .truncate(),
                                );
                            }
                            schema_rows.push(view_item.into_any_element());

                            if view_open {
                                if let Some(TableStatus::Loaded(rows)) =
                                    self.table_status.get(&view_key)
                                {
                                    database_grids.push(render_table_grid(
                                        &format!("database-view-grid-{index}-{view_index}"),
                                        &view.name,
                                        &view.columns,
                                        rows,
                                        cx,
                                    ));
                                }
                            }
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
                                            .size(LabelSize::Small)
                                            .color(Color::Default),
                                    )
                                    .child(
                                        Label::new(names_count)
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    ),
                            )
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _window, cx| {
                                    this.toggle_database_names(
                                        names_connection.clone(),
                                        names_root.clone(),
                                        cx,
                                    );
                                },
                            ))
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
                                                .size(LabelSize::XSmall)
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
                                                .size(LabelSize::XSmall)
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
                                                        .size(IconSize::XSmall)
                                                        .color(Color::Muted),
                                                )
                                                .child(
                                                    Label::new(db_name.clone())
                                                        .size(LabelSize::Small),
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
                                                .size(LabelSize::XSmall)
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
                .database_check_status
                .get(&database.name)
                .map(|status| match status {
                    SyncStatus::Syncing => ("checking…".to_string(), Color::Muted),
                    SyncStatus::Ok => ("ok".to_string(), Color::Success),
                    SyncStatus::Failed(message) => (message.clone(), Color::Error),
                })
                .or_else(|| {
                    self.database_status
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
                })
                .or_else(|| {
                    self.query_status
                        .get(&database.name)
                        .and_then(|status| match status {
                            TableStatus::Loading | TableStatus::Loaded(_) => None,
                            TableStatus::Failed(message) => {
                                Some((message.clone(), Color::Error))
                            }
                        })
                });

            let mut connection_row = h_flex()
                .id(("database-connection", index))
                .w_full()
                .flex_none()
                .px_3()
                .py_1()
                .gap_1()
                .cursor_pointer()
                .hover(|this| this.bg(cx.theme().colors().element_hover))
                .child(
                    div().flex_none().child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                    ),
                )
                .child(driver_badge(driver, driver_badge_color(driver), cx))
                .child(
                    Label::new(database.name.clone())
                        .size(LabelSize::Small)
                        .color(Color::Default)
                        .flex_none(),
                )
                .child(
                    Label::new(format!("({})", paren))
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(div().flex_1().min_w_0());
            if let Some((status_text, color)) = status {
                connection_row = connection_row.child(
                    Label::new(status_text)
                        .size(LabelSize::XSmall)
                        .color(color)
                        .truncate()
                        .flex_none(),
                );
            }
            connection_row = connection_row.on_click(cx.listener(
                move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_connection_expand(name.clone(), is_sqlite, root.clone(), cx);
                },
            ));
            database_rows.push(connection_row.into_any_element());

            database_rows.extend(schema_rows);

            if let Some(TableStatus::Loaded(rows)) = self.query_status.get(&database.name) {
                database_grids.push(render_table_grid(
                    &format!("database-query-grid-{index}"),
                    "query",
                    &query_columns(rows),
                    rows,
                    cx,
                ));
            }
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
                            .size(LabelSize::Small)
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
                });

                let mut item = ListItem::new(("collection-request", collection_row_index))
                    .child(Label::new(text).truncate())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.run_collection_request(request_for_click.clone(), cx);
                    }));
                if let Some((status_text, color)) = status {
                    item = item.end_slot(
                        Label::new(status_text)
                            .size(LabelSize::XSmall)
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
                });

                let mut item = ListItem::new(("http-request", index))
                    .child(Label::new(text).truncate())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.run_http_request(request_for_click.clone(), cx);
                    }));
                if let Some((status_text, color)) = status {
                    item = item.end_slot(
                        Label::new(status_text)
                            .size(LabelSize::XSmall)
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
                Button::new("tests-run-all", "Run all")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .disabled(tests_running)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.run_tests(None, cx);
                    })),
            )
            .child(
                Button::new("tests-refresh", "Refresh")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.refresh_tests(cx);
                    })),
            )
            .child(
                Label::new(tests_summary_text)
                    .size(LabelSize::XSmall)
                    .color(tests_summary_color)
                    .truncate()
                    .flex_1(),
            );

        let mut test_rows = Vec::new();
        for (index, file) in self.test_files.iter().enumerate() {
            let file_for_click = file.clone();
            let mut item = ListItem::new(("test-file", index))
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
                        .size(LabelSize::XSmall)
                        .color(color)
                        .truncate(),
                );
            }
            test_rows.push(item.into_any_element());
        }

        if test_rows.is_empty() {
            let (message, color) = match &self.test_list_status {
                TestListStatus::Loading => ("loading…".to_string(), Color::Muted),
                TestListStatus::Failed(message) => (message.clone(), Color::Error),
                _ => ("no test files".to_string(), Color::Muted),
            };
            test_rows.push(
                h_flex()
                    .px_3()
                    .py_1()
                    .child(Label::new(message).size(LabelSize::Small).color(color))
                    .into_any_element(),
            );
        }

        let test_result_rows = match &self.test_run {
            Some(TestRunState::Running) => vec![
                h_flex()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new("running…")
                            .size(LabelSize::XSmall)
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
                            .size(LabelSize::XSmall)
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
                                .size(LabelSize::XSmall)
                                .color(case.status.color())
                                .flex_none(),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    Label::new(format!("{}::{}", case.class, case.name))
                                        .size(LabelSize::XSmall)
                                        .color(case.status.color())
                                        .truncate(),
                                )
                                .when(!case.message.is_empty(), |this| {
                                    this.child(
                                        Label::new(case.message.clone())
                                            .size(LabelSize::XSmall)
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
                .size(LabelSize::Small)
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
                    .icon_size(IconSize::XSmall)
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
                    .icon_size(IconSize::XSmall)
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
                    .icon_size(IconSize::XSmall)
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
            let mut menu_rows = Vec::new();
            menu_rows.push(
                h_flex()
                    .id("db-menu-datasource")
                    .w_full()
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .child(
                        Label::new("Data Sources and Drivers...")
                            .size(LabelSize::Small)
                            .color(Color::Accent),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.db_menu_open = false;
                        if let Some(root) = this.worktree_root(cx) {
                            open_data_sources_window(root, cx);
                        }
                    }))
                    .into_any_element(),
            );
            menu_rows.push(
                h_flex()
                    .w_full()
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .child(
                        Label::new("Full Support")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
            for (menu_index, driver) in DATABASE_DRIVERS.iter().enumerate() {
                let driver_name = driver.to_string();
                menu_rows.push(
                    h_flex()
                        .id(("db-menu-driver", menu_index))
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .cursor_pointer()
                        .hover(|style| style.bg(cx.theme().colors().element_hover))
                        .child(driver_badge(
                            driver_badge_short(driver),
                            driver_badge_color(driver),
                            cx,
                        ))
                        .child(
                            Label::new(driver_display_name(driver))
                                .size(LabelSize::Small)
                                .color(Color::Default)
                                .truncate(),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_database_form(Some(driver_name.as_str()), window, cx);
                        }))
                        .into_any_element(),
                );
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
                    .max_h(px(220.))
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
                        .size(LabelSize::XSmall)
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
                        .filter(|form| matches!(form.kind, FormKind::Database | FormKind::Sql)),
                    |this, form| this.child(render_form(form)),
                )
                .child(
                    div()
                        .id("php-database-tree")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(if database_rows.is_empty() {
                            vec![empty_row("no databases configured")]
                        } else {
                            database_rows
                        }),
                )
                .children(database_grids)
                .into_any_element(),
            PanelTab::Tests => v_flex()
                .flex_1()
                .min_h_0()
                .child(tests_toolbar)
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
            PanelTab::Collections => div()
                .id("php-collections-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(if collection_rows.is_empty() {
                    vec![empty_row("not configured")]
                } else {
                    collection_rows
                })
                .into_any_element(),
            PanelTab::Http => div()
                .id("php-http-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scroll()
                .children(if http_rows.is_empty() {
                    vec![empty_row("not configured")]
                } else {
                    http_rows
                })
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
                        if (1..=PanelTab::ALL.len()).contains(&index) {
                            this.select_tab(PanelTab::ALL[index - 1], cx);
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
    }}

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
        .children(PanelTab::ALL.into_iter().map(|tab| {
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
                        .size(LabelSize::XSmall)
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
        FormKind::Database => "new database",
        FormKind::Sql => "sql console",
    };
    rows.push(
        h_flex()
            .px_3()
            .py_1()
            .child(
                Label::new(title)
                    .size(LabelSize::XSmall)
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
        let value_label = Label::new(display).size(LabelSize::Small).truncate();
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
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .flex_none(),
                )
                .child(value_label.flex_1())
                .into_any_element(),
        );
    }

    let footer = match form.kind {
        FormKind::Sql => "Enter run · Esc cancel",
        FormKind::Database => "Tab next · Enter save · Esc cancel",
    };
    rows.push(
        h_flex()
            .px_3()
            .py_1()
            .child(
                Label::new(footer)
                    .size(LabelSize::XSmall)
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
                        .size(LabelSize::XSmall)
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
            Label::new(format!("{} ", protocol))
                .size(LabelSize::XSmall)
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
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
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

        let trigger = Button::new("deploy-server-indicator", active.unwrap_or_else(|| "no server".to_string()))
            .size(ButtonSize::Compact)
            .label_size(LabelSize::Small)
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

    #[test]
    fn form_defaults_match_field_labels() {
        for kind in [FormKind::Database, FormKind::Sql] {
            let form = FormState::new(kind);
            assert_eq!(form.fields.len(), form.labels().len());
            assert_eq!(form.active, 0);
        }
        let database = FormState::new(FormKind::Database);
        assert_eq!(database.fields[1], "sqlite");
        let sql = FormState::sql("app".to_string());
        assert_eq!(sql.labels(), &["sql"]);
        assert_eq!(sql.placeholder(0), Some("select * from users limit 20"));
        assert_eq!(sql.connection.as_deref(), Some("app"));
    }

    #[test]
    fn query_columns_unions_keys_in_row_order() {
        let rows = vec![
            json!({"id": 1, "email": "a@example.test"}),
            json!({"email": "b@example.test", "name": "B"}),
        ];
        let rows = rows
            .into_iter()
            .filter_map(|row| row.as_object().cloned())
            .collect::<Vec<_>>();
        let columns = query_columns(&rows);
        let names = columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["id", "email", "name"]);
    }

    #[test]
    fn form_key_sequence_matches_typing_model() {
        let mut database = FormState::new(FormKind::Database);
        assert_eq!(database.fields.len(), 5);

        for character in "notes".chars() {
            let character = character.to_string();
            assert_eq!(
                database.handle_key("", Some(&character)),
                FormKeyOutcome::Continue
            );
        }
        database.handle_key("space", None);
        assert_eq!(database.fields[0], "notes ");
        database.handle_key("tab", None);
        assert_eq!(database.active, 1);
        database.handle_key("up", None);
        assert_eq!(database.active, 0);
        assert_eq!(database.handle_key("enter", None), FormKeyOutcome::Save);
        assert_eq!(database.handle_key("escape", None), FormKeyOutcome::Cancel);

        let mut sql = FormState::sql("app".to_string());
        for character in "my query".chars() {
            let character = character.to_string();
            sql.handle_key("", Some(&character));
        }
        assert_eq!(sql.fields[0], "my query");
        assert_eq!(sql.handle_key("enter", None), FormKeyOutcome::Save);
    }

#[test]
    fn save_server_creates_config_with_numeric_port() {
        let root = temp_root("server_create");
        let name = save_server_config(
            &root,
            &fields(&["dev", "sftp", "example.test", "2222", "deploy", "/var/www", "."]),
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
            resolve_active_server(Some("missing".to_string()), Some("sftp".to_string()), &names),
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
    fn save_database_maps_target_by_shape() {
        let root = temp_root("database_targets");
        save_database_config(
            &root,
            &fields(&["notes", "sqlite", "notes.sqlite", "", ""]),
        )
        .expect("save sqlite connection");
        save_database_config(
            &root,
            &fields(&["pg", "postgres", "postgres://app@localhost:5432/app", "", ""]),
        )
        .expect("save url connection");
        save_database_config(
            &root,
            &fields(&["mysql", "mysql", "db.local:3306", "app", "app_db"]),
        )
        .expect("save host connection");

        let path = root.join(".zed/databases.json");
        let config = read_json(&path).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 3);
        let notes = &connections[0];
        assert_eq!(notes["driver"], json!("sqlite"));
        assert_eq!(notes["path"], json!("notes.sqlite"));
        assert!(notes.get("host").is_none());
        let pg = &connections[1];
        assert_eq!(pg["url"], json!("postgres://app@localhost:5432/app"));
        assert!(pg.get("path").is_none());
        let mysql = &connections[2];
        assert_eq!(mysql["host"], json!("db.local"));
        assert_eq!(mysql["port"], json!(3306));
        assert_eq!(mysql["user"], json!("app"));
        assert_eq!(mysql["database"], json!("app_db"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
    }

    #[test]
    fn save_database_upserts_and_preserves_other_connections() {
        let root = temp_root("database_upsert");
        let zed = root.join(".zed");
        std::fs::create_dir_all(&zed).expect("create .zed");
        std::fs::write(
            zed.join("databases.json"),
            r#"{
  "connections": [
    { "name": "app", "driver": "sqlite", "path": "old.sqlite" },
    { "name": "pg", "driver": "postgres", "url": "postgres://localhost/app", "password_env": "PGPASSWORD" }
  ]
}"#,
        )
        .expect("seed databases.json");

        save_database_config(&root, &fields(&["app", "sqlite", "new.sqlite", "", ""]))
            .expect("update app");

        let config = read_json(&zed.join("databases.json")).expect("parse databases.json");
        let connections = config["connections"].as_array().expect("connections array");
        assert_eq!(connections.len(), 2);
        assert_eq!(connections[0]["path"], json!("new.sqlite"));
        assert_eq!(connections[1]["password_env"], json!("PGPASSWORD"));

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
        assert_eq!(result.tests[1].message, "Failed asserting that 5 is identical to 4.");
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
        assert_eq!(
            server.local_root.as_deref(),
            Some(".")
        );
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
            vec!["vendor/".to_string(), "node_modules/".to_string(), "tmp".to_string()]
        );
        assert_eq!(parse_excluded(""), Vec::<String>::new());
        assert_eq!(excluded_text(Some(&parse_excluded("a,b"))), "a, b");
        assert_eq!(excluded_text(None), "");

        let mut server = blank_server(PathBuf::from("/tmp"));
        server.excluded = vec!["vendor/".to_string()];
        assert_eq!(
            excluded_text(Some(server.excluded.as_slice())),
            "vendor/"
        );
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
        }
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
    fn next_connection_name_prefills_unique_names() {
        assert_eq!(next_connection_name("sqlite", &[]), "sqlite");
        let taken = vec![connection_entry("sqlite")];
        assert_eq!(next_connection_name("sqlite", &taken), "sqlite (1)");
        let taken = vec![
            connection_entry("sqlite"),
            connection_entry("sqlite (1)"),
        ];
        assert_eq!(next_connection_name("sqlite", &taken), "sqlite (2)");
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
    fn first_tab_is_remote_and_cycling_wraps() {
        assert_eq!(PanelTab::ALL.first(), Some(&PanelTab::Remote));
        assert_eq!(PanelTab::ALL.len(), 5);
        assert_eq!(next_tab(PanelTab::Remote, 1), PanelTab::Databases);
        assert_eq!(next_tab(PanelTab::Remote, -1), PanelTab::Collections);
        assert_eq!(next_tab(PanelTab::Collections, 1), PanelTab::Remote);
        assert_eq!(next_tab(PanelTab::Collections, -1), PanelTab::Http);
        for tab in PanelTab::ALL {
            assert_eq!(next_tab(tab, 5), tab);
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
        assert!(
            data_source_general_fields("custom")
                .contains(&DsGeneralField::Host)
        );
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
        std::fs::write(root.join(".zed/ssh.json"), r#"{"configs":[{"name":"legacy"}]}"#)
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
    fn next_ssh_auth_cycles_password_key_agent() {
        let expected = [
            SshAuthMode::Key,
            SshAuthMode::Agent,
            SshAuthMode::Password,
            SshAuthMode::Key,
        ];
        let mut current = SshAuthMode::Password;
        for expected_mode in expected {
            assert_eq!(next_ssh_auth(current), expected_mode);
            current = next_ssh_auth(current);
        }
        assert_eq!(SshAuthMode::parse("key"), SshAuthMode::Key);
        assert_eq!(SshAuthMode::parse("agent"), SshAuthMode::Agent);
        assert_eq!(SshAuthMode::parse("password"), SshAuthMode::Password);
        assert_eq!(SshAuthMode::parse("gibberish"), SshAuthMode::Password);
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

        let mut tunnelled =
            serde_json::json!({"ssh_tunnel": {"enabled": true, "name": "gone"}});
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
        assert_eq!(rows, vec![("a_key".to_string(), "1".to_string()), ("b_key".to_string(), "2".to_string())]);

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
        for name in ["demo.sqlite", "backup.db", "dump.sqlite3", "notes.DB", "x.SQLite"] {
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
        for expected in ["host is required", "port is required", "user is required", "database is required"] {
            assert!(problems.contains(&expected.to_string()), "missing {expected}");
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
        assert_eq!(connections[1]["url"], json!("postgres://app@localhost:5432/app"));

        std::fs::remove_dir_all(&root).expect("clean temporary project root");
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
        assert_eq!(
            next_data_source_name("a copy", &sources),
            "a copy"
        );
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

        assert_eq!(next_deploy_type("webdavs"), "local");
        assert_eq!(next_deploy_type("sftp"), "ftp");
        assert_eq!(next_deploy_type("gopher"), "in-place");
    }

    #[test]
    fn deploy_connection_fields_matrix_matches_types() {
        assert_eq!(
            deploy_connection_fields("sftp"),
            &[
                DeployField::SshConfig,
                DeployField::RsyncToggle,
                DeployField::RootPath,
                DeployField::WebUrl,
                DeployField::TestConnection,
            ]
        );
        for kind in ["ftp", "ftps", "webdav", "webdavs"] {
            assert_eq!(
                deploy_connection_fields(kind),
                &[
                    DeployField::Host,
                    DeployField::Port,
                    DeployField::User,
                    DeployField::AnonToggle,
                    DeployField::Password,
                    DeployField::RootPath,
                    DeployField::WebUrl,
                    DeployField::TestConnection,
                ]
            );
        }
        assert_eq!(
            deploy_connection_fields("local"),
            &[
                DeployField::Folder,
                DeployField::RootPath,
                DeployField::WebUrl,
                DeployField::TestConnection,
            ]
        );
        assert_eq!(
            deploy_connection_fields("in-place"),
            &[DeployField::WebUrl, DeployField::TestConnection]
        );

        assert_eq!(deploy_default_port("ftp"), Some("21"));
        assert_eq!(deploy_default_port("ftps"), Some("990"));
        assert_eq!(deploy_default_port("webdav"), Some("80"));
        assert_eq!(deploy_default_port("webdavs"), Some("443"));
        assert_eq!(deploy_default_port("sftp"), None);
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
        assert_eq!(
            parsed[0].1,
            vec!["Folder is not specified".to_string()]
        );

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
        let value: Value = serde_json::from_str(r#"{"name":"ftp1","protocol":"ftp"}"#)
            .expect("parse config");
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

        let mut connection =
            serde_json::json!({"ssh_tunnel": {"enabled": true, "name": "local-ssh", "local_port": 15432}});
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
}
