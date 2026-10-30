//! PhpStorm-like database table data editor, opened as a workspace tab.
//!
//! Clicking a table (or view) in the Databases tab opens this view next to
//! regular file tabs. Rows load through the `php-db` CLI. SQLite tables are
//! editable: committing an edited cell runs a single-row UPDATE (`where
//! rowid = …`) and reloads the current page. Views and non-SQLite drivers
//! open read-only.

use crate::{PANEL_TEXT_REMS, display_value, first_non_empty_line, parse_table_rows};
use editor::{Editor, EditorElement, EditorEvent, EditorStyle, actions as editor_actions};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use menu::Confirm;
use serde_json::Value;
use std::path::{Path, PathBuf};
use ui::{
    Color, Icon, IconButton, IconButtonShape, IconName, IconSize, Label, LabelSize, Tooltip,
    prelude::*,
};
use util::ResultExt as _;
use workspace::Item;
use workspace::item::ItemEvent;

/// Hidden column alias carrying sqlite `rowid` so UPDATEs can target a row.
const ROWID_COLUMN: &str = "__phpwb_rowid";
const CELL_WIDTH: f32 = 200.0;
const ROW_NUMBER_WIDTH: f32 = 56.0;
const ROW_HEIGHT: f32 = 26.0;
const HEADER_HEIGHT: f32 = 34.0;
const PAGE_SIZE: usize = 50;

/// A table to open as a tab.
pub struct TableTabTarget {
    pub connection: String,
    pub table: String,
    pub root: PathBuf,
    pub columns: Vec<crate::DatabaseColumn>,
    /// Editable candidate: sqlite driver and a table, not a view.
    pub editable_candidate: bool,
}

pub struct DatabaseTableView {
    connection: String,
    table: String,
    root: PathBuf,
    columns: Vec<crate::DatabaseColumn>,
    rows: Vec<serde_json::Map<String, Value>>,
    /// Whether the table was offered as an editable candidate (drives the
    /// rowid probe; a failed probe falls back to read-only).
    editable_candidate: bool,
    /// Whether the loaded rows carry the hidden rowid column and may be edited.
    editable: bool,
    status: LoadStatus,
    page: usize,
    total: Option<u64>,
    message: Option<(SharedString, Color)>,
    busy: bool,
    focus_handle: FocusHandle,
    editing: Option<CellEdit>,
}

struct CellEdit {
    row: usize,
    column: String,
    rowid: Value,
    original_text: String,
    original_is_null: bool,
    editor: Entity<Editor>,
    _subscription: Subscription,
}

enum LoadStatus {
    Loading,
    Loaded,
    Failed(String),
}

impl DatabaseTableView {
    pub fn open(target: TableTabTarget, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let focus_handle = cx.focus_handle();
            let mut this = Self {
                connection: target.connection,
                table: target.table,
                root: target.root,
                columns: target.columns,
                rows: Vec::new(),
                editable_candidate: target.editable_candidate,
                editable: false,
                status: LoadStatus::Loading,
                page: 0,
                total: None,
                message: None,
                busy: false,
                focus_handle,
                editing: None,
            };
            this.load(cx);
            this
        })
    }

    /// Reloads the current page (count + rows) in the background.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.status = LoadStatus::Loading;
        self.editing = None;
        cx.notify();

        let connection = self.connection.clone();
        let table = self.table.clone();
        let root = self.root.clone();
        let offset = (self.page * PAGE_SIZE) as u64;
        let editable_candidate = self.editable_candidate;

        let task = cx.background_spawn(async move {
            let total = run_query_rows(
                &root,
                &connection,
                &format!("select count(*) as \"n\" from {}", quote_ident(&table)),
                false,
            )?
            .first()
            .and_then(|row| row.get("n"))
            .and_then(count_value);

            let page_sql = format!(
                "select * from {} limit {} offset {}",
                quote_ident(&table),
                PAGE_SIZE,
                offset
            );
            let rowid_sql = format!(
                "select rowid as \"{}\", * from {} limit {} offset {}",
                ROWID_COLUMN,
                quote_ident(&table),
                PAGE_SIZE,
                offset
            );
            let (rows, editable) = match run_query_rows(&root, &connection, &rowid_sql, false) {
                Ok(rows) => (rows, true),
                Err(_) => (run_query_rows(&root, &connection, &page_sql, false)?, false),
            };
            Ok::<_, String>((total, rows, editable && editable_candidate))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((total, rows, editable)) => {
                        this.total = total;
                        this.rows = rows;
                        this.editable = editable;
                        this.status = LoadStatus::Loaded;
                    }
                    Err(message) => {
                        this.rows.clear();
                        this.status = LoadStatus::Failed(message);
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn start_editing(
        &mut self,
        row_index: usize,
        column: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || !self.editable {
            return;
        }
        if self
            .editing
            .as_ref()
            .is_some_and(|edit| edit.row == row_index && edit.column == column)
        {
            return;
        }
        let Some(row) = self.rows.get(row_index) else {
            return;
        };
        let Some(rowid) = row.get(ROWID_COLUMN).cloned() else {
            return;
        };
        let value = row.get(&column).cloned().unwrap_or(Value::Null);
        let original_text = display_value(&value);
        let original_is_null = value.is_null();

        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            if !original_is_null {
                editor.set_text(original_text.clone(), window, cx);
            }
            editor
        });
        editor.update(cx, |editor, cx| {
            editor.focus_handle(cx).focus(window, cx);
        });
        let subscription = cx.subscribe(&editor, |this, _editor, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Blurred) {
                this.commit_cell(cx);
            }
        });
        self.editing = Some(CellEdit {
            row: row_index,
            column,
            rowid,
            original_text,
            original_is_null,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    /// Applies the pending cell edit as a single-row UPDATE and reloads.
    fn commit_cell(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let text = edit.editor.read(cx).text(cx).trim().to_string();
        if text == edit.original_text || (edit.original_is_null && text.is_empty()) {
            cx.notify();
            return;
        }
        let sql = format!(
            "update {} set {} = {} where rowid = {}",
            quote_ident(&self.table),
            quote_ident(&edit.column),
            sql_text_literal(&text),
            display_value(&edit.rowid),
        );
        self.busy = true;
        cx.notify();

        let connection = self.connection.clone();
        let root = self.root.clone();
        let label = format!("{}: {}", self.table, edit.column);
        let task = cx.background_spawn(async move {
            run_query_rows(&root, &connection, &sql, true).map(|_| ())
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.message = Some((format!("updated: {label}").into(), Color::Created));
                        this.load(cx);
                    }
                    Err(message) => {
                        this.message = Some((message.into(), Color::Error));
                        cx.notify();
                    }
                }
            })
            .log_err();
        })
        .detach();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        let mut header = h_flex()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.surface_background)
            .child(
                h_flex()
                    .gap_1()
                    .flex_none()
                    .child(
                        Icon::new(IconName::Table)
                            .size(IconSize::XSmall)
                            .color(Color::Accent),
                    )
                    .child(Label::new(self.table.clone()).size(LabelSize::Custom(PANEL_TEXT_REMS))),
            )
            .child(
                Label::new(self.connection.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted)
                    .truncate(),
            )
            .child(div().flex_1());
        if !self.editable && matches!(self.status, LoadStatus::Loaded) {
            header = header.child(
                Label::new("read-only")
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            );
        }

        let from = if self.rows.is_empty() {
            0
        } else {
            self.page * PAGE_SIZE + 1
        };
        let to = self.page * PAGE_SIZE + self.rows.len();
        let range_text = match self.total {
            Some(total) => format!("{from}–{to} of {total}"),
            None => format!("{from}–{to}"),
        };
        header = header
            .child(
                Label::new(range_text)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            )
            .child(
                IconButton::new("table-prev-page", IconName::ChevronLeft)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(self.page == 0 || self.busy)
                    .tooltip(Tooltip::text("Previous page"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        if this.page > 0 {
                            this.page -= 1;
                            this.load(cx);
                        }
                    })),
            )
            .child(
                IconButton::new("table-next-page", IconName::ChevronRight)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(
                        self.busy
                            || match self.total {
                                Some(total) => (self.page + 1) * PAGE_SIZE >= total as usize,
                                None => self.rows.len() < PAGE_SIZE,
                            },
                    )
                    .tooltip(Tooltip::text("Next page"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.page += 1;
                        this.load(cx);
                    })),
            )
            .child(
                IconButton::new("table-reload", IconName::RotateCw)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(self.busy)
                    .tooltip(Tooltip::text("Reload"))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.load(cx);
                    })),
            );

        if let Some((message, color)) = &self.message {
            header = header.child(
                Label::new(message.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(*color)
                    .truncate(),
            );
        }
        header
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.status {
            LoadStatus::Loading => v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .child(
                    Label::new("loading rows…")
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Muted),
                )
                .into_any_element(),
            LoadStatus::Failed(message) => v_flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .child(
                    Label::new(message.clone())
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Error)
                        .truncate(),
                )
                .into_any_element(),
            LoadStatus::Loaded => {
                if self.rows.is_empty() {
                    return v_flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .child(
                            Label::new("no rows")
                                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                .color(Color::Muted),
                        )
                        .into_any_element();
                }
                let total_width = ROW_NUMBER_WIDTH + self.columns.len() as f32 * CELL_WIDTH;
                let mut grid = v_flex()
                    .w(px(total_width))
                    .flex_none()
                    .child(self.render_grid_header_row(cx));
                for (row_index, row) in self.rows.iter().enumerate() {
                    grid = grid.child(self.render_grid_row(row_index, row, cx));
                }
                let colors = cx.theme().colors();
                div()
                    .id("table-grid-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .bg(colors.editor_background)
                    .child(grid)
                    .into_any_element()
            }
        }
    }

    fn render_grid_header_row(&self, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let mut header = h_flex()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .bg(colors.element_background)
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .w(px(ROW_NUMBER_WIDTH))
                    .h_full()
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .child(
                        Label::new("#")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    ),
            );
        for column in &self.columns {
            let mut column_label = h_flex().gap_1().child(
                Label::new(column.name.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .truncate(),
            );
            if column.primary_key {
                column_label = column_label.child(
                    Label::new("pk")
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(Color::Warning),
                );
            }
            header = header.child(
                div()
                    .w(px(CELL_WIDTH))
                    .h_full()
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .border_r_1()
                    .border_color(colors.border)
                    .overflow_hidden()
                    .child(column_label),
            );
        }
        header.into_any_element()
    }

    fn render_grid_row(
        &self,
        row_index: usize,
        row: &serde_json::Map<String, Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let mut row_element = h_flex()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .border_b_1()
            .border_color(colors.border)
            .hover(|style| style.bg(colors.element_hover))
            .child(
                div()
                    .w(px(ROW_NUMBER_WIDTH))
                    .h_full()
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .child(
                        Label::new(format!("{}", self.page * PAGE_SIZE + row_index + 1))
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(Color::Muted),
                    ),
            );
        for (column_index, column) in self.columns.iter().enumerate() {
            row_element =
                row_element.child(self.render_cell(row_index, column_index, column, row, cx));
        }
        row_element.into_any_element()
    }

    fn render_cell(
        &self,
        row_index: usize,
        column_index: usize,
        column: &crate::DatabaseColumn,
        row: &serde_json::Map<String, Value>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let value = row.get(&column.name).cloned().unwrap_or(Value::Null);
        let cell = div()
            .id(SharedString::from(format!(
                "cell-{row_index}-{column_index}"
            )))
            .w(px(CELL_WIDTH))
            .h_full()
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .border_r_1()
            .border_color(colors.border)
            .hover(|style| style.bg(colors.element_hover));

        if let Some(edit) = &self.editing
            && edit.row == row_index
            && edit.column == column.name
        {
            return cell
                .bg(colors.editor_background)
                .child(
                    div()
                        .id("cell-editor")
                        .size_full()
                        .key_context("DatabaseTableCellEditor")
                        .on_action(cx.listener(|this, _: &Confirm, _window, cx| {
                            this.commit_cell(cx);
                        }))
                        .on_action(
                            cx.listener(|this, _: &editor_actions::Cancel, _window, cx| {
                                this.cancel_editing(cx);
                            }),
                        )
                        .child(cell_editor_element(&edit.editor, cx)),
                )
                .into_any_element();
        }

        let text = single_line_text(&display_value(&value));
        cell.child(
            Label::new(text)
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(if value.is_null() {
                    Color::Muted
                } else {
                    Color::Default
                })
                .truncate(),
        )
        .when(self.editable, |cell| {
            let column_name = column.name.clone();
            cell.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() > 1 {
                    this.start_editing(row_index, column_name.clone(), window, cx);
                }
            }))
        })
        .into_any_element()
    }
}

impl EventEmitter<ItemEvent> for DatabaseTableView {}

impl Focusable for DatabaseTableView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for DatabaseTableView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Table).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        format!("{} · {}", self.table, self.connection).into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} — table data from `{}`", self.table, self.connection).into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Database Table Opened")
    }

    fn to_item_events(_event: &ItemEvent, _f: &mut dyn FnMut(ItemEvent)) {}
}

impl Render for DatabaseTableView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        v_flex()
            .id("database-table-view")
            .size_full()
            .bg(colors.background)
            .text_ui(cx)
            .child(self.render_header(cx))
            .child(self.render_body(cx))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cell_editor_element(editor: &Entity<Editor>, cx: &App) -> AnyElement {
    let style = EditorStyle {
        background: cx.theme().colors().editor_background,
        local_player: cx.theme().players().local(),
        ..EditorStyle::default()
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .child(EditorElement::new(editor, style))
        .into_any_element()
}

fn single_line_text(text: &str) -> String {
    text.replace('\n', " ")
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn sql_text_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn count_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// Runs `php-db query <connection> <sql> --json` and returns row objects.
fn run_query_rows(
    root: &Path,
    connection: &str,
    sql: &str,
    allow_write: bool,
) -> Result<Vec<serde_json::Map<String, Value>>, String> {
    let mut command = std::process::Command::new("php-db");
    command.arg("query").arg(connection).arg(sql).arg("--json");
    if allow_write {
        command.arg("--allow-write");
    }
    command.current_dir(root);
    match command.output() {
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
}
