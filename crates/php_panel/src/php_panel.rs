use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};

use anyhow::Result;
use gpui::{
    Anchor, App, AppContext, AsyncWindowContext, ClickEvent, Context, Entity, EventEmitter,
    TextStyle,
    FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render,
    StatefulInteractiveElement, Styled, Subscription, UniformListScrollHandle, WeakEntity, Window,
    AnyWindowHandle, SharedString, TitlebarOptions, WindowBounds, WindowOptions, actions, px, size,
    uniform_list,
};
use editor::{Editor, EditorElement, EditorStyle, actions as editor_actions};
use project::{Project, ProjectPath};
use regex::Regex;
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
    [ToggleFocus, OpenDeploymentDialog, BrowseRemoteHost, SelectNextTab, SelectPrevTab]
);

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
    port: Option<String>,
    user: Option<String>,
    remote_root: Option<String>,
    local_root: Option<String>,
    web_url: Option<String>,
    excluded: Vec<String>,
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
    remote_root: String,
    local_root: String,
    web_url: String,
    excluded: String,
}

impl DialogBuffer {
    fn load(server: Option<&RemoteServer>) -> Self {
        match server {
            Some(server) => Self {
                name: server.name.clone(),
                host: server.host.clone().unwrap_or_default(),
                port: server.port.clone().unwrap_or_default(),
                user: server.user.clone().unwrap_or_default(),
                remote_root: server.remote_root.clone().unwrap_or_default(),
                local_root: server.local_root.clone().unwrap_or_default(),
                web_url: server.web_url.clone().unwrap_or_default(),
                excluded: excluded_text(Some(server.excluded.as_slice())),
            },
            None => Self::default(),
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
    name_editor: Entity<Editor>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    user_editor: Entity<Editor>,
    remote_root_editor: Entity<Editor>,
    local_root_editor: Entity<Editor>,
    web_url_editor: Entity<Editor>,
    excluded_editor: Entity<Editor>,
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
            name_editor: cx.new(|cx| Editor::single_line(window, cx)),
            host_editor: cx.new(|cx| Editor::single_line(window, cx)),
            port_editor: cx.new(|cx| Editor::single_line(window, cx)),
            user_editor: cx.new(|cx| Editor::single_line(window, cx)),
            remote_root_editor: cx.new(|cx| Editor::single_line(window, cx)),
            local_root_editor: cx.new(|cx| Editor::single_line(window, cx)),
            web_url_editor: cx.new(|cx| Editor::single_line(window, cx)),
            excluded_editor: cx.new(|cx| Editor::single_line(window, cx)),
            _subscriptions: Vec::new(),
        };
        dialog.seed_from_disk(window, cx);
        dialog
    }

    fn seed_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers = load_deploy_servers(&self.root);
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
        set(&self.port_editor, buffer.port.clone());
        set(&self.user_editor, buffer.user.clone());
        set(&self.remote_root_editor, buffer.remote_root.clone());
        set(&self.local_root_editor, buffer.local_root.clone());
        set(&self.web_url_editor, buffer.web_url.clone());
        set(&self.excluded_editor, buffer.excluded.clone());
    }

    fn buffer(&self, cx: &App) -> DialogBuffer {
        let text = |editor: &Entity<Editor>| editor.read(cx).text(cx);
        DialogBuffer {
            name: text(&self.name_editor),
            host: text(&self.host_editor),
            port: text(&self.port_editor),
            user: text(&self.user_editor),
            remote_root: text(&self.remote_root_editor),
            local_root: text(&self.local_root_editor),
            web_url: text(&self.web_url_editor),
            excluded: text(&self.excluded_editor),
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
        self.load_editors(window, cx);
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
        cx.notify();
    }

    fn add_server(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_editors(cx);
        dialog_add_server(&mut self.servers, &self.root, &mut self.next_number);
        self.selected = Some(self.servers.len() - 1);
        self.tab = DeployTab::Connection;
        self.message = None;
        self.load_editors(window, cx);
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
        self.load_editors(window, cx);
        cx.notify();
    }

    fn toggle_protocol(&mut self, cx: &mut Context<Self>) {
        self.protocol = if self.protocol == "local" {
            "sftp".to_string()
        } else {
            "local".to_string()
        };
        cx.notify();
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
        let content = serde_json::json!({
            "servers": [deploy_server_entry(&server)],
            "active": server.name,
        });
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

fn dialog_editor_box(
    id: &'static str,
    editor: &Entity<Editor>,
    width: Option<Pixels>,
    cx: &App,
) -> AnyElement {
    let colors = cx.theme().colors();
    let mut box_div = div()
        .id(id)
        .flex_1()
        .min_w_0()
        .bg(colors.editor_background)
        .border_1()
        .border_color(colors.border_variant)
        .rounded_sm()
        .px_1()
        .py_0p5()
        .cursor_text()
        .child(dialog_editor_element(editor, cx));
    if let Some(width) = width {
        box_div = box_div.flex_none().w(width);
    }
    box_div.into_any_element()
}

fn dialog_field(
    id: &'static str,
    label: &'static str,
    editor: &Entity<Editor>,
    width: Option<Pixels>,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .flex_none()
        .items_center()
        .gap_2()
        .py_1()
        .child(
            div()
                .flex_none()
                .w(px(90.))
                .child(
                    Label::new(label)
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
        )
        .child(dialog_editor_box(id, editor, width, cx))
        .into_any_element()
}

impl Render for DeploymentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let protocol = self.protocol.clone();
        let message = self.message.clone();

        let mut server_rows = Vec::new();
        for (index, server) in self.servers.iter().enumerate() {
            let is_selected = self.selected == Some(index);
            let badge = server
                .protocol
                .clone()
                .unwrap_or_else(|| "server".to_string());
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
                    .hover(|style| style.bg(colors.element_hover))
                    .child(
                        Label::new(badge)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted)
                            .flex_none(),
                    )
                    .child(
                        Label::new(server.name.clone())
                            .size(LabelSize::XSmall)
                            .color(if is_selected {
                                Color::Accent
                            } else {
                                Color::Default
                            })
                            .truncate(),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.select_server(index, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if server_rows.is_empty() {
            server_rows.push(
                h_flex()
                    .px_1()
                    .py_0p5()
                    .child(
                        Label::new("no servers")
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .into_any_element(),
            );
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

        let type_button = Button::new("deploy-type", self.protocol.clone())
            .size(ButtonSize::Compact)
            .label_size(LabelSize::XSmall)
            .style(ButtonStyle::Subtle)
            .end_icon(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.toggle_protocol(cx);
            }))
            .into_any_element();

        let test_button = Button::new("deploy-test", "Test Connection")
            .size(ButtonSize::Compact)
            .label_size(LabelSize::XSmall)
            .style(ButtonStyle::Subtle)
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.test_connection(cx);
            }));

        let test_row = match message.as_ref().filter(|_| self.tab == DeployTab::Connection) {
            Some((text, color)) => h_flex()
                .flex_none()
                .items_center()
                .gap_2()
                .px_1()
                .py_1()
                .child(test_button)
                .child(
                    Label::new(text.clone())
                        .size(LabelSize::XSmall)
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
        };

        let tab_content: AnyElement = match self.tab {
            DeployTab::Connection => v_flex()
                .id("deploy-tab-connection")
                .w_full()
                .children(vec![
                    dialog_field(
                        "deploy-name-editor",
                        "Name:",
                        &self.name_editor,
                        None,
                        cx,
                    ),
                    h_flex()
                        .w_full()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .child(
                            div().flex_none().w(px(90.)).child(
                                Label::new("Type:")
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                        )
                        .child(type_button)
                        .into_any_element(),
                    dialog_field(
                        "deploy-host-editor",
                        "Host:",
                        &self.host_editor,
                        None,
                        cx,
                    ),
                    dialog_field(
                        "deploy-port-editor",
                        "Port:",
                        &self.port_editor,
                        Some(px(60.)),
                        cx,
                    ),
                    dialog_field(
                        "deploy-user-editor",
                        "Username:",
                        &self.user_editor,
                        None,
                        cx,
                    ),
                    h_flex()
                        .flex_none()
                        .items_center()
                        .gap_2()
                        .py_1()
                        .px_1()
                        .child(
                            Label::new("password saved permanently")
                                .size(LabelSize::XSmall)
                                .color(Color::Muted),
                        )
                        .into_any_element(),
                    test_row,
                    dialog_field(
                        "deploy-root-editor",
                        "Root path:",
                        &self.remote_root_editor,
                        None,
                        cx,
                    ),
                    dialog_field(
                        "deploy-url-editor",
                        "Web server URL:",
                        &self.web_url_editor,
                        None,
                        cx,
                    ),
                ])
                .into_any_element(),
            DeployTab::Mappings => v_flex()
                .id("deploy-tab-mappings")
                .w_full()
                .children(vec![
                    dialog_field(
                        "deploy-local-root-editor",
                        "Local path:",
                        &self.local_root_editor,
                        None,
                        cx,
                    ),
                    dialog_field(
                        "deploy-deploy-path-editor",
                        "Deployment path:",
                        &self.remote_root_editor,
                        None,
                        cx,
                    ),
                    dialog_field(
                        "deploy-web-path-editor",
                        "Web path:",
                        &self.web_url_editor,
                        None,
                        cx,
                    ),
                ])
                .into_any_element(),
            DeployTab::Excluded => v_flex()
                .id("deploy-tab-excluded")
                .w_full()
                .gap_1()
                .child(
                    Label::new("excluded paths, comma separated")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .child(dialog_editor_box(
                    "deploy-excluded-editor",
                    &self.excluded_editor,
                    None,
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
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        Button::new("deploy-cancel", "Cancel")
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Small)
                            .style(ButtonStyle::Subtle)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.cancel(window, cx);
                            })),
                    )
                    .child(
                        Button::new("deploy-ok", "OK")
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
        let mut files = Vec::new();
        let mut http_requests = Vec::new();
        let mut collections = Vec::new();
        let mut remote_servers = Vec::new();
        let mut deploy_config = None;
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

        let names = self
            .remote_servers
            .iter()
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
        self.active_server
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

        if kind == FormKind::Sql {
            return;
        }
        let saved = save_database_config(&root, &fields);

        match saved {
            Ok(name) => {
                self.form = None;
                self.query.clear();
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
    Some(RemoteServer {
        name,
        protocol: string_field(server, "protocol"),
        host: string_field(server, "host"),
        port: port_field(server),
        user: string_field(server, "user"),
        remote_root: string_field(server, "remote_root"),
        local_root: string_field(server, "local_root"),
        web_url: string_field(server, "web_url"),
        excluded: excluded_paths(server),
        upload_on_save: server
            .get("upload_on_save")
            .and_then(Value::as_bool)
            .unwrap_or(false),
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

fn blank_server(root: PathBuf) -> RemoteServer {
    RemoteServer {
        name: String::new(),
        protocol: Some("local".to_string()),
        host: None,
        port: None,
        user: None,
        remote_root: None,
        local_root: None,
        web_url: None,
        excluded: Vec::new(),
        upload_on_save: false,
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
            if !protocol.is_empty() && !SERVER_PROTOCOLS.contains(&protocol) {
                return Some(format!("\"{}\" protocol must be local or sftp", server.name));
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
        "sqlite" => "docs",
        "sqlserver" => "msr",
        "oracle" => "orc",
        "mongodb" => "mdb",
        "redis" => "sh",
        _ => "?",
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
        let active_name = self.active_server.clone();

        let picker_servers = self.remote_servers.clone();
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
            let haystack = format!(
                "{} {} {}",
                database.name,
                display_field(&database.driver),
                display_field(&database.target),
            )
            .to_lowercase();
            let connection_matches = query.is_empty() || haystack.contains(&query);

            let expanded = self.db_expanded_conns.contains(&database.name);

            let mut schema_rows = Vec::new();
            let mut schema_matches = false;
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

            if !connection_matches && !schema_matches {
                continue;
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
                        Label::new("Data Source")
                            .size(LabelSize::Small)
                            .color(Color::Accent),
                    )
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.open_form(FormKind::Database, window, cx);
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let modifiers = &event.keystroke.modifiers;
                if this.form.is_none()
                    && modifiers.control
                    && modifiers.alt
                    && !modifiers.shift
                    && !modifiers.platform
                    && !modifiers.function
                {
                    if let Ok(index) = event.keystroke.key.parse::<usize>() {
                        if (1..=6).contains(&index) {
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
                                let servers = panel.remote_servers.clone();
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
                    remote_root: Some("remote".to_string()),
                    local_root: None,
                    web_url: None,
                    excluded: Vec::new(),
                    upload_on_save: false,
                    root: root.clone(),
                },
                RemoteServer {
                    name: "sftp".to_string(),
                    protocol: Some("sftp".to_string()),
                    host: Some("127.0.0.1".to_string()),
                    port: Some("2222".to_string()),
                    user: Some("deploy".to_string()),
                    remote_root: Some("/var/www/app".to_string()),
                    local_root: Some(".".to_string()),
                    web_url: None,
                    excluded: Vec::new(),
                    upload_on_save: false,
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
        assert_eq!(driver_badge_short("sqlite"), "docs");
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
    fn next_tab_advances_cyclically() {
        assert_eq!(next_tab(PanelTab::Files, 1), PanelTab::Remote);
        assert_eq!(next_tab(PanelTab::Collections, 1), PanelTab::Files);
        assert_eq!(next_tab(PanelTab::Files, -1), PanelTab::Collections);
        assert_eq!(next_tab(PanelTab::Collections, -1), PanelTab::Http);
        for tab in PanelTab::ALL {
            assert_eq!(next_tab(tab, 6), tab);
        }
    }
}
