//! PhpStorm-like database table data editor, opened as a workspace tab.
//!
//! Clicking a table in the Databases tab opens this view next to regular file
//! tabs. Rows load through the `php-db` CLI (max 500 rows). SQLite tables are
//! editable: committing a cell runs a single-row `UPDATE` keyed by the primary
//! key, or by `rowid` when the table has no primary key. Rows can be added
//! (`INSERT`) and deleted (`DELETE`). Views, read-only connections and
//! non-SQLite drivers open read-only.

use crate::{DatabaseColumn, PANEL_TEXT_REMS, display_value, first_non_empty_line, parse_table_rows};
use editor::{Editor, EditorElement, EditorEvent, EditorStyle, actions as editor_actions};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use menu::Confirm;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use ui::{
    Color, Icon, IconButton, IconButtonShape, IconName, IconSize, Label, LabelSize, Tooltip,
    prelude::*,
};
use util::ResultExt as _;
use workspace::{Item, WorkspaceId};
use workspace::item::ItemEvent;

/// Hidden column alias carrying sqlite `rowid` so keyless rows can be targeted.
pub(crate) const ROWID_COLUMN: &str = "__rowid";
const CELL_WIDTH: f32 = 180.0;
const ROW_NUMBER_WIDTH: f32 = 48.0;
const ROW_HEIGHT: f32 = 24.0;
const TOOLBAR_HEIGHT: f32 = 34.0;
const ROW_LIMIT: usize = 500;

/// A table to open as a tab.
#[derive(Clone)]
pub struct TableTabTarget {
    pub connection: String,
    /// Human-facing connection label: sqlite file name when known.
    pub connection_label: String,
    pub table: String,
    pub root: PathBuf,
    pub columns: Vec<DatabaseColumn>,
    /// Editable candidate: a sqlite table (not a view).
    pub editable_candidate: bool,
    /// Connection is configured `read_only: true`.
    pub read_only: bool,
}

/// One grid row plus the hidden key used to target it.
#[derive(Clone)]
struct RowData {
    values: Map<String, Value>,
    rowid: Option<Value>,
    /// Appended locally by `+`, not yet present in the database.
    is_new: bool,
}

pub struct DatabaseTableView {
    connection: String,
    connection_label: String,
    table: String,
    root: PathBuf,
    columns: Vec<DatabaseColumn>,
    read_only: bool,
    editable_candidate: bool,
    rows: Vec<RowData>,
    editable: bool,
    /// The load selected `rowid` because the table has no primary key.
    rowid_mode: bool,
    status: LoadStatus,
    message: Option<(SharedString, Color)>,
    busy: bool,
    focus_handle: FocusHandle,
    editing: Option<CellEdit>,
    selected_cell: Option<(usize, usize)>,
    selected_row: Option<usize>,
}

struct CellEdit {
    row: usize,
    column: String,
    column_type: String,
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
                connection_label: target.connection_label,
                table: target.table,
                root: target.root,
                columns: target.columns,
                read_only: target.read_only,
                editable_candidate: target.editable_candidate && !target.read_only,
                rows: Vec::new(),
                editable: false,
                rowid_mode: false,
                status: LoadStatus::Loading,
                message: None,
                busy: false,
                focus_handle,
                editing: None,
                selected_cell: None,
                selected_row: None,
            };
            this.load(cx);
            this
        })
    }

    fn target(&self) -> TableTabTarget {
        TableTabTarget {
            connection: self.connection.clone(),
            connection_label: self.connection_label.clone(),
            table: self.table.clone(),
            root: self.root.clone(),
            columns: self.columns.clone(),
            editable_candidate: self.editable_candidate,
            read_only: self.read_only,
        }
    }

    fn primary_key(&self) -> Option<&DatabaseColumn> {
        self.columns.iter().find(|column| column.primary_key)
    }

    /// Reloads the first page of rows in the background.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.status = LoadStatus::Loading;
        self.editing = None;
        self.selected_cell = None;
        self.selected_row = None;
        cx.notify();

        let connection = self.connection.clone();
        let table = self.table.clone();
        let root = self.root.clone();
        let wants_rowid = rowid_fallback(&self.columns, self.editable_candidate);

        let task = cx.background_spawn(async move {
            let quoted = sql_quote_ident(&table);
            if wants_rowid {
                let rowid_sql = format!(
                    "select rowid as {} , * from {} limit {}",
                    sql_quote_ident(ROWID_COLUMN),
                    quoted,
                    ROW_LIMIT
                );
                match run_query(&root, &connection, &rowid_sql) {
                    Ok(rows) => return Ok((rows, true, true)),
                    Err(_) => {}
                }
            }
            let sql = format!("select * from {} limit {}", quoted, ROW_LIMIT);
            run_query(&root, &connection, &sql).map(|rows| (rows, false, false))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((rows, rowid_mode, rowid_ok)) => {
                        this.rowid_mode = rowid_mode;
                        this.rows = rows
                            .into_iter()
                            .map(|row| row_data_from_json(row, rowid_mode))
                            .collect();
                        if this.columns.is_empty() {
                            this.columns = derive_columns(&this.rows);
                        }
                        this.editable = this.editable_candidate
                            && (this.primary_key().is_some() || (rowid_mode && rowid_ok));
                        this.status = LoadStatus::Loaded;
                    }
                    Err(message) => {
                        this.rows.clear();
                        this.editable = false;
                        this.status = LoadStatus::Failed(message);
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.selected_cell = Some((row, column));
        self.selected_row = Some(row);
        cx.notify();
    }

    fn start_editing(
        &mut self,
        row_index: usize,
        column_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || !self.editable {
            return;
        }
        let Some(column) = self.columns.get(column_index).cloned() else {
            return;
        };
        let Some(row) = self.rows.get(row_index) else {
            return;
        };
        let value = row.values.get(&column.name).cloned().unwrap_or(Value::Null);
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
        // After the editor mounts, focus it and select the value so typing
        // replaces it, like PhpStorm.
        let deferred = editor.clone();
        cx.defer_in(window, move |this, window, cx| {
            let active = this
                .editing
                .as_ref()
                .is_some_and(|edit| edit.editor == deferred);
            if active {
                deferred.update(cx, |editor, cx| {
                    editor.focus_handle(cx).focus(window, cx);
                });
                window.dispatch_action(Box::new(editor_actions::SelectAll), cx);
            }
        });
        let subscription = cx.subscribe(&editor, |this, _editor, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Blurred) {
                this.on_editor_blur(cx);
            }
        });
        self.selected_cell = Some((row_index, column_index));
        self.selected_row = Some(row_index);
        self.editing = Some(CellEdit {
            row: row_index,
            column: column.name,
            column_type: column.column_type,
            original_text,
            original_is_null,
            editor,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// Enter on the selected cell starts editing (PhpStorm-like).
    fn edit_selected_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((row, column)) = self.selected_cell {
            self.start_editing(row, column, window, cx);
        }
    }

    fn on_editor_blur(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.as_ref() else {
            return;
        };
        if self.rows.get(edit.row).is_some_and(|row| row.is_new) {
            // Draft rows stage the typed value but are not inserted until Enter.
            self.stage_draft_cell(cx);
        } else {
            self.commit_cell(cx);
        }
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    /// Stores a draft-row cell value locally without touching the database.
    fn stage_draft_cell(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let text = edit.editor.read(cx).text(cx);
        if let Some(row) = self.rows.get_mut(edit.row) {
            if text.is_empty() {
                row.values.remove(&edit.column);
            } else {
                row.values.insert(edit.column, Value::String(text));
            }
        }
        cx.notify();
    }

    /// Applies the pending cell edit. Existing rows run UPDATE; draft rows run
    /// INSERT and require Enter (blur only stages the value).
    fn commit_cell(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let text = edit.editor.read(cx).text(cx);
        let is_new = self.rows.get(edit.row).is_some_and(|row| row.is_new);
        // Resolve the row key before mutating the draft value, so editing the
        // primary key column still targets the original row.
        let key = if is_new { None } else { self.row_key(edit.row) };

        if let Some(row) = self.rows.get_mut(edit.row) {
            if text.is_empty() {
                row.values.remove(&edit.column);
            } else {
                row.values.insert(edit.column.clone(), Value::String(text.clone()));
            }
        }

        if is_new {
            self.insert_draft_row(edit.row, cx);
            return;
        }

        if text == edit.original_text || (edit.original_is_null && text.is_empty()) {
            cx.notify();
            return;
        }

        let Some((key_column, key_value)) = key else {
            self.message = Some(("no key column for this row".into(), Color::Error));
            cx.notify();
            return;
        };
        let literal = if edit.original_is_null && text.is_empty() {
            "NULL".to_string()
        } else {
            sql_value_for_column(&edit.column_type, &text)
        };
        let sql = update_sql(
            &self.table,
            &edit.column,
            &literal,
            &key_column,
            &sql_literal_for_value(&key_value),
        );
        self.busy = true;
        self.message = None;
        cx.notify();

        let connection = self.connection.clone();
        let root = self.root.clone();
        let row_index = edit.row;
        let column = edit.column.clone();
        let saved_text = text.clone();
        let task = cx.background_spawn(async move { run_write(&root, &connection, &sql) });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if let Some(row) = this.rows.get_mut(row_index) {
                            if saved_text.is_empty() {
                                row.values.insert(column, Value::Null);
                            } else {
                                row.values.insert(column, Value::String(saved_text));
                            }
                        }
                        this.message = Some(("saved".into(), Color::Success));
                    }
                    Err(message) => {
                        this.message = Some((message.into(), Color::Error));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn insert_draft_row(&mut self, row_index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(row_index) else {
            return;
        };
        let columns = self.columns.clone();
        let sql = insert_sql(&self.table, &columns, &row.values);
        self.busy = true;
        self.message = None;
        cx.notify();

        let connection = self.connection.clone();
        let root = self.root.clone();
        let task = cx.background_spawn(async move { run_write(&root, &connection, &sql) });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.message = Some(("row added".into(), Color::Success));
                        this.load(cx);
                    }
                    Err(message) => {
                        // Keep the draft so the user can complete required fields.
                        this.message = Some((message.into(), Color::Error));
                        cx.notify();
                    }
                }
            })
            .log_err();
        })
        .detach();
    }

    fn add_row(&mut self, cx: &mut Context<Self>) {
        if !self.editable || self.busy {
            return;
        }
        self.rows.push(RowData {
            values: Map::new(),
            rowid: None,
            is_new: true,
        });
        let index = self.rows.len() - 1;
        self.selected_row = Some(index);
        self.selected_cell = Some((index, 0));
        self.message = Some((
            "new row — fill cells, Enter to insert".into(),
            Color::Muted,
        ));
        cx.notify();
    }

    fn delete_selected_row(&mut self, cx: &mut Context<Self>) {
        if !self.editable || self.busy {
            return;
        }
        let Some(index) = self.selected_row else {
            self.message = Some(("select a row first".into(), Color::Muted));
            cx.notify();
            return;
        };
        let Some(row) = self.rows.get(index) else {
            return;
        };
        if row.is_new {
            self.rows.remove(index);
            self.selected_row = None;
            self.selected_cell = None;
            self.message = Some(("draft row discarded".into(), Color::Muted));
            cx.notify();
            return;
        }
        let Some((key_column, key_value)) = self.row_key(index) else {
            self.message = Some(("no key column for this row".into(), Color::Error));
            cx.notify();
            return;
        };
        let sql = delete_sql(
            &self.table,
            &key_column,
            &sql_literal_for_value(&key_value),
        );
        self.busy = true;
        self.message = None;
        cx.notify();

        let connection = self.connection.clone();
        let root = self.root.clone();
        let task = cx.background_spawn(async move { run_write(&root, &connection, &sql) });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        if index < this.rows.len() {
                            this.rows.remove(index);
                        }
                        this.selected_row = None;
                        this.selected_cell = None;
                        this.message = Some(("deleted".into(), Color::Success));
                    }
                    Err(message) => {
                        this.message = Some((message.into(), Color::Error));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Key column and value that identify a row: the primary key, else rowid.
    fn row_key(&self, row_index: usize) -> Option<(String, Value)> {
        let row = self.rows.get(row_index)?;
        if let Some(pk) = self.primary_key() {
            let value = row.values.get(&pk.name).cloned()?;
            return Some((pk.name.clone(), value));
        }
        row.rowid.clone().map(|value| ("rowid".to_string(), value))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();

        let mut toolbar = h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(colors.border)
            .bg(colors.surface_background)
            .child(
                IconButton::new("table-refresh", IconName::RotateCw)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(self.busy)
                    .tooltip(Tooltip::text("Refresh"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.load(cx))),
            )
            .child(
                IconButton::new("table-add-row", IconName::Plus)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(!self.editable || self.busy)
                    .tooltip(Tooltip::text("Add row"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.add_row(cx))),
            )
            .child(
                IconButton::new("table-delete-row", IconName::Dash)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(!self.editable || self.busy || self.selected_row.is_none())
                    .tooltip(Tooltip::text("Delete selected row"))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _window, cx| {
                            this.delete_selected_row(cx)
                        }),
                    ),
            )
            .child(
                Label::new(format!("{} rows", self.rows.len()))
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            )
            .child(
                Label::new(format!("{} · {}", self.connection_label, self.table))
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted)
                    .truncate(),
            );

        if !self.editable && matches!(self.status, LoadStatus::Loaded) {
            toolbar = toolbar.child(
                Label::new("read-only")
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            );
        }

        toolbar = toolbar.child(div().flex_1());

        if let Some((message, color)) = &self.message {
            toolbar = toolbar.child(
                Label::new(message.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(*color)
                    .truncate(),
            );
        }
        toolbar.into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.status {
            LoadStatus::Loading => centered_note("loading rows…", Color::Muted),
            LoadStatus::Failed(message) => centered_note(message.clone(), Color::Error),
            LoadStatus::Loaded => {
                if self.rows.is_empty() {
                    return centered_note("no rows", Color::Muted);
                }
                let total_width = ROW_NUMBER_WIDTH + self.columns.len() as f32 * CELL_WIDTH;
                let editor_background = cx.theme().colors().editor_background;
                let mut body = v_flex().w(px(total_width)).flex_none();
                for (row_index, row) in self.rows.iter().enumerate() {
                    body = body.child(self.render_grid_row(row_index, row, cx));
                }
                // Horizontal scroll wraps a sticky header and a vertically
                // scrolling body so the header stays visible.
                div()
                    .id("table-grid-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .bg(editor_background)
                    .child(
                        v_flex()
                            .w(px(total_width))
                            .h_full()
                            .flex_none()
                            .child(self.render_grid_header_row(cx))
                            .child(
                                div()
                                    .id("table-grid-body")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(body),
                            ),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_grid_header_row(&self, cx: &App) -> AnyElement {
        let colors = cx.theme().colors();
        let mut header = h_flex()
            .h(px(ROW_HEIGHT + 4.0))
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
        row: &RowData,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let row_selected = self.selected_row == Some(row_index);
        let mut row_element = h_flex()
            .id(("table-row", row_index))
            .h(px(ROW_HEIGHT))
            .flex_none()
            .border_b_1()
            .border_color(colors.border)
            .when(row_selected, |this| this.bg(colors.element_selected))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.selected_row = Some(row_index);
                cx.notify();
            }))
            .child(
                div()
                    .w(px(ROW_NUMBER_WIDTH))
                    .h_full()
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .child(
                        Label::new(if row.is_new {
                            "·".to_string()
                        } else {
                            format!("{}", row_index + 1)
                        })
                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                        .color(if row.is_new { Color::Accent } else { Color::Muted }),
                    ),
            );
        for (column_index, column) in self.columns.iter().enumerate() {
            row_element = row_element.child(
                self.render_cell(row_index, column_index, column, row, cx),
            );
        }
        row_element.into_any_element()
    }

    fn render_cell(
        &self,
        row_index: usize,
        column_index: usize,
        column: &DatabaseColumn,
        row: &RowData,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let value = row.values.get(&column.name).cloned().unwrap_or(Value::Null);
        let selected = self.selected_cell == Some((row_index, column_index));
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
            .when(selected, |this| {
                this.bg(colors.element_hover)
                    .border_1()
                    .border_color(colors.border_focused)
            })
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
                        .capture_action(cx.listener(
                            |this, _: &editor_actions::Newline, _window, cx| {
                                this.commit_cell(cx);
                            },
                        ))
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
            cell.cursor_pointer()
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.focus_handle.focus(window, cx);
                    this.select_cell(row_index, column_index, cx);
                    if event.click_count() > 1 {
                        this.start_editing(row_index, column_index, window, cx);
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
        table_tab_title(&self.connection_label, &self.table).into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(format!("{} — table data from `{}`", self.table, self.connection).into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Database Table Opened")
    }

    fn can_split(&self) -> bool {
        true
    }

    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>>
    where
        Self: Sized,
    {
        Task::ready(Some(Self::open(self.target(), cx)))
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
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.editing.is_some() {
                    return;
                }
                if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                    this.edit_selected_cell(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "delete"
                    || (event.keystroke.key == "backspace"
                        && event.keystroke.modifiers.platform)
                {
                    this.delete_selected_row(cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.render_toolbar(cx))
            .child(self.render_body(cx))
    }
}

// ---------------------------------------------------------------------------
// Pure SQL / parsing helpers
// ---------------------------------------------------------------------------

/// Stable identity of a table tab: same connection + table must reuse the tab.
pub(crate) fn table_tab_key(connection: &str, table: &str) -> String {
    format!("{connection}\u{1f}{table}")
}

/// Tab label in PhpStorm form: `demo.sqlite/users` for file data sources, or
/// `app/users` for named connections.
pub(crate) fn table_tab_title(connection: &str, table: &str) -> String {
    format!("{connection}/{table}")
}

/// Quotes an identifier, doubling embedded quotes.
pub(crate) fn sql_quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Escapes a value for a single-quoted SQL string literal.
pub(crate) fn sql_escape(text: &str) -> String {
    text.replace('\'', "''")
}

pub(crate) fn sql_text_literal(text: &str) -> String {
    format!("'{}'", sql_escape(text))
}

/// SQL literal for a JSON value read from the database.
pub(crate) fn sql_literal_for_value(value: &Value) -> String {
    match value {
        Value::String(text) => sql_text_literal(text),
        Value::Null => "NULL".to_string(),
        other => other.to_string(),
    }
}

/// SQL literal for user-entered cell text, honouring numeric column affinity
/// so `INTEGER`/`REAL` columns receive unquoted numbers.
pub(crate) fn sql_value_for_column(column_type: &str, text: &str) -> String {
    let upper = column_type.to_ascii_uppercase();
    let numeric_type = ["INT", "REAL", "NUM", "DEC", "FLOAT", "DOUBLE"]
        .iter()
        .any(|needle| upper.contains(needle));
    if numeric_type && !text.is_empty() && text.parse::<f64>().is_ok() {
        return text.to_string();
    }
    sql_text_literal(text)
}

pub(crate) fn update_sql(
    table: &str,
    column: &str,
    literal: &str,
    key_column: &str,
    key_literal: &str,
) -> String {
    if key_column.eq_ignore_ascii_case("rowid") {
        format!(
            "update {} set {} = {} where rowid = {}",
            sql_quote_ident(table),
            sql_quote_ident(column),
            literal,
            key_literal
        )
    } else {
        format!(
            "update {} set {} = {} where {} = {}",
            sql_quote_ident(table),
            sql_quote_ident(column),
            literal,
            sql_quote_ident(key_column),
            key_literal
        )
    }
}

pub(crate) fn delete_sql(table: &str, key_column: &str, key_literal: &str) -> String {
    if key_column.eq_ignore_ascii_case("rowid") {
        format!(
            "delete from {} where rowid = {}",
            sql_quote_ident(table),
            key_literal
        )
    } else {
        format!(
            "delete from {} where {} = {}",
            sql_quote_ident(table),
            sql_quote_ident(key_column),
            key_literal
        )
    }
}

/// Builds an INSERT skipping columns whose draft value is empty. Falls back to
/// `DEFAULT VALUES` when every column is empty.
pub(crate) fn insert_sql(
    table: &str,
    columns: &[DatabaseColumn],
    values: &Map<String, Value>,
) -> String {
    let mut names = Vec::new();
    let mut literals = Vec::new();
    for column in columns {
        let Some(value) = values.get(&column.name) else {
            continue;
        };
        let text = match value {
            Value::Null => continue,
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        if text.is_empty() {
            continue;
        }
        names.push(sql_quote_ident(&column.name));
        literals.push(sql_value_for_column(&column.column_type, &text));
    }
    if names.is_empty() {
        format!("insert into {} default values", sql_quote_ident(table))
    } else {
        format!(
            "insert into {} ({}) values ({})",
            sql_quote_ident(table),
            names.join(", "),
            literals.join(", ")
        )
    }
}

/// Whether a load needs the hidden `rowid` key: usable sqlite table, no PK.
pub(crate) fn rowid_fallback(columns: &[DatabaseColumn], editable_candidate: bool) -> bool {
    editable_candidate && !columns.iter().any(|column| column.primary_key)
}

fn row_data_from_json(row: Map<String, Value>, rowid_mode: bool) -> RowData {
    let mut values = row;
    let rowid = if rowid_mode {
        values.remove(ROWID_COLUMN)
    } else {
        None
    };
    RowData {
        values,
        rowid,
        is_new: false,
    }
}

fn derive_columns(rows: &[RowData]) -> Vec<DatabaseColumn> {
    let mut seen = Vec::new();
    let mut columns = Vec::new();
    for row in rows {
        for name in row.values.keys() {
            if !seen.contains(name) {
                seen.push(name.clone());
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

// ---------------------------------------------------------------------------
// Rendering helpers
// ---------------------------------------------------------------------------

fn centered_note(text: impl Into<SharedString>, color: Color) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .child(
            Label::new(text.into())
                .size(LabelSize::Custom(PANEL_TEXT_REMS))
                .color(color)
                .truncate(),
        )
        .into_any_element()
}

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

// ---------------------------------------------------------------------------
// php-db invocation
// ---------------------------------------------------------------------------

fn config_path(root: &Path) -> PathBuf {
    root.join(".zed/databases.json")
}

/// Runs `php-db query … --json` and parses row objects.
fn run_query(
    root: &Path,
    connection: &str,
    sql: &str,
) -> Result<Vec<Map<String, Value>>, String> {
    let mut command = std::process::Command::new("php-db");
    command
        .arg("--config")
        .arg(config_path(root))
        .arg("query")
        .arg(connection)
        .arg(sql)
        .arg("--json")
        .current_dir(root);
    match command.output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            parse_table_rows(&stdout)
        }
        Ok(output) => Err(command_error(&output)),
        Err(error) => Err(error.to_string()),
    }
}

/// Runs a mutating statement and treats a zero exit as success. `php-db`
/// prints `no rows returned` for writes, which is not JSON.
fn run_write(root: &Path, connection: &str, sql: &str) -> Result<(), String> {
    let mut command = std::process::Command::new("php-db");
    command
        .arg("--config")
        .arg(config_path(root))
        .arg("query")
        .arg(connection)
        .arg(sql)
        .arg("--allow-write")
        .current_dir(root);
    match command.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(command_error(&output)),
        Err(error) => Err(error.to_string()),
    }
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    first_non_empty_line(stderr.lines().chain(stdout.lines()))
        .unwrap_or_else(|| "php-db command failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn columns() -> Vec<DatabaseColumn> {
        vec![
            DatabaseColumn {
                name: "id".into(),
                column_type: "INTEGER".into(),
                primary_key: true,
            },
            DatabaseColumn {
                name: "email".into(),
                column_type: "TEXT".into(),
                primary_key: false,
            },
        ]
    }

    #[test]
    fn escape_doubles_single_quotes() {
        assert_eq!(sql_escape("O'Brien"), "O''Brien");
        assert_eq!(sql_text_literal("a'b'c"), "'a''b''c'");
    }

    #[test]
    fn update_sql_uses_primary_key_identifier() {
        assert_eq!(
            update_sql("users", "email", "'new@example.com'", "id", "2"),
            "update \"users\" set \"email\" = 'new@example.com' where \"id\" = 2"
        );
    }

    #[test]
    fn update_sql_uses_rowid_without_identifier_quotes() {
        assert_eq!(
            update_sql("users", "email", "'x'", "rowid", "7"),
            "update \"users\" set \"email\" = 'x' where rowid = 7"
        );
    }

    #[test]
    fn insert_sql_skips_empty_columns_and_keeps_number_affinity() {
        let mut values = Map::new();
        values.insert("id".into(), json!("99"));
        values.insert("email".into(), json!("new@example.com"));
        values.insert("created_at".into(), json!(""));
        assert_eq!(
            insert_sql("users", &columns(), &values),
            "insert into \"users\" (\"id\", \"email\") values (99, 'new@example.com')"
        );
    }

    #[test]
    fn insert_sql_falls_back_to_default_values() {
        let mut values = Map::new();
        values.insert("id".into(), json!(""));
        assert_eq!(
            insert_sql("users", &columns(), &values),
            "insert into \"users\" default values"
        );
    }

    #[test]
    fn delete_sql_targets_pk_or_rowid() {
        assert_eq!(
            delete_sql("users", "id", "99"),
            "delete from \"users\" where \"id\" = 99"
        );
        assert_eq!(
            delete_sql("users", "rowid", "4"),
            "delete from \"users\" where rowid = 4"
        );
    }

    #[test]
    fn rowid_fallback_only_for_editable_keyless_tables() {
        assert!(!rowid_fallback(&columns(), true));
        assert!(rowid_fallback(&[], true));
        assert!(!rowid_fallback(&[], false));
    }

    #[test]
    fn parses_rows_and_extracts_hidden_rowid() {
        let stdout = r#"[
            {"__rowid": 1, "id": 1, "email": "alice@example.com"},
            {"__rowid": 2, "id": 2, "email": null}
        ]"#;
        let rows = parse_table_rows(stdout).expect("parse rows");
        let parsed = rows
            .into_iter()
            .map(|row| row_data_from_json(row, true))
            .collect::<Vec<_>>();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].rowid, Some(json!(1)));
        assert!(!parsed[0].values.contains_key(ROWID_COLUMN));
        assert_eq!(parsed[0].values.get("email"), Some(&json!("alice@example.com")));
        assert_eq!(parsed[1].values.get("email"), Some(&Value::Null));
    }

    #[test]
    fn table_tab_title_and_key_are_stable() {
        assert_eq!(table_tab_title("demo.sqlite", "users"), "demo.sqlite/users");
        assert_eq!(table_tab_key("demo.sqlite", "users"), "demo.sqlite\u{1f}users");
        assert_ne!(
            table_tab_key("demo.sqlite", "users"),
            table_tab_key("demo.sqlite", "orders")
        );
        assert_ne!(
            table_tab_key("demo.sqlite", "users"),
            table_tab_key("app", "users")
        );
    }

    #[test]
    fn sql_literal_for_value_quotes_strings_only() {
        assert_eq!(sql_literal_for_value(&json!(1)), "1");
        assert_eq!(sql_literal_for_value(&Value::Null), "NULL");
        assert_eq!(sql_literal_for_value(&json!("it's")), "'it''s'");
    }
}
