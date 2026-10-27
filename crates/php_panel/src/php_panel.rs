use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};

use anyhow::Result;
use gpui::{
    App, AppContext, AsyncWindowContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, WeakEntity, Window,
    actions, uniform_list,
};
use project::{Project, ProjectPath};
use regex::Regex;
use serde_json::Value;
use ui::{Label, LabelSize, ListItem, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::dock::{DockPosition, PanelEvent};
use workspace::{OpenOptions, Panel, Workspace};

actions!(php_panel, [ToggleFocus]);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PanelTab {
    Files,
    Remote,
    Databases,
    Tests,
    Http,
    Collections,
}

impl PanelTab {
    const ALL: [PanelTab; 6] = [
        PanelTab::Files,
        PanelTab::Remote,
        PanelTab::Databases,
        PanelTab::Tests,
        PanelTab::Http,
        PanelTab::Collections,
    ];

    fn label(self) -> &'static str {
        match self {
            PanelTab::Files => "Files",
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
    user: Option<String>,
    remote_root: Option<String>,
    upload_on_save: bool,
    root: PathBuf,
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
    root: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind {
    Server,
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
            FormKind::Server => vec![
                String::new(),
                "local".to_string(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                ".".to_string(),
            ],
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
            FormKind::Server => &[
                "name",
                "protocol",
                "host",
                "port",
                "user",
                "remote_root",
                "local_root",
            ],
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

const SERVER_PROTOCOLS: [&str; 2] = ["local", "sftp"];
const DATABASE_DRIVERS: [&str; 8] = [
    "sqlite",
    "postgres",
    "mysql",
    "mariadb",
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

enum TableStatus {
    Loading,
    Loaded(Vec<serde_json::Map<String, Value>>),
    Failed(String),
}

#[derive(Clone)]
struct Symbol {
    name: String,
    kind: &'static str,
    file: ProjectPath,
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

struct SymbolRegexes {
    php_type: Regex,
    php_function: Regex,
    js_class: Regex,
    js_interface: Regex,
    js_type: Regex,
    js_function: Regex,
    js_const: Regex,
}

fn symbol_regexes() -> &'static SymbolRegexes {
    static REGEXES: OnceLock<SymbolRegexes> = OnceLock::new();
    REGEXES.get_or_init(|| SymbolRegexes {
        php_type: Regex::new(
            r"^\s*(?:(?:abstract|final|readonly)\s+)*(class|interface|trait|enum)\s+([A-Za-z_][A-Za-z0-9_]*)",
        )
        .expect("valid PHP type regex"),
        php_function: Regex::new(r"\bfunction\s+&?\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(")
            .expect("valid PHP function regex"),
        js_class: Regex::new(
            r"^\s*(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        )
        .expect("valid JS class regex"),
        js_interface: Regex::new(
            r"^\s*(?:export\s+)?(?:declare\s+)?interface\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        )
        .expect("valid JS interface regex"),
        js_type: Regex::new(
            r"^\s*(?:export\s+)?(?:declare\s+)?type\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?:<[^=]*>)?\s*=",
        )
        .expect("valid JS type regex"),
        js_function: Regex::new(
            r"\b(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
        )
        .expect("valid JS function regex"),
        js_const: Regex::new(
            r"^\s*(?:export\s+)?const\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]+)?=",
        )
        .expect("valid JS const regex"),
    })
}

fn symbol_extension_supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("php" | "ts" | "tsx" | "js" | "jsx" | "vue")
    )
}

fn scan_symbols(path: &Path, file: &ProjectPath) -> Vec<Symbol> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let is_php = path.extension().and_then(|extension| extension.to_str()) == Some("php");
    let patterns = symbol_regexes();
    let mut symbols = Vec::new();

    for line in contents.lines() {
        if is_php {
            if let Some(captures) = patterns.php_type.captures(line) {
                symbols.push(Symbol {
                    name: captures[2].to_string(),
                    kind: match &captures[1] {
                        "interface" => "interface",
                        "trait" => "trait",
                        "enum" => "enum",
                        _ => "class",
                    },
                    file: file.clone(),
                });
            } else if let Some(captures) = patterns.php_function.captures(line) {
                symbols.push(Symbol {
                    name: captures[1].to_string(),
                    kind: "function",
                    file: file.clone(),
                });
            }
        } else if let Some(captures) = patterns.js_class.captures(line) {
            symbols.push(Symbol {
                name: captures[1].to_string(),
                kind: "class",
                file: file.clone(),
            });
        } else if let Some(captures) = patterns.js_interface.captures(line) {
            symbols.push(Symbol {
                name: captures[1].to_string(),
                kind: "interface",
                file: file.clone(),
            });
        } else if let Some(captures) = patterns.js_type.captures(line) {
            symbols.push(Symbol {
                name: captures[1].to_string(),
                kind: "type",
                file: file.clone(),
            });
        } else if let Some(captures) = patterns.js_function.captures(line) {
            symbols.push(Symbol {
                name: captures[1].to_string(),
                kind: "function",
                file: file.clone(),
            });
        } else if let Some(captures) = patterns.js_const.captures(line) {
            symbols.push(Symbol {
                name: captures[1].to_string(),
                kind: "const",
                file: file.clone(),
            });
        }
    }

    symbols
}

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
    scroll_handle: UniformListScrollHandle,
    active_tab: PanelTab,
    query: String,
    files: Vec<ProjectPath>,
    visible_files: Vec<ProjectPath>,
    symbols: Vec<Symbol>,
    symbol_cache: HashMap<ProjectPath, (SystemTime, Vec<Symbol>)>,
    http_requests: Vec<HttpRequest>,
    collections: Vec<PostmanCollection>,
    remote_servers: Vec<RemoteServer>,
    selected_remote_server: Option<String>,
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
    http_status: HashMap<String, HttpStatus>,
    test_list_status: TestListStatus,
    test_files: Vec<String>,
    test_run: Option<TestRunState>,
    test_active_file: Option<String>,
    form: Option<FormState>,
    saved_message: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl PhpPanel {
    pub fn init(cx: &mut App) {
        cx.observe_new(|workspace: &mut Workspace, _, _| {
            workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
                workspace.toggle_panel_focus::<PhpPanel>(window, cx);
            });
        })
        .detach();
    }

    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, _window, cx| {
            let project = workspace.project().clone();
            let workspace_handle = cx.entity().downgrade();

            cx.new(|cx| {
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
                    scroll_handle: UniformListScrollHandle::new(),
                    active_tab: PanelTab::Files,
                    query: String::new(),
                    files: Vec::new(),
                    visible_files: Vec::new(),
                    symbols: Vec::new(),
                    symbol_cache: HashMap::new(),
                    http_requests: Vec::new(),
                    collections: Vec::new(),
                    remote_servers: Vec::new(),
                    selected_remote_server: None,
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
                    http_status: HashMap::new(),
                    test_list_status: TestListStatus::Unloaded,
                    test_files: Vec::new(),
                    test_run: None,
                    test_active_file: None,
                    form: None,
                    saved_message: None,
                    _subscriptions: vec![subscription],
                }
            })
        })
    }

    fn refresh(&mut self, cx: &App) {
        let project = self.project.read(cx);
        let mut files = Vec::new();
        let mut http_requests = Vec::new();
        let mut collections = Vec::new();
        let mut remote_servers = Vec::new();
        let mut databases = Vec::new();
        let mut symbol_candidates = Vec::new();

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
                if symbol_extension_supported(&absolute_path) {
                    symbol_candidates.push((project_path.clone(), absolute_path.clone()));
                }
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
                files.push(project_path);
            }

            if let Some(config) = read_json(&root.join(".zed/deploy.json")) {
                if let Some(servers) = config.get("servers").and_then(Value::as_array) {
                    for server in servers {
                        let Some(name) = string_field(server, "name") else {
                            continue;
                        };
                        remote_servers.push(RemoteServer {
                            name,
                            protocol: string_field(server, "protocol"),
                            host: string_field(server, "host"),
                            user: string_field(server, "user"),
                            remote_root: string_field(server, "remote_root"),
                            upload_on_save: server
                                .get("upload_on_save")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            root: root.clone(),
                        });
                    }
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
                            root: root.clone(),
                        });
                    }
                }
            }
        }

        files.sort_by(|left, right| left.path.as_unix_str().cmp(right.path.as_unix_str()));
        http_requests.sort_by(|left, right| {
            left.file
                .path
                .as_unix_str()
                .cmp(right.file.path.as_unix_str())
                .then(left.line.cmp(&right.line))
        });

        let mut symbols = Vec::new();
        for (project_path, absolute_path) in symbol_candidates {
            let Ok(modified) =
                std::fs::metadata(&absolute_path).and_then(|metadata| metadata.modified())
            else {
                continue;
            };
            if let Some((cached_modified, cached_symbols)) = self.symbol_cache.get(&project_path) {
                if *cached_modified == modified {
                    symbols.extend(cached_symbols.iter().cloned());
                    continue;
                }
            }
            let scanned = scan_symbols(&absolute_path, &project_path);
            self.symbol_cache
                .insert(project_path, (modified, scanned.clone()));
            symbols.extend(scanned);
        }

        self.files = files;
        self.symbols = symbols;
        self.http_requests = http_requests;
        self.collections = collections;
        self.remote_servers = remote_servers;
        self.databases = databases;

        let selection_valid = self.selected_remote_server.as_ref().is_some_and(|name| {
            self.remote_servers
                .iter()
                .any(|server| &server.name == name)
        });
        if !selection_valid {
            self.selected_remote_server = self
                .remote_servers
                .first()
                .map(|server| server.name.clone());
        }
    }

    fn open_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        if let Some(path) = self
            .files
            .iter()
            .find(|path| path.path.as_unix_str().to_lowercase().contains(&query))
            .cloned()
        {
            self.open_file(path, window, cx);
        }
    }

    fn open_file(&mut self, path: ProjectPath, _window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update_in(cx, |workspace, window, cx| {
                workspace
                    .open_path(path, None, true, window, cx)
                    .detach_and_log_err(cx);
            })
            .log_err();
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
        self.selected_remote_server
            .as_ref()
            .and_then(|name| self.remote_servers.iter().find(|server| &server.name == name))
            .or_else(|| self.remote_servers.first())
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
        if self.selected_remote_server.as_deref() != Some(server.name.as_str()) {
            self.selected_remote_server = Some(server.name.clone());
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

    fn cycle_remote_server(&mut self, cx: &mut Context<Self>) {
        if self.remote_servers.is_empty() {
            return;
        }
        let current_index = self
            .selected_remote_server
            .as_ref()
            .and_then(|name| {
                self.remote_servers
                    .iter()
                    .position(|server| &server.name == name)
            })
            .unwrap_or(0);
        let next_index = (current_index + 1) % self.remote_servers.len();
        let server = self.remote_servers[next_index].clone();
        self.selected_remote_server = Some(server.name.clone());
        self.remote_current_path
            .entry(server.name.clone())
            .or_insert_with(|| ".".to_string());
        self.ensure_remote_selected(cx);
        cx.notify();
    }

    fn refresh_remote(&mut self, server: String, root: PathBuf, cx: &mut Context<Self>) {
        self.selected_remote_server = Some(server.clone());
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
        self.selected_remote_server = Some(server.clone());
        self.remote_current_path
            .insert(server.clone(), remote_parent_path(&path));
        self.remote_selected_file = Some((server, path));
        cx.notify();
    }

    fn refresh_databases(&mut self, cx: &mut Context<Self>) {
        self.database_status.clear();
        self.database_check_status.clear();
        self.table_status.clear();
        self.expanded_tables.clear();
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
        self.selected_remote_server = Some(server.clone());
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
        self.selected_remote_server = Some(server.clone());
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
            let icon = if entry.is_directory {
                if expanded {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                }
            } else {
                IconName::File
            };

            let mut item = ListItem::new(("remote-entry", *line))
                .indent_level(depth + 1)
                .selectable(true)
                .toggle_state(selected)
                .always_show_disclosure_icon(true)
                .start_slot(Icon::new(icon).size(IconSize::XSmall))
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
        self.query.clear();
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
        self.query.clear();
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

        let saved = match kind {
            FormKind::Server => save_server_config(&root, &fields),
            FormKind::Database => save_database_config(&root, &fields),
            FormKind::Sql => return,
        };

        match saved {
            Ok(name) => {
                self.form = None;
                self.query.clear();
                self.saved_message = Some("saved".to_string());
                self.refresh(cx);
                match kind {
                    FormKind::Server => {
                        self.selected_remote_server = Some(name.clone());
                        self.ensure_remote_selected(cx);
                        self.run_server_check(name, root, cx);
                    }
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

    fn toggle_database_schema(&mut self, name: String, root: PathBuf, cx: &mut Context<Self>) {
        if matches!(
            self.database_status.get(&name),
            Some(DatabaseStatus::Loading)
        ) {
            return;
        }
        if self.database_status.contains_key(&name) {
            self.database_status.remove(&name);
            cx.notify();
        } else {
            self.load_database_schema(name, root, cx);
        }
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

fn parse_database_schema(stdout: &str) -> Result<Vec<DatabaseTable>, String> {
    let value: Value = serde_json::from_str(stdout).map_err(|_| {
        first_non_empty_line(stdout.lines()).unwrap_or_else(|| "invalid schema JSON".to_string())
    })?;
    let tables = value
        .get("tables")
        .and_then(Value::as_array)
        .ok_or_else(|| "schema JSON had no tables".to_string())?;

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

    Ok(parsed)
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
        gpui::px(360.)
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
        let query = if self.active_tab == PanelTab::Files {
            self.query.trim().to_lowercase()
        } else {
            String::new()
        };
        self.visible_files = self
            .files
            .iter()
            .filter(|path| {
                query.is_empty() || path.path.as_unix_str().to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        let file_count = self.visible_files.len();
        let total = self.files.len();

        let mut matched_symbols = self
            .symbols
            .iter()
            .filter(|symbol| {
                query.is_empty()
                    || symbol.name.to_lowercase().contains(&query)
                    || symbol
                        .file
                        .path
                        .as_unix_str()
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let symbol_total = matched_symbols.len();
        if !query.is_empty() {
            matched_symbols.sort_by_key(|symbol| {
                let name = symbol.name.to_lowercase();
                let path = symbol.file.path.as_unix_str().to_lowercase();
                if name.starts_with(&query) {
                    0
                } else if path.starts_with(&query) {
                    1
                } else {
                    2
                }
            });
        }
        matched_symbols.truncate(200);

        let symbol_rows = matched_symbols
            .iter()
            .enumerate()
            .map(|(index, symbol)| {
                let target = symbol.file.clone();
                let text = format!(
                    "{} · {} · {}",
                    symbol.name,
                    symbol.kind,
                    symbol.file.path.as_unix_str(),
                );
                ListItem::new(("symbol", index))
                    .child(Label::new(text).truncate())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_file(target.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let files_list = uniform_list(
            "search-files",
            file_count,
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let mut items = Vec::with_capacity(range.len());
                for index in range {
                    let entry = this.visible_files[index].clone();
                    let path = entry.path.as_unix_str().to_string();
                    items.push(
                        ListItem::new(index)
                            .child(Label::new(path).truncate())
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_file(entry.clone(), window, cx);
                            }))
                            .into_any_element(),
                    );
                }
                items
            }),
        )
        .track_scroll(&self.scroll_handle)
        .flex_1();

        let selected_remote = self.current_remote_server().cloned();

        let server_selector = if let Some(server) = &selected_remote {
            Button::new(
                format!("remote-server-{}", server.name),
                format!("{} ▾", server.name),
            )
            .size(ButtonSize::Compact)
            .label_size(LabelSize::XSmall)
            .style(ButtonStyle::Subtle)
            .truncate(true)
            .tooltip(Tooltip::text("Switch server"))
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.cycle_remote_server(cx);
            }))
            .into_any_element()
        } else {
            Button::new("remote-server-none", "no servers")
                .size(ButtonSize::Compact)
                .label_size(LabelSize::XSmall)
                .style(ButtonStyle::Subtle)
                .disabled(true)
                .into_any_element()
        };

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
            .child(h_flex().flex_1().min_w_0().child(server_selector))
            .child(
                Button::new("remote-refresh", "Refresh")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .disabled(refresh_target.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        if let Some((name, root)) = refresh_target.clone() {
                            this.refresh_remote(name, root, cx);
                        }
                    })),
            )
            .child(
                Button::new("remote-sync", "Sync")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .disabled(sync_target.is_none())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        if let Some((name, root)) = sync_target.clone() {
                            this.sync_server(name, root, cx);
                        }
                    })),
            )
            .child(
                Button::new("remote-add-server", "+")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .tooltip(Tooltip::text("Add server"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_form(FormKind::Server, window, cx);
                    })),
            );

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
        let remote_breadcrumb = h_flex()
            .id("remote-breadcrumb")
            .w_full()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .overflow_hidden()
            .tooltip(Tooltip::text(crumb_text))
            .children(crumb_children);

        let mut remote_rows = Vec::new();
        let mut remote_line = 0usize;
        if let Some(server) = &selected_remote {
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
            let haystack = format!(
                "{} {} {}",
                database.name,
                display_field(&database.driver),
                display_field(&database.target),
            )
            .to_lowercase();
            let connection_matches = query.is_empty() || haystack.contains(&query);

            let mut schema_rows = Vec::new();
            let mut schema_matches = false;
            if let Some(DatabaseStatus::Loaded(tables)) =
                self.database_status.get(&database.name)
            {
                for (table_index, table) in tables.iter().enumerate() {
                    let table_key = (database.name.clone(), table.name.clone());
                    let preview_status = self.table_status.get(&table_key);
                    let preview_matches = matches!(
                        preview_status,
                        Some(TableStatus::Loaded(rows))
                            if !query.is_empty()
                                && rows.iter().any(|row| row.values().any(|value| {
                                    display_value(value).to_lowercase().contains(&query)
                                }))
                    );
                    let table_matches = query.is_empty()
                        || table.name.to_lowercase().contains(&query)
                        || preview_matches;
                    let mut matching_columns = Vec::new();
                    for (column_index, column) in table.columns.iter().enumerate() {
                        let column_matches = query.is_empty()
                            || table_matches
                            || column.name.to_lowercase().contains(&query)
                            || column.column_type.to_lowercase().contains(&query);
                        if column_matches {
                            matching_columns.push((column_index, column));
                        }
                    }
                    if !table_matches && matching_columns.is_empty() {
                        continue;
                    }
                    schema_matches = true;

                    let expanded = self.expanded_tables.contains(&table_key);
                    let connection_name = database.name.clone();
                    let table_name = table.name.clone();
                    let table_root = database.root.clone();

                    let mut table_item =
                        ListItem::new(format!("database-table-{index}-{table_index}"))
                            .indent_level(1)
                            .toggle(expanded)
                            .child(
                                Label::new(format!(
                                    "{} ({} columns)",
                                    table.name,
                                    table.columns.len()
                                ))
                                .truncate(),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                                this.toggle_table_preview(
                                    connection_name.clone(),
                                    table_name.clone(),
                                    table_root.clone(),
                                    cx,
                                );
                            }));
                    if let Some(status) = preview_status {
                        let (status_text, color) = match status {
                            TableStatus::Loading => ("loading…".to_string(), Color::Muted),
                            TableStatus::Loaded(rows) => (
                                format!(
                                    "{} {}",
                                    rows.len(),
                                    if rows.len() == 1 { "row" } else { "rows" }
                                ),
                                Color::Default,
                            ),
                            TableStatus::Failed(message) => (message.clone(), Color::Error),
                        };
                        table_item = table_item.end_slot(
                            Label::new(status_text)
                                .size(LabelSize::XSmall)
                                .color(color)
                                .truncate(),
                        );
                    }
                    schema_rows.push(table_item.into_any_element());

                    if expanded {
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

                    for (column_index, column) in matching_columns {
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

            if !connection_matches && !schema_matches {
                continue;
            }

            let text = format!(
                "{} · {} · {}",
                database.name,
                display_field(&database.driver),
                display_field(&database.target),
            );
            let name = database.name.clone();
            let root = database.root.clone();
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
                });

            let mut item = ListItem::new(("database", index))
                .child(Label::new(text).truncate())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.toggle_database_schema(name.clone(), root.clone(), cx);
                }));
            if let Some((status_text, color)) = status {
                item = item.end_slot(
                    Label::new(status_text)
                        .size(LabelSize::XSmall)
                        .color(color)
                        .truncate(),
                );
            }
            database_rows.push(item.into_any_element());

            let sql_connection = database.name.clone();
            let mut sql_item = ListItem::new(("database-sql", index))
                .indent_level(1)
                .child(Label::new("SQL").truncate())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_sql_form(sql_connection.clone(), window, cx);
                }));
            if let Some(status) = self.query_status.get(&database.name) {
                let (status_text, color) = match status {
                    TableStatus::Loading => ("running…".to_string(), Color::Muted),
                    TableStatus::Loaded(rows) => (
                        format!(
                            "{} {}",
                            rows.len(),
                            if rows.len() == 1 { "row" } else { "rows" }
                        ),
                        Color::Default,
                    ),
                    TableStatus::Failed(message) => (message.clone(), Color::Error),
                };
                sql_item = sql_item.end_slot(
                    Label::new(status_text)
                        .size(LabelSize::XSmall)
                        .color(color)
                        .truncate(),
                );
            }
            database_rows.push(sql_item.into_any_element());

            if let Some(TableStatus::Loaded(rows)) = self.query_status.get(&database.name) {
                database_grids.push(render_table_grid(
                    &format!("database-query-grid-{index}"),
                    "query",
                    &query_columns(rows),
                    rows,
                    cx,
                ));
            }

            database_rows.extend(schema_rows);
        }

        let mut collection_rows = Vec::new();
        let mut collection_row_index = 0usize;
        for collection in &self.collections {
            let collection_matches =
                query.is_empty() || collection.name.to_lowercase().contains(&query);
            let requests = collection
                .requests
                .iter()
                .filter(|request| {
                    collection_matches
                        || format!(
                            "{} {} {}",
                            request.folder.join(" "),
                            request.name,
                            request.url
                        )
                        .to_lowercase()
                        .contains(&query)
                })
                .collect::<Vec<_>>();
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
            .filter(|request| {
                query.is_empty()
                    || format!(
                        "{} {} {} {}",
                        request.name,
                        request.method,
                        request.url,
                        request.file.path.as_unix_str(),
                    )
                    .to_lowercase()
                    .contains(&query)
            })
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

        let add_server_row = h_flex()
            .id("add-server")
            .px_3()
            .py_1()
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_form(FormKind::Server, window, cx);
            }))
            .child(
                Label::new("+ add server")
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .into_any_element();

        let add_database_row = h_flex()
            .id("add-database")
            .px_3()
            .py_1()
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_form(FormKind::Database, window, cx);
            }))
            .child(
                Label::new("+ add database")
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .into_any_element();

        let section_header = |title: &'static str, cx: &Context<Self>| {
            h_flex()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(cx.theme().colors().border)
                .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
        };

        let files_header = v_flex()
            .px_3()
            .py_2()
            .child(
                Label::new(if query.is_empty() {
                    format!("Search everywhere · {total} files · {symbol_total} symbols")
                } else {
                    format!(
                        "Search: {} · {} matches",
                        self.query.trim(),
                        file_count + symbol_total
                    )
                })
                .truncate(),
            )
            .child(
                Label::new("type to filter · Enter opens first · Esc clears")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .when_some(self.saved_message.clone(), |this, message| {
                this.child(
                    Label::new(message)
                        .size(LabelSize::XSmall)
                        .color(Color::Success),
                )
            });

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
                Label::new(format!("{} connections", self.databases.len()))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted)
                    .truncate()
                    .flex_1(),
            )
            .child(
                Button::new("database-refresh", "Refresh")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.refresh_databases(cx);
                    })),
            )
            .child(
                Button::new("database-add", "+")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::XSmall)
                    .style(ButtonStyle::Subtle)
                    .tooltip(Tooltip::text("Add database"))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_form(FormKind::Database, window, cx);
                    })),
            );

        let tab_bar = render_tab_bar(self.active_tab, cx);

        let content = match self.active_tab {
            PanelTab::Files => v_flex()
                .flex_1()
                .min_h_0()
                .child(files_header)
                .child(files_list)
                .child(section_header("Symbols", cx))
                .child(
                    div()
                        .id("php-symbols")
                        .flex_shrink_0()
                        .max_h(relative(0.42))
                        .overflow_y_scroll()
                        .children(if symbol_rows.is_empty() {
                            vec![empty_row("no symbols")]
                        } else {
                            symbol_rows
                        }),
                )
                .into_any_element(),
            PanelTab::Remote => v_flex()
                .flex_1()
                .min_h_0()
                .child(remote_toolbar)
                .child(remote_status)
                .child(remote_breadcrumb)
                .when_some(
                    self.form
                        .as_ref()
                        .filter(|form| form.kind == FormKind::Server),
                    |this, form| this.child(render_form(form)),
                )
                .child(
                    div()
                        .id("php-remote-tree")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .children(remote_rows)
                        .child(add_server_row),
                )
                .into_any_element(),
            PanelTab::Databases => v_flex()
                .flex_1()
                .min_h_0()
                .child(database_toolbar)
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
                            vec![empty_row("not configured")]
                        } else {
                            database_rows
                        })
                        .child(add_database_row),
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let modifiers = &event.keystroke.modifiers;
                if modifiers.platform || modifiers.control || modifiers.alt {
                    return;
                }
                if this.form.is_some() {
                    this.handle_form_key(event, cx);
                    return;
                }
                if this.active_tab != PanelTab::Files {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "backspace" => {
                        this.query.pop();
                        this.saved_message = None;
                        cx.notify();
                    }
                    "escape" => {
                        this.query.clear();
                        this.saved_message = None;
                        cx.notify();
                    }
                    "enter" => this.open_first_match(window, cx),
                    "space" => {
                        this.query.push(' ');
                        this.saved_message = None;
                        cx.notify();
                    }
                    _ => {
                        if let Some(character) = &event.keystroke.key_char {
                            this.query.push_str(character);
                            this.saved_message = None;
                            cx.notify();
                        }
                    }
                }
            }))
            .child(tab_bar)
            .child(content)
    }
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
        FormKind::Server => "new server",
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
        FormKind::Server | FormKind::Database => "Tab next · Enter save · Esc cancel",
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
        for kind in [FormKind::Server, FormKind::Database, FormKind::Sql] {
            let form = FormState::new(kind);
            assert_eq!(form.fields.len(), form.labels().len());
            assert_eq!(form.active, 0);
        }
        let server = FormState::new(FormKind::Server);
        assert_eq!(server.fields[1], "local");
        assert_eq!(server.fields[6], ".");
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
        let mut form = FormState::new(FormKind::Server);
        for character in "dev".chars() {
            let character = character.to_string();
            assert_eq!(
                form.handle_key("", Some(&character)),
                FormKeyOutcome::Continue
            );
        }
        form.handle_key("tab", None);
        for _ in 0..5 {
            form.handle_key("backspace", None);
        }
        for character in "local".chars() {
            let character = character.to_string();
            form.handle_key("", Some(&character));
        }
        for _ in 0..4 {
            form.handle_key("tab", None);
        }
        for character in "/tmp/settings-target".chars() {
            let character = character.to_string();
            form.handle_key("", Some(&character));
        }

        assert_eq!(form.fields[0], "dev");
        assert_eq!(form.fields[1], "local");
        assert_eq!(form.fields[5], "/tmp/settings-target");
        assert_eq!(form.active, 5);
        assert_eq!(form.handle_key("enter", None), FormKeyOutcome::Save);
        assert_eq!(form.handle_key("escape", None), FormKeyOutcome::Cancel);

        let mut spaced = FormState::new(FormKind::Server);
        for character in "my".chars() {
            let character = character.to_string();
            spaced.handle_key("", Some(&character));
        }
        spaced.handle_key("space", None);
        for character in "server".chars() {
            let character = character.to_string();
            spaced.handle_key("", Some(&character));
        }
        assert_eq!(spaced.fields[0], "my server");
        spaced.handle_key("up", None);
        assert_eq!(spaced.active, spaced.fields.len() - 1);
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
}
