//! PhpStorm-like database table data editor, opened as a workspace tab.
//!
//! Clicking a table in the Databases tab opens this view next to regular file
//! tabs. Rows load through the `php-db` CLI (max 500 rows). SQLite tables are
//! editable: committing a cell runs a single-row `UPDATE` keyed by the primary
//! key, or by `rowid` when the table has no primary key. Rows can be added
//! (`INSERT`) and deleted (`DELETE`). Views, read-only connections and
//! non-SQLite drivers open read-only.

use crate::{
    DatabaseColumn, PANEL_TEXT_REMS, dialog_text_style, display_value, first_non_empty_line,
    parse_table_rows,
};
use editor::{Editor, EditorElement, EditorEvent, EditorStyle, actions as editor_actions};
use gpui::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent,
    ListSizingBehavior, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, Task, UniformListScrollHandle, Window, div,
    px, uniform_list,
};
use menu::Confirm;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use ui::{
    Color, ContextMenu, ContextMenuEntry, Icon, IconButton, IconButtonShape, IconName,
    IconSize, Label, LabelSize, PopoverMenu, ScrollAxes, Scrollbars, Tooltip,
    WithScrollbar,
    prelude::*,
};
use util::ResultExt as _;
use workspace::{Item, WorkspaceId};
use workspace::item::ItemEvent;

/// Hidden column alias carrying sqlite `rowid` so keyless rows can be targeted.
pub(crate) const ROWID_COLUMN: &str = "__rowid";
pub(crate) const PAGE_SIZES: &[usize] = &[10, 25, 50, 100, 250, 500];
const CELL_WIDTH: f32 = 180.0;
const ROW_NUMBER_WIDTH: f32 = 48.0;
const ROW_HEIGHT: f32 = 24.0;
const TOOLBAR_HEIGHT: f32 = 34.0;
const DEFAULT_PAGE_SIZE: usize = 50;

/// A table to open as a tab.
#[derive(Clone)]
pub struct TableTabTarget {
    pub connection: String,
    /// Human-facing connection label: sqlite file name when known.
    pub connection_label: String,
    pub table: String,
    pub driver: Option<String>,
    pub root: PathBuf,
    pub columns: Vec<DatabaseColumn>,
    /// Editable candidate: a table (not a view).
    pub editable_candidate: bool,
    /// Connection is configured `read_only: true`.
    pub read_only: bool,
}

/// One grid row plus the hidden key used to target it.
#[derive(Clone)]
struct RowData {
    values: Map<String, Value>,
    formatted_cells: Vec<SharedString>,
    is_null_cells: Vec<bool>,
    rowid: Option<Value>,
    /// Appended locally by `+`, not yet present in the database.
    is_new: bool,
}

impl RowData {
    fn format_for_columns(&mut self, columns: &[DatabaseColumn]) {
        self.formatted_cells.clear();
        self.is_null_cells.clear();
        self.formatted_cells.reserve(columns.len());
        self.is_null_cells.reserve(columns.len());
        for col in columns {
            let val = self.values.get(&col.name);
            let is_null = val.map_or(true, |v| v.is_null());
            let text = match val {
                Some(Value::String(s)) => single_line_text(s),
                Some(Value::Null) | None => "NULL".to_string(),
                Some(other) => single_line_text(&display_value(other)),
            };
            self.formatted_cells.push(SharedString::from(text));
            self.is_null_cells.push(is_null);
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum ExportFormat {
    Csv,
    Tsv,
    Json,
    SqlInserts,
    Markdown,
    Html,
}

impl ExportFormat {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Csv => "CSV",
            Self::Tsv => "TSV",
            Self::Json => "JSON",
            Self::SqlInserts => "SQL Inserts",
            Self::Markdown => "Markdown",
            Self::Html => "HTML",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PendingCellUpdate {
    row_index: usize,
    column: String,
    column_type: String,
    original_value: Value,
    new_value: Value,
    original_text: String,
    new_text: String,
    key_column: String,
    key_value: Value,
}

pub struct DatabaseTableView {
    connection: String,
    connection_label: String,
    table: String,
    driver: Option<String>,
    root: PathBuf,
    columns: Vec<DatabaseColumn>,
    read_only: bool,
    editable_candidate: bool,
    rows: Vec<RowData>,
    original_rows: Vec<RowData>,
    pending_updates: HashMap<(usize, String), PendingCellUpdate>,
    pending_deletions: HashSet<usize>,
    editable: bool,
    /// Current 0-based page index.
    page: usize,
    /// Rows to fetch per page (default 50).
    page_size: usize,
    /// The load selected `rowid` because the table has no primary key.
    rowid_mode: bool,
    status: LoadStatus,
    message: Option<(SharedString, Color)>,
    busy: bool,
    focus_handle: FocusHandle,
    vertical_scroll_handle: UniformListScrollHandle,
    horizontal_scroll_handle: ScrollHandle,
    editing: Option<CellEdit>,
    selected_cell: Option<(usize, usize)>,
    selected_row: Option<usize>,
    filter_where: Option<String>,
    filter_order_by: Option<String>,
    where_editor: Option<Entity<Editor>>,
    order_by_editor: Option<Entity<Editor>>,
    tx_manual: bool,
    export_format: ExportFormat,
    ddl_modal: Option<String>,
    preview_modal: Option<String>,
    last_executed_sql: Option<String>,
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
            let vertical_scroll_handle = UniformListScrollHandle::new();
            let horizontal_scroll_handle = ScrollHandle::new();
            let mut this = Self {
                connection: target.connection,
                connection_label: target.connection_label,
                table: target.table,
                driver: target.driver,
                root: target.root,
                columns: target.columns,
                read_only: target.read_only,
                editable_candidate: target.editable_candidate && !target.read_only,
                rows: Vec::new(),
                original_rows: Vec::new(),
                pending_updates: HashMap::new(),
                pending_deletions: HashSet::new(),
                editable: false,
                page: 0,
                page_size: DEFAULT_PAGE_SIZE,
                rowid_mode: false,
                status: LoadStatus::Loading,
                message: None,
                busy: false,
                focus_handle,
                vertical_scroll_handle,
                horizontal_scroll_handle,
                editing: None,
                selected_cell: None,
                selected_row: None,
                filter_where: None,
                filter_order_by: None,
                where_editor: None,
                order_by_editor: None,
                tx_manual: false,
                export_format: ExportFormat::Csv,
                ddl_modal: None,
                preview_modal: None,
                last_executed_sql: None,
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
            driver: self.driver.clone(),
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
        let driver = self.driver.clone();
        let root = self.root.clone();
        let page = self.page;
        let page_size = self.page_size;
        let offset = page * page_size;
        let is_sqlite = driver.as_deref().map_or(true, |d| {
            let d = d.to_ascii_lowercase();
            d.contains("sqlite")
        });
        let wants_rowid = is_sqlite && rowid_fallback(&self.columns, self.editable_candidate);
        let columns_empty = self.columns.is_empty();

        let where_clause = self.filter_where.as_ref().and_then(|w| {
            let t = w.trim();
            if t.is_empty() {
                None
            } else if t.to_ascii_uppercase().starts_with("WHERE ") {
                Some(t.to_string())
            } else {
                Some(format!("WHERE {}", t))
            }
        });

        let order_by_clause = self.filter_order_by.as_ref().and_then(|o| {
            let t = o.trim();
            if t.is_empty() {
                None
            } else if t.to_ascii_uppercase().starts_with("ORDER BY ") {
                Some(t.to_string())
            } else {
                Some(format!("ORDER BY {}", t))
            }
        });

        let task = cx.background_spawn(async move {
            let is_mysql = driver.as_deref().map_or(false, |d| {
                let d = d.to_ascii_lowercase();
                d.contains("mysql") || d.contains("mariadb")
            });
            let quoted = if is_mysql {
                format!("`{}`", table.replace('`', "``"))
            } else {
                format!("\"{}\"", table.replace('"', "\"\""))
            };

            let mut fetched_columns = Vec::new();
            if columns_empty && is_mysql {
                let describe_sql = format!("DESCRIBE {}", quoted);
                if let Ok(desc_rows) = run_query(&root, &connection, &describe_sql) {
                    for row in desc_rows {
                        let name = row.get("Field").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        if !name.is_empty() {
                            let column_type = row.get("Type").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let primary_key = row.get("Key").and_then(|v| v.as_str()).map_or(false, |k| k == "PRI");
                            fetched_columns.push(DatabaseColumn {
                                name,
                                column_type,
                                primary_key,
                            });
                        }
                    }
                }
            }

            if wants_rowid {
                let mut rowid_sql = format!(
                    "select rowid as {} , * from {}",
                    sql_quote_ident_for(driver.as_deref(), ROWID_COLUMN),
                    quoted
                );
                if let Some(w) = &where_clause {
                    rowid_sql.push(' ');
                    rowid_sql.push_str(w);
                }
                if let Some(o) = &order_by_clause {
                    rowid_sql.push(' ');
                    rowid_sql.push_str(o);
                }
                rowid_sql.push_str(&format!(" limit {} offset {}", page_size, offset));
                let executed_sql = rowid_sql.clone();
                match run_query(&root, &connection, &rowid_sql) {
                    Ok(rows) => return Ok((rows, true, true, fetched_columns, executed_sql)),
                    Err(_) => {}
                }
            }
            let mut sql = format!("select * from {}", quoted);
            if let Some(w) = &where_clause {
                sql.push(' ');
                sql.push_str(w);
            }
            if let Some(o) = &order_by_clause {
                sql.push(' ');
                sql.push_str(o);
            }
            sql.push_str(&format!(" limit {} offset {}", page_size, offset));
            let executed_sql = sql.clone();
            run_query(&root, &connection, &sql).map(|rows| (rows, false, false, fetched_columns, executed_sql))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((rows, rowid_mode, rowid_ok, fetched_columns, executed_sql)) => {
                        this.last_executed_sql = Some(executed_sql);
                        this.rowid_mode = rowid_mode;
                        this.rows = rows
                            .into_iter()
                            .map(|row| row_data_from_json(row, rowid_mode))
                            .collect();
                        if this.columns.is_empty() {
                            if !fetched_columns.is_empty() {
                                this.columns = fetched_columns;
                            } else {
                                this.columns = derive_columns(&this.rows);
                            }
                        }
                        for row in &mut this.rows {
                            row.format_for_columns(&this.columns);
                        }
                        this.original_rows = this.rows.clone();
                        this.pending_updates.clear();
                        this.pending_deletions.clear();
                        this.preview_modal = None;
                        this.editable = this.editable_candidate
                            && (this.primary_key().is_some() || (rowid_mode && rowid_ok));
                        this.status = LoadStatus::Loaded;
                    }
                    Err(message) => {
                        this.rows.clear();
                        this.editable = false;
                        this.status = LoadStatus::Loaded;
                        this.message = Some((message.into(), Color::Error));
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

    fn has_pending_changes(&self) -> bool {
        !self.pending_updates.is_empty()
            || !self.pending_deletions.is_empty()
            || self.rows.iter().any(|r| r.is_new)
    }

    fn pending_count(&self) -> usize {
        self.pending_updates.len()
            + self.pending_deletions.len()
            + self.rows.iter().filter(|r| r.is_new).count()
    }

    fn update_pending_message(&mut self) {
        let count = self.pending_count();
        if count > 0 {
            self.message = Some((
                format!(
                    "{} uncommitted change{} · click ↑ to Submit",
                    count,
                    if count == 1 { "" } else { "s" }
                )
                .into(),
                Color::Warning,
            ));
        } else {
            self.message = None;
        }
    }

    fn on_editor_blur(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.stage_cell_edit(cx);
        }
    }

    fn cancel_editing(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            cx.notify();
        }
    }

    /// Stores cell edit in memory and marks it as pending. Does not write to DB.
    fn stage_cell_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        let text = edit.editor.read(cx).text(cx);
        let is_new = self.rows.get(edit.row).is_some_and(|row| row.is_new);

        if is_new {
            if let Some(row) = self.rows.get_mut(edit.row) {
                if text.is_empty() {
                    row.values.remove(&edit.column);
                } else {
                    row.values.insert(edit.column.clone(), Value::String(text.clone()));
                }
                row.format_for_columns(&self.columns);
            }
            self.update_pending_message();
            cx.notify();
            return;
        }

        let original_row = self.original_rows.get(edit.row);
        let original_val = original_row
            .and_then(|r| r.values.get(&edit.column))
            .cloned()
            .unwrap_or(Value::Null);
        let original_text = match &original_val {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => display_value(other),
        };
        let original_is_null = original_val.is_null();

        let is_same = if original_is_null {
            text.is_empty()
        } else {
            text == original_text
        };

        if is_same {
            self.pending_updates.remove(&(edit.row, edit.column.clone()));
            if let Some(row) = self.rows.get_mut(edit.row) {
                row.values.insert(edit.column.clone(), original_val);
                row.format_for_columns(&self.columns);
            }
            self.update_pending_message();
            cx.notify();
            return;
        }

        let key = self.row_key(edit.row);
        let Some((key_column, key_value)) = key else {
            self.message = Some(("no key column for this row".into(), Color::Error));
            cx.notify();
            return;
        };

        let new_val = if text.is_empty() {
            Value::Null
        } else {
            Value::String(text.clone())
        };

        if let Some(row) = self.rows.get_mut(edit.row) {
            row.values.insert(edit.column.clone(), new_val.clone());
            row.format_for_columns(&self.columns);
        }

        self.pending_updates.insert(
            (edit.row, edit.column.clone()),
            PendingCellUpdate {
                row_index: edit.row,
                column: edit.column,
                column_type: edit.column_type,
                original_value: original_val,
                new_value: new_val,
                original_text,
                new_text: text,
                key_column,
                key_value,
            },
        );

        self.update_pending_message();
        cx.notify();
    }

    fn add_row(&mut self, cx: &mut Context<Self>) {
        if !self.editable || self.busy {
            return;
        }
        let mut row = RowData {
            values: Map::new(),
            formatted_cells: Vec::new(),
            is_null_cells: Vec::new(),
            rowid: None,
            is_new: true,
        };
        row.format_for_columns(&self.columns);
        self.rows.push(row);
        let index = self.rows.len() - 1;
        self.selected_row = Some(index);
        self.selected_cell = Some((index, 0));
        self.update_pending_message();
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
            self.update_pending_message();
            cx.notify();
            return;
        }
        if self.pending_deletions.contains(&index) {
            self.pending_deletions.remove(&index);
            self.update_pending_message();
        } else {
            self.pending_deletions.insert(index);
            self.update_pending_message();
        }
        cx.notify();
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

    fn ensure_filter_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.where_editor.is_none() {
            let ed = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("Filter criteria (e.g. status = 1 AND users > 10)", window, cx);
                editor
            });
            self.where_editor = Some(ed);
        }
        if self.order_by_editor.is_none() {
            let ed = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("e.g. id DESC, datetime ASC", window, cx);
                editor
            });
            self.order_by_editor = Some(ed);
        }
    }

    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        if let Some(ed) = &self.where_editor {
            let text = ed.read(cx).text(cx);
            let trimmed = text.trim();
            self.filter_where = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            self.page = 0;
            self.load(cx);
        }
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = &self.where_editor {
            ed.update(cx, |ed, cx| ed.set_text("", window, cx));
        }
        self.filter_where = None;
        self.page = 0;
        self.load(cx);
    }

    fn apply_sort(&mut self, cx: &mut Context<Self>) {
        if let Some(ed) = &self.order_by_editor {
            let text = ed.read(cx).text(cx);
            let trimmed = text.trim();
            self.filter_order_by = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            self.page = 0;
            self.load(cx);
        }
    }

    fn clear_sort(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = &self.order_by_editor {
            ed.update(cx, |ed, cx| ed.set_text("", window, cx));
        }
        self.filter_order_by = None;
        self.page = 0;
        self.load(cx);
    }

    fn toggle_column_sort(&mut self, col_name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.filter_order_by.as_deref().unwrap_or("").trim();
        let asc_clause = format!("{col_name} ASC");
        let desc_clause = format!("{col_name} DESC");

        let next_sort = if current.eq_ignore_ascii_case(&asc_clause) || current.eq_ignore_ascii_case(col_name) {
            Some(desc_clause)
        } else if current.eq_ignore_ascii_case(&desc_clause) {
            None
        } else {
            Some(asc_clause)
        };

        if let Some(sort_str) = &next_sort {
            if let Some(ed) = &self.order_by_editor {
                ed.update(cx, |ed, cx| ed.set_text(sort_str.clone(), window, cx));
            }
            self.filter_order_by = Some(sort_str.clone());
        } else {
            if let Some(ed) = &self.order_by_editor {
                ed.update(cx, |ed, cx| ed.set_text("", window, cx));
            }
            self.filter_order_by = None;
        }
        self.page = 0;
        self.load(cx);
    }

    fn quick_filter_column(&mut self, col_name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let prefill = if let Some((row_idx, col_idx)) = self.selected_cell {
            if self.columns.get(col_idx).is_some_and(|c| c.name == col_name) {
                if let Some(row) = self.rows.get(row_idx) {
                    if let Some(val) = row.values.get(col_name) {
                        if val.is_null() {
                            format!("{col_name} IS NULL")
                        } else if let Some(s) = val.as_str() {
                            format!("{col_name} = '{s}'")
                        } else {
                            format!("{col_name} = {val}")
                        }
                    } else {
                        format!("{col_name} = ")
                    }
                } else {
                    format!("{col_name} = ")
                }
            } else {
                format!("{col_name} = ")
            }
        } else {
            format!("{col_name} = ")
        };

        if let Some(ed) = &self.where_editor {
            ed.update(cx, |ed, cx| {
                ed.set_text(prefill.clone(), window, cx);
                ed.focus_handle(cx).focus(window, cx);
            });
        }
        self.filter_where = Some(prefill);
        cx.notify();
    }

    fn show_ddl(&mut self, cx: &mut Context<Self>) {
        let connection = self.connection.clone();
        let root = self.root.clone();
        let table = self.table.clone();
        let driver = self.driver.clone();
        let columns = self.columns.clone();
        let is_mysql = driver.as_deref().map_or(false, |d| {
            let d = d.to_ascii_lowercase();
            d.contains("mysql") || d.contains("mariadb")
        });
        let quoted = if is_mysql {
            format!("`{}`", table.replace('`', "``"))
        } else {
            format!("\"{}\"", table.replace('"', "\"\""))
        };

        self.busy = true;
        self.message = Some(("Fetching DDL…".into(), Color::Muted));
        cx.notify();

        let task = cx.background_spawn(async move {
            if is_mysql {
                let sql = format!("SHOW CREATE TABLE {}", quoted);
                if let Ok(rows) = run_query(&root, &connection, &sql) {
                    if let Some(row) = rows.first() {
                        if let Some(ddl) = row.get("Create Table").and_then(|v| v.as_str()) {
                            return Ok::<String, String>(ddl.to_string());
                        }
                    }
                }
            } else {
                let sql = format!(
                    "SELECT sql FROM sqlite_master WHERE type='table' AND name='{}'",
                    table.replace('\'', "''")
                );
                if let Ok(rows) = run_query(&root, &connection, &sql) {
                    if let Some(row) = rows.first() {
                        if let Some(ddl) = row.get("sql").and_then(|v| v.as_str()) {
                            return Ok::<String, String>(ddl.to_string());
                        }
                    }
                }
            }
            let mut ddl = format!("CREATE TABLE {} (\n", quoted);
            for (i, col) in columns.iter().enumerate() {
                let pk = if col.primary_key { " PRIMARY KEY" } else { "" };
                ddl.push_str(&format!("  {} {}{}", col.name, col.column_type, pk));
                if i + 1 < columns.len() {
                    ddl.push(',');
                }
                ddl.push('\n');
            }
            ddl.push_str(");");
            Ok::<String, String>(ddl)
        });

        cx.spawn(async move |this, cx| {
            let result: Result<String, String> = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(ddl) => {
                        this.ddl_modal = Some(ddl);
                        this.message = None;
                    }
                    Err(err) => {
                        this.message = Some((err.into(), Color::Error));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn revert_changes(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.editing = None;
        self.pending_updates.clear();
        self.pending_deletions.clear();
        self.preview_modal = None;
        self.rows = self.original_rows.clone();
        self.message = Some(("Reverted all pending changes".into(), Color::Muted));
        cx.notify();
    }

    fn generate_pending_sqls(&self) -> Vec<String> {
        let mut sqls = Vec::new();

        // 1. Pending updates
        for update in self.pending_updates.values() {
            let literal = if update.new_value.is_null() {
                "NULL".to_string()
            } else {
                sql_value_for_column(&update.column_type, &update.new_text)
            };
            let sql = update_sql_for(
                self.driver.as_deref(),
                &self.table,
                &update.column,
                &literal,
                &update.key_column,
                &sql_literal_for_value(&update.key_value),
            );
            sqls.push(sql);
        }

        // 2. Pending new rows (inserts)
        for row in &self.rows {
            if row.is_new {
                let sql = insert_sql_for(
                    self.driver.as_deref(),
                    &self.table,
                    &self.columns,
                    &row.values,
                );
                sqls.push(sql);
            }
        }

        // 3. Pending deletions
        for &del_idx in &self.pending_deletions {
            if let Some((key_col, key_val)) = self.row_key(del_idx) {
                let sql = delete_sql_for(
                    self.driver.as_deref(),
                    &self.table,
                    &key_col,
                    &sql_literal_for_value(&key_val),
                );
                sqls.push(sql);
            }
        }

        sqls
    }

    fn show_pending_changes_preview(&mut self, cx: &mut Context<Self>) {
        if self.editing.is_some() {
            self.stage_cell_edit(cx);
        }
        let sqls = self.generate_pending_sqls();
        if sqls.is_empty() {
            self.message = Some(("No pending changes to preview".into(), Color::Muted));
            cx.notify();
            return;
        }
        let count = sqls.len();
        let mut body = format!(
            "-- Pending Changes ({} statement{}):\n\n",
            count,
            if count == 1 { "" } else { "s" }
        );
        for sql in &sqls {
            body.push_str(sql);
            body.push_str(";\n");
        }
        self.preview_modal = Some(body);
        cx.notify();
    }

    fn submit_changes(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.editing.is_some() {
            self.stage_cell_edit(cx);
        }
        let sqls = self.generate_pending_sqls();
        if sqls.is_empty() {
            self.message = Some(("No pending changes to submit".into(), Color::Muted));
            cx.notify();
            return;
        }
        self.busy = true;
        self.message = Some(("Submitting changes...".into(), Color::Muted));
        self.preview_modal = None;
        cx.notify();

        let connection = self.connection.clone();
        let root = self.root.clone();

        let task = cx.background_spawn(async move {
            for sql in &sqls {
                run_write(&root, &connection, sql)?;
            }
            Ok::<(), String>(())
        });

        cx.spawn(async move |this, cx| {
            let result: Result<(), String> = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.pending_updates.clear();
                        this.pending_deletions.clear();
                        this.message = Some(("saved".into(), Color::Success));
                        this.load(cx);
                    }
                    Err(err) => {
                        this.message = Some((err.into(), Color::Error));
                        cx.notify();
                    }
                }
            })
            .log_err();
        })
        .detach();
    }

    fn stop_query(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.busy = false;
            self.message = Some(("Query stopped".into(), Color::Muted));
            cx.notify();
        }
    }

    fn format_export_data(&self, format: ExportFormat) -> String {
        match format {
            ExportFormat::Csv => {
                let mut out = String::new();
                let col_names = self.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
                out.push_str(&col_names.join(","));
                out.push('\n');
                for row in &self.rows {
                    let values = self.columns.iter().map(|c| {
                        let val = row.values.get(&c.name);
                        match val {
                            None | Some(Value::Null) => String::new(),
                            Some(Value::String(s)) => {
                                if s.contains(',') || s.contains('"') || s.contains('\n') {
                                    format!("\"{}\"", s.replace('"', "\"\""))
                                } else {
                                    s.clone()
                                }
                            }
                            Some(other) => other.to_string(),
                        }
                    }).collect::<Vec<_>>();
                    out.push_str(&values.join(","));
                    out.push('\n');
                }
                out
            }
            ExportFormat::Tsv => {
                let mut out = String::new();
                let col_names = self.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
                out.push_str(&col_names.join("\t"));
                out.push('\n');
                for row in &self.rows {
                    let values = self.columns.iter().map(|c| {
                        let val = row.values.get(&c.name);
                        match val {
                            None | Some(Value::Null) => String::new(),
                            Some(Value::String(s)) => s.replace('\t', " ").replace('\n', " "),
                            Some(other) => other.to_string(),
                        }
                    }).collect::<Vec<_>>();
                    out.push_str(&values.join("\t"));
                    out.push('\n');
                }
                out
            }
            ExportFormat::Json => {
                let mut list = Vec::new();
                for row in &self.rows {
                    list.push(&row.values);
                }
                serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".to_string())
            }
            ExportFormat::SqlInserts => {
                let mut out = String::new();
                let table_quoted = format!("`{}`", self.table.replace('`', "``"));
                let col_quoted = self.columns.iter().map(|c| format!("`{}`", c.name.replace('`', "``"))).collect::<Vec<_>>().join(", ");
                for row in &self.rows {
                    let values = self.columns.iter().map(|c| {
                        let val = row.values.get(&c.name);
                        match val {
                            None | Some(Value::Null) => "NULL".to_string(),
                            Some(Value::String(s)) => format!("'{}'", s.replace('\'', "''")),
                            Some(Value::Number(n)) => n.to_string(),
                            Some(Value::Bool(b)) => if *b { "1" } else { "0" }.to_string(),
                            Some(other) => format!("'{}'", other.to_string().replace('\'', "''")),
                        }
                    }).collect::<Vec<_>>().join(", ");
                    out.push_str(&format!("INSERT INTO {} ({}) VALUES ({});\n", table_quoted, col_quoted, values));
                }
                out
            }
            ExportFormat::Markdown => {
                let mut out = String::new();
                let headers = self.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(" | ");
                out.push_str(&format!("| {} |\n", headers));
                let separators = self.columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | ");
                out.push_str(&format!("| {} |\n", separators));
                for row in &self.rows {
                    let values = self.columns.iter().map(|c| {
                        let val = row.values.get(&c.name);
                        match val {
                            None | Some(Value::Null) => "".to_string(),
                            Some(Value::String(s)) => s.replace('|', "\\|").replace('\n', " "),
                            Some(other) => other.to_string(),
                        }
                    }).collect::<Vec<_>>().join(" | ");
                    out.push_str(&format!("| {} |\n", values));
                }
                out
            }
            ExportFormat::Html => {
                let mut out = String::new();
                out.push_str("<table>\n  <thead>\n    <tr>\n");
                for col in &self.columns {
                    out.push_str(&format!("      <th>{}</th>\n", html_escape(&col.name)));
                }
                out.push_str("    </tr>\n  </thead>\n  <tbody>\n");
                for row in &self.rows {
                    out.push_str("    <tr>\n");
                    for col in &self.columns {
                        let val = row.values.get(&col.name);
                        let s = match val {
                            None | Some(Value::Null) => "".to_string(),
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                        };
                        out.push_str(&format!("      <td>{}</td>\n", html_escape(&s)));
                    }
                    out.push_str("    </tr>\n");
                }
                out.push_str("  </tbody>\n</table>\n");
                out
            }
        }
    }

    fn export_to_clipboard(&mut self, format: ExportFormat, cx: &mut Context<Self>) {
        let text = self.format_export_data(format);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.message = Some((
            format!("Copied {} rows as {} to clipboard", self.rows.len(), format.label()).into(),
            Color::Success,
        ));
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let border_color = colors.border;
        let has_pending = self.has_pending_changes();

        let mut toolbar = h_flex()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .items_center()
            .gap_1p5()
            .px_2()
            .border_b_1()
            .border_color(border_color)
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
                IconButton::new("table-stop", IconName::Stop)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(!self.busy)
                    .tooltip(Tooltip::text("Cancel Query"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.stop_query(cx))),
            )
            .child(div().w(px(1.0)).h(px(16.0)).bg(border_color))
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
                IconButton::new("table-revert-btn", IconName::Undo)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(!has_pending || self.busy)
                    .tooltip(Tooltip::text(if has_pending {
                        "Revert pending changes"
                    } else {
                        "No pending changes to revert"
                    }))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.revert_changes(cx))),
            )
            .child(
                IconButton::new("table-preview-btn", IconName::Eye)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(!has_pending || self.busy)
                    .tooltip(Tooltip::text(if has_pending {
                        "Preview pending changes (Show SQL)"
                    } else {
                        "No pending changes to preview"
                    }))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.show_pending_changes_preview(cx))),
            )
            .child(
                if has_pending && !self.busy {
                    IconButton::new("table-submit-btn", IconName::ArrowUp)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::XSmall)
                        .icon_color(Color::Success)
                        .tooltip(Tooltip::text("Submit changes (Cmd+Enter)"))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.submit_changes(cx)))
                } else {
                    IconButton::new("table-submit-btn", IconName::ArrowUp)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::XSmall)
                        .disabled(true)
                        .tooltip(Tooltip::text("No pending changes to submit"))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.submit_changes(cx)))
                }
            )
            .child(div().w(px(1.0)).h(px(16.0)).bg(border_color))
            .child(
                PopoverMenu::new("table-tx-menu")
                    .trigger(
                        Button::new("table-tx-btn", if self.tx_manual { "Tx: Manual" } else { "Tx: Auto" })
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .style(ButtonStyle::Subtle)
                            .tooltip(Tooltip::text("Transaction Mode")),
                    )
                    .anchor(Anchor::BottomLeft)
                    .menu({
                        let is_manual = self.tx_manual;
                        let view = cx.entity().downgrade();
                        move |window, cx| {
                            let view = view.clone();
                            Some(ContextMenu::build(window, cx, move |menu, _, _| {
                                let view_auto = view.clone();
                                let view_man = view.clone();
                                menu
                                    .item(
                                        ContextMenuEntry::new("Tx: Auto (Auto-commit)")
                                            .toggleable(ui::IconPosition::Start, !is_manual)
                                            .handler(move |_window, cx| {
                                                if let Some(view) = view_auto.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.tx_manual = false;
                                                        this.message = Some(("Auto-commit enabled".into(), Color::Muted));
                                                        cx.notify();
                                                    });
                                                }
                                            }),
                                    )
                                    .item(
                                        ContextMenuEntry::new("Tx: Manual (Explicit commit)")
                                            .toggleable(ui::IconPosition::Start, is_manual)
                                            .handler(move |_window, cx| {
                                                if let Some(view) = view_man.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.tx_manual = true;
                                                        this.message = Some(("Manual transaction mode enabled".into(), Color::Muted));
                                                        cx.notify();
                                                    });
                                                }
                                            }),
                                    )
                            }))
                        }
                    }),
            )
            .child(
                Button::new("table-ddl-btn", "DDL")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .style(ButtonStyle::Subtle)
                    .tooltip(Tooltip::text("View Table DDL"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| this.show_ddl(cx))),
            )
            .child(div().w(px(1.0)).h(px(16.0)).bg(border_color))
            .child(
                IconButton::new("table-page-prev", IconName::ArrowLeft)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(self.page == 0 || self.busy)
                    .tooltip(Tooltip::text("Previous page"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.page = this.page.saturating_sub(1);
                        this.load(cx);
                    })),
            )
            .child(
                Label::new(format!("Page {}", self.page + 1))
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Default),
            )
            .child(
                IconButton::new("table-page-next", IconName::ArrowRight)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .disabled(self.rows.len() < self.page_size || self.busy)
                    .tooltip(Tooltip::text("Next page"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.page += 1;
                        this.load(cx);
                    })),
            )
            .child(
                PopoverMenu::new("table-page-size-menu")
                    .trigger(
                        Button::new("table-page-size", format!("{} / page", self.page_size))
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .style(ButtonStyle::Subtle)
                            .tooltip(Tooltip::text("Rows per page")),
                    )
                    .anchor(Anchor::BottomLeft)
                    .menu({
                        let active_page_size = self.page_size;
                        let view = cx.entity().downgrade();
                        move |window, cx| {
                            let view = view.clone();
                            Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                for &size in PAGE_SIZES {
                                    let is_active = size == active_page_size;
                                    let view = view.clone();
                                    menu = menu.item(
                                        ContextMenuEntry::new(format!("{size} rows"))
                                            .toggleable(ui::IconPosition::Start, is_active)
                                            .handler(move |_window, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        if this.page_size != size {
                                                            this.page_size = size;
                                                            this.page = 0;
                                                            this.load(cx);
                                                        }
                                                    });
                                                }
                                            }),
                                    );
                                }
                                menu
                            }))
                        }
                    }),
            )
            .child(
                Label::new(if self.rows.is_empty() {
                    "0 rows".to_string()
                } else {
                    let start = self.page * self.page_size + 1;
                    let end = self.page * self.page_size + self.rows.len();
                    format!("{start}–{end} rows")
                })
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

        if let Some((message, color)) = &self.message {
            toolbar = toolbar.child(
                Label::new(message.clone())
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(*color)
                    .truncate(),
            );
        }

        toolbar = toolbar.child(div().flex_1());

        let sql_tip: SharedString = self
            .last_executed_sql
            .as_deref()
            .unwrap_or("No query executed yet")
            .to_string()
            .into();

        toolbar = toolbar
            .child(
                Button::new("table-sql-btn", "SQL")
                    .size(ButtonSize::Compact)
                    .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .style(ButtonStyle::Subtle)
                    .tooltip(Tooltip::text(sql_tip))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        if let Some(sql) = &this.last_executed_sql {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(sql.clone()));
                            this.message = Some(("Current SQL copied to clipboard".into(), Color::Success));
                            cx.notify();
                        }
                    })),
            )
            .child(
                PopoverMenu::new("table-export-menu")
                    .trigger(
                        Button::new("table-export-btn", self.export_format.label())
                            .size(ButtonSize::Compact)
                            .label_size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .style(ButtonStyle::Subtle)
                            .tooltip(Tooltip::text("Export Format")),
                    )
                    .anchor(Anchor::BottomRight)
                    .menu({
                        let current_fmt = self.export_format;
                        let view = cx.entity().downgrade();
                        move |window, cx| {
                            let view = view.clone();
                            Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                                const FORMATS: &[ExportFormat] = &[
                                    ExportFormat::Csv,
                                    ExportFormat::Tsv,
                                    ExportFormat::Json,
                                    ExportFormat::SqlInserts,
                                    ExportFormat::Markdown,
                                    ExportFormat::Html,
                                ];
                                for &fmt in FORMATS {
                                    let is_active = fmt == current_fmt;
                                    let view = view.clone();
                                    menu = menu.item(
                                        ContextMenuEntry::new(fmt.label())
                                            .toggleable(ui::IconPosition::Start, is_active)
                                            .handler(move |_window, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.export_format = fmt;
                                                        this.export_to_clipboard(fmt, cx);
                                                    });
                                                }
                                            }),
                                    );
                                }
                                menu
                            }))
                        }
                    }),
            )
            .child(
                IconButton::new("table-export-dl", IconName::Download)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Export data (Copy to Clipboard)"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        let fmt = this.export_format;
                        this.export_to_clipboard(fmt, cx);
                    })),
            )
            .child(
                PopoverMenu::new("table-settings-menu")
                    .trigger(
                        IconButton::new("table-settings-btn", IconName::Settings)
                            .shape(IconButtonShape::Square)
                            .icon_size(IconSize::XSmall)
                            .tooltip(Tooltip::text("Table Options")),
                    )
                    .anchor(Anchor::BottomRight)
                    .menu({
                        let view = cx.entity().downgrade();
                        move |window, cx| {
                            let view = view.clone();
                            Some(ContextMenu::build(window, cx, move |menu, _, _| {
                                let v1 = view.clone();
                                let v2 = view.clone();
                                let v3 = view.clone();
                                menu
                                    .item(
                                        ContextMenuEntry::new("Reload Data & Schema")
                                            .icon(IconName::RotateCw)
                                            .handler(move |_window, cx| {
                                                if let Some(view) = v1.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.load(cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .item(
                                        ContextMenuEntry::new("Clear WHERE Filter")
                                            .icon(IconName::Filter)
                                            .handler(move |window, cx| {
                                                if let Some(view) = v2.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.clear_filter(window, cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .item(
                                        ContextMenuEntry::new("Reset Sorting (ORDER BY)")
                                            .icon(IconName::ArrowDown10)
                                            .handler(move |window, cx| {
                                                if let Some(view) = v3.upgrade() {
                                                    view.update(cx, |this, cx| {
                                                        this.clear_sort(window, cx);
                                                    });
                                                }
                                            }),
                                    )
                            }))
                        }
                    }),
            );

        toolbar.into_any_element()
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let border_color = colors.border;
        let editor_bg = colors.editor_background;

        let where_editor_el = self.where_editor.as_ref().map(|ed| {
            let style = EditorStyle {
                background: editor_bg,
                local_player: cx.theme().players().local(),
                text: dialog_text_style(cx),
                ..EditorStyle::default()
            };
            div()
                .flex_1()
                .h(px(22.0))
                .px_1()
                .border_1()
                .border_color(border_color)
                .rounded_xs()
                .bg(editor_bg)
                .flex()
                .items_center()
                .on_action(cx.listener(|this, _: &editor_actions::Newline, _window, cx| {
                    this.apply_filter(cx);
                }))
                .on_action(cx.listener(|this, _: &Confirm, _window, cx| {
                    this.apply_filter(cx);
                }))
                .child(EditorElement::new(ed, style))
        });

        let order_by_editor_el = self.order_by_editor.as_ref().map(|ed| {
            let style = EditorStyle {
                background: editor_bg,
                local_player: cx.theme().players().local(),
                text: dialog_text_style(cx),
                ..EditorStyle::default()
            };
            div()
                .w(px(220.0))
                .h(px(22.0))
                .px_1()
                .border_1()
                .border_color(border_color)
                .rounded_xs()
                .bg(editor_bg)
                .flex()
                .items_center()
                .on_action(cx.listener(|this, _: &editor_actions::Newline, _window, cx| {
                    this.apply_sort(cx);
                }))
                .on_action(cx.listener(|this, _: &Confirm, _window, cx| {
                    this.apply_sort(cx);
                }))
                .child(EditorElement::new(ed, style))
        });

        let has_where = self.filter_where.is_some();
        let has_order_by = self.filter_order_by.is_some();

        h_flex()
            .h(px(30.0))
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(border_color)
            .bg(colors.surface_background)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::Filter)
                            .size(IconSize::XSmall)
                            .color(if has_where { Color::Accent } else { Color::Muted }),
                    )
                    .child(
                        Label::new("WHERE")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if has_where { Color::Accent } else { Color::Muted }),
                    ),
            )
            .children(where_editor_el)
            .when(has_where, |this| {
                this.child(
                    IconButton::new("table-clear-filter", IconName::Close)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Clear filter"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.clear_filter(window, cx);
                        })),
                )
            })
            .child(
                IconButton::new("table-apply-filter", IconName::Check)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Apply filter (Enter)"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.apply_filter(cx);
                    })),
            )
            .child(div().w(px(1.0)).h(px(16.0)).bg(border_color))
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Icon::new(IconName::ArrowDown10)
                            .size(IconSize::XSmall)
                            .color(if has_order_by { Color::Accent } else { Color::Muted }),
                    )
                    .child(
                        Label::new("ORDER BY")
                            .size(LabelSize::Custom(PANEL_TEXT_REMS))
                            .color(if has_order_by { Color::Accent } else { Color::Muted }),
                    ),
            )
            .children(order_by_editor_el)
            .when(has_order_by, |this| {
                this.child(
                    IconButton::new("table-clear-sort", IconName::Close)
                        .shape(IconButtonShape::Square)
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Clear ordering"))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.clear_sort(window, cx);
                        })),
                )
            })
            .child(
                IconButton::new("table-apply-sort", IconName::Check)
                    .shape(IconButtonShape::Square)
                    .icon_size(IconSize::XSmall)
                    .tooltip(Tooltip::text("Apply order (Enter)"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                        this.apply_sort(cx);
                    })),
            )
            .into_any_element()
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match &self.status {
            LoadStatus::Loading => centered_note("loading rows…", Color::Muted),
            LoadStatus::Failed(message) => centered_note(message.clone(), Color::Error),
            LoadStatus::Loaded => {
                if self.rows.is_empty() {
                    return centered_note("no rows", Color::Muted);
                }
                let editor_background = cx.theme().colors().editor_background;
                let row_count = self.rows.len();
                let columns_width = self.columns.len() as f32 * CELL_WIDTH;

                let vertical_scroll_handle = self.vertical_scroll_handle.clone();
                let horizontal_scroll_handle = self.horizontal_scroll_handle.clone();

                let header_row = self.render_grid_header_row(columns_width, cx);

                let rows_list = uniform_list(
                    "table-grid-body",
                    row_count,
                    cx.processor(move |this, range: Range<usize>, _window, cx| {
                        range
                            .map(|row_index| this.render_grid_row(row_index, columns_width, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .with_sizing_behavior(ListSizingBehavior::Auto)
                .track_scroll(&vertical_scroll_handle);

                let h_scrollbar = div()
                    .absolute()
                    .inset_0()
                    .left(px(ROW_NUMBER_WIDTH))
                    .custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Horizontal)
                            .tracked_scroll_handle(&horizontal_scroll_handle),
                        window,
                        cx,
                    );

                let mut body = v_flex()
                    .id("table-grid-viewport")
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .bg(editor_background)
                    .child(header_row)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .size_full()
                            .relative()
                            .child(rows_list)
                            .child(h_scrollbar),
                    );

                body = body.custom_scrollbars(
                    Scrollbars::new(ScrollAxes::Vertical)
                        .tracked_scroll_handle(&vertical_scroll_handle),
                    window,
                    cx,
                );

                body.into_any_element()
            }
        }
    }

    fn render_grid_header_row(&self, columns_width: f32, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors();
        let pinned_header = div()
            .w(px(ROW_NUMBER_WIDTH))
            .h_full()
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .border_r_1()
            .border_color(colors.border)
            .child(
                Label::new("#")
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(Color::Muted),
            );

        let mut columns_header = h_flex().w(px(columns_width)).h_full().flex_none();
        for (col_idx, column) in self.columns.iter().enumerate() {
            let is_sorted_asc = self.filter_order_by.as_deref().map_or(false, |s| {
                s.eq_ignore_ascii_case(&format!("{} ASC", column.name))
                    || s.eq_ignore_ascii_case(&column.name)
            });
            let is_sorted_desc = self.filter_order_by.as_deref().map_or(false, |s| {
                s.eq_ignore_ascii_case(&format!("{} DESC", column.name))
            });

            let mut column_label = h_flex().gap_1().items_center().overflow_hidden().child(
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

            let col_name = column.name.clone();
            let col_name_for_filter = column.name.clone();

            columns_header = columns_header.child(
                div()
                    .w(px(CELL_WIDTH))
                    .h_full()
                    .flex_none()
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_r_1()
                    .border_color(colors.border)
                    .overflow_hidden()
                    .child(column_label)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_0p5()
                            .child(
                                IconButton::new(("col-filter", col_idx), IconName::Filter)
                                    .shape(IconButtonShape::Square)
                                    .icon_size(IconSize::XSmall)
                                    .tooltip(Tooltip::text(format!("Filter by {}", column.name)))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        this.quick_filter_column(&col_name_for_filter, window, cx);
                                    })),
                            )
                            .child(
                                IconButton::new(
                                    ("col-sort", col_idx),
                                    if is_sorted_asc {
                                        IconName::ArrowUp
                                    } else if is_sorted_desc {
                                        IconName::ArrowDown
                                    } else {
                                        IconName::ArrowDown10
                                    },
                                )
                                .shape(IconButtonShape::Square)
                                .icon_size(IconSize::XSmall)
                                .tooltip(Tooltip::text(format!("Sort by {}", column.name)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.toggle_column_sort(&col_name, window, cx);
                                })),
                            ),
                    ),
            );
        }

        let scrollable_header = div()
            .id("table-header-scrollable")
            .flex_grow_1()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.horizontal_scroll_handle)
            .child(columns_header);

        h_flex()
            .id("table-grid-header")
            .h(px(ROW_HEIGHT + 4.0))
            .w_full()
            .flex_none()
            .bg(colors.element_background)
            .border_b_1()
            .border_color(colors.border)
            .child(pinned_header)
            .child(scrollable_header)
            .into_any_element()
    }

    fn render_grid_row(
        &self,
        row_index: usize,
        columns_width: f32,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(row_index) else {
            return div().into_any_element();
        };
        let colors = cx.theme().colors();
        let row_selected = self.selected_row == Some(row_index);

        let row_number_text = if row.is_new {
            "·".to_string()
        } else {
            format!("{}", self.page * self.page_size + row_index + 1)
        };

        let pinned_cell = div()
            .id(("row-num", row_index))
            .w(px(ROW_NUMBER_WIDTH))
            .h_full()
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .border_r_1()
            .border_color(colors.border)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.selected_row = Some(row_index);
                this.selected_cell = None;
                cx.notify();
            }))
            .child(
                Label::new(row_number_text)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(if row.is_new { Color::Accent } else { Color::Muted }),
            );

        let mut columns_container = h_flex().w(px(columns_width)).h_full().flex_none();
        for (column_index, column) in self.columns.iter().enumerate() {
            columns_container = columns_container.child(
                self.render_cell(row_index, column_index, column, row, cx),
            );
        }

        let scrollable_section = div()
            .id(("row-cells", row_index))
            .flex_grow_1()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.horizontal_scroll_handle)
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                let container_bounds = this.horizontal_scroll_handle.bounds();
                let h_offset = this.horizontal_scroll_handle.offset().x;
                let col_x = event.position().x - container_bounds.left() - h_offset;
                if col_x >= Pixels::ZERO {
                    let col_idx = (col_x / px(CELL_WIDTH)).floor() as usize;
                    if col_idx < this.columns.len() {
                        this.focus_handle.focus(window, cx);
                        this.select_cell(row_index, col_idx, cx);
                        if this.editable && event.click_count() > 1 {
                            this.start_editing(row_index, col_idx, window, cx);
                        }
                    }
                }
            }))
            .child(columns_container);

        h_flex()
            .id(("table-row", row_index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex_none()
            .border_b_1()
            .border_color(colors.border)
            .when(row_selected, |this| this.bg(colors.element_selected))
            .when(!row_selected, |this| {
                this.hover(|style| style.bg(colors.element_hover.opacity(0.4)))
            })
            .child(pinned_cell)
            .child(scrollable_section)
            .into_any_element()
    }

    fn render_cell(
        &self,
        row_index: usize,
        column_index: usize,
        column: &DatabaseColumn,
        row: &RowData,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let selected = self.selected_cell == Some((row_index, column_index));

        if let Some(edit) = &self.editing
            && edit.row == row_index
            && edit.column == column.name
        {
            return div()
                .id(("c-edit", row_index * 10000 + column_index))
                .w(px(CELL_WIDTH))
                .h_full()
                .flex_none()
                .px_2()
                .flex()
                .items_center()
                .overflow_hidden()
                .border_r_1()
                .border_color(colors.border)
                .bg(colors.editor_background)
                .border_1()
                .border_color(colors.border_focused)
                .child(
                    div()
                        .id("cell-editor")
                        .size_full()
                        .key_context("DatabaseTableCellEditor")
                        .capture_action(cx.listener(
                            |this, _: &editor_actions::Newline, _window, cx| {
                                this.stage_cell_edit(cx);
                            },
                        ))
                        .on_action(cx.listener(|this, _: &Confirm, _window, cx| {
                            this.stage_cell_edit(cx);
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

        let is_modified = self
            .pending_updates
            .contains_key(&(row_index, column.name.clone()));
        let is_deleted = self.pending_deletions.contains(&row_index);

        let text = row
            .formatted_cells
            .get(column_index)
            .cloned()
            .unwrap_or_else(|| SharedString::from("NULL"));
        let is_null = row.is_null_cells.get(column_index).copied().unwrap_or(true);

        div()
            .w(px(CELL_WIDTH))
            .h_full()
            .flex_none()
            .px_2()
            .flex()
            .items_center()
            .overflow_hidden()
            .border_r_1()
            .border_color(colors.border)
            .when(is_modified && !selected, |this| {
                this.bg(gpui::rgba(0x215a8e73))
                    .border_1()
                    .border_color(gpui::rgba(0x388bfd99))
            })
            .when(is_modified && selected, |this| {
                this.bg(gpui::rgba(0x2868a5a6))
                    .border_1()
                    .border_color(colors.border_focused)
            })
            .when(!is_modified && selected, |this| {
                this.bg(colors.element_hover)
                    .border_1()
                    .border_color(colors.border_focused)
            })
            .when(is_deleted, |this| {
                this.bg(gpui::rgba(0x8a1c1c59))
            })
            .when(row.is_new && !selected && !is_modified, |this| {
                this.bg(gpui::rgba(0x1a5c2e40))
            })
            .child(
                Label::new(text)
                    .size(LabelSize::Custom(PANEL_TEXT_REMS))
                    .color(if is_deleted {
                        Color::Muted
                    } else if is_modified {
                        Color::Accent
                    } else if is_null {
                        Color::Muted
                    } else {
                        Color::Default
                    })
                    .truncate(),
            )
            .into_any_element()
    }

    fn render_ddl_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ddl = self.ddl_modal.as_ref()?;
        let colors = cx.theme().colors();
        let table_name = self.table.clone();
        let ddl_text = ddl.clone();

        Some(
            div()
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x000000aa))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .w(px(640.0))
                        .max_h(px(460.0))
                        .bg(colors.elevated_surface_background)
                        .border_1()
                        .border_color(colors.border)
                        .rounded_md()
                        .shadow_lg()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .h(px(36.0))
                                .px_3()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(colors.border)
                                .bg(colors.surface_background)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(Icon::new(IconName::FileCode).size(IconSize::Small))
                                        .child(
                                            Label::new(format!("DDL · {}", table_name))
                                                .size(LabelSize::Default)
                                                .color(Color::Default),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new("copy-ddl", "Copy DDL")
                                                .start_icon(Icon::new(IconName::Copy))
                                                .size(ButtonSize::Compact)
                                                .style(ButtonStyle::Subtle)
                                                .on_click(cx.listener({
                                                    let ddl_to_copy = ddl_text.clone();
                                                    move |this, _: &ClickEvent, _window, cx| {
                                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(ddl_to_copy.clone()));
                                                        this.message = Some(("DDL copied to clipboard".into(), Color::Success));
                                                        cx.notify();
                                                    }
                                                })),
                                        )
                                        .child(
                                            IconButton::new("close-ddl-modal", IconName::Close)
                                                .shape(IconButtonShape::Square)
                                                .icon_size(IconSize::Small)
                                                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                                                    this.ddl_modal = None;
                                                    cx.notify();
                                                })),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("ddl-body")
                                .flex_1()
                                .p_3()
                                .overflow_y_scroll()
                                .bg(colors.editor_background)
                                .child(
                                    Label::new(ddl_text)
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Default),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_preview_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let preview = self.preview_modal.as_ref()?;
        let colors = cx.theme().colors();
        let preview_text = preview.clone();

        Some(
            div()
                .absolute()
                .inset_0()
                .bg(gpui::rgba(0x000000aa))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .w(px(640.0))
                        .max_h(px(460.0))
                        .bg(colors.elevated_surface_background)
                        .border_1()
                        .border_color(colors.border)
                        .rounded_md()
                        .shadow_lg()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .h(px(36.0))
                                .px_3()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(colors.border)
                                .bg(colors.surface_background)
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            Icon::new(IconName::Eye)
                                                .size(IconSize::Small)
                                                .color(Color::Accent),
                                        )
                                        .child(
                                            Label::new("Pending Changes (SQL Preview)")
                                                .size(LabelSize::Default)
                                                .color(Color::Default),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            Button::new("submit-from-preview", "Submit Changes")
                                                .start_icon(
                                                    Icon::new(IconName::ArrowUp)
                                                        .color(Color::Success),
                                                )
                                                .size(ButtonSize::Compact)
                                                .style(ButtonStyle::Filled)
                                                .on_click(cx.listener(
                                                    |this, _: &ClickEvent, _window, cx| {
                                                        this.submit_changes(cx);
                                                    },
                                                )),
                                        )
                                        .child(
                                            Button::new("copy-preview-sql", "Copy SQL")
                                                .start_icon(Icon::new(IconName::Copy))
                                                .size(ButtonSize::Compact)
                                                .style(ButtonStyle::Subtle)
                                                .on_click(cx.listener({
                                                    let text_to_copy = preview_text.clone();
                                                    move |this, _: &ClickEvent, _window, cx| {
                                                        cx.write_to_clipboard(
                                                            gpui::ClipboardItem::new_string(
                                                                text_to_copy.clone(),
                                                            ),
                                                        );
                                                        this.message = Some((
                                                            "SQL copied to clipboard".into(),
                                                            Color::Success,
                                                        ));
                                                        cx.notify();
                                                    }
                                                })),
                                        )
                                        .child(
                                            IconButton::new(
                                                "close-preview-modal",
                                                IconName::Close,
                                            )
                                            .shape(IconButtonShape::Square)
                                            .icon_size(IconSize::Small)
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _window, cx| {
                                                    this.preview_modal = None;
                                                    cx.notify();
                                                },
                                            )),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .id("preview-body")
                                .flex_1()
                                .p_3()
                                .overflow_y_scroll()
                                .bg(colors.editor_background)
                                .child(
                                    Label::new(preview_text)
                                        .size(LabelSize::Custom(PANEL_TEXT_REMS))
                                        .color(Color::Default),
                                ),
                        ),
                )
                .into_any_element(),
        )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_filter_editors(window, cx);
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
                if event.keystroke.key == "enter" && event.keystroke.modifiers.platform {
                    this.submit_changes(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                    this.edit_selected_cell(window, cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "escape" && this.has_pending_changes() {
                    this.revert_changes(cx);
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
            .child(self.render_filter_bar(cx))
            .child(self.render_body(window, cx))
            .children(self.render_ddl_modal(cx))
            .children(self.render_preview_modal(cx))
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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

pub(crate) fn sql_quote_ident_for(driver: Option<&str>, name: &str) -> String {
    let is_mysql = driver.map_or(false, |d| {
        let d = d.to_ascii_lowercase();
        d.contains("mysql") || d.contains("mariadb")
    });
    if is_mysql {
        format!("`{}`", name.replace('`', "``"))
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Quotes an identifier, doubling embedded quotes.
pub(crate) fn sql_quote_ident(name: &str) -> String {
    sql_quote_ident_for(None, name)
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

pub(crate) fn update_sql_for(
    driver: Option<&str>,
    table: &str,
    column: &str,
    literal: &str,
    key_column: &str,
    key_literal: &str,
) -> String {
    let q_table = sql_quote_ident_for(driver, table);
    let q_col = sql_quote_ident_for(driver, column);
    if key_column.eq_ignore_ascii_case("rowid") {
        format!("update {q_table} set {q_col} = {literal} where rowid = {key_literal}")
    } else {
        let q_key = sql_quote_ident_for(driver, key_column);
        format!("update {q_table} set {q_col} = {literal} where {q_key} = {key_literal}")
    }
}

pub(crate) fn update_sql(
    table: &str,
    column: &str,
    literal: &str,
    key_column: &str,
    key_literal: &str,
) -> String {
    update_sql_for(None, table, column, literal, key_column, key_literal)
}

pub(crate) fn delete_sql_for(
    driver: Option<&str>,
    table: &str,
    key_column: &str,
    key_literal: &str,
) -> String {
    let q_table = sql_quote_ident_for(driver, table);
    if key_column.eq_ignore_ascii_case("rowid") {
        format!("delete from {q_table} where rowid = {key_literal}")
    } else {
        let q_key = sql_quote_ident_for(driver, key_column);
        format!("delete from {q_table} where {q_key} = {key_literal}")
    }
}

pub(crate) fn delete_sql(table: &str, key_column: &str, key_literal: &str) -> String {
    delete_sql_for(None, table, key_column, key_literal)
}

/// Builds an INSERT skipping columns whose draft value is empty. Falls back to
/// `DEFAULT VALUES` when every column is empty.
pub(crate) fn insert_sql_for(
    driver: Option<&str>,
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
        names.push(sql_quote_ident_for(driver, &column.name));
        literals.push(sql_value_for_column(&column.column_type, &text));
    }
    let q_table = sql_quote_ident_for(driver, table);
    if names.is_empty() {
        let is_mysql = driver.map_or(false, |d| {
            let d = d.to_ascii_lowercase();
            d.contains("mysql") || d.contains("mariadb")
        });
        if is_mysql {
            format!("insert into {q_table} () values ()")
        } else {
            format!("insert into {q_table} default values")
        }
    } else {
        format!(
            "insert into {q_table} ({}) values ({})",
            names.join(", "),
            literals.join(", ")
        )
    }
}

pub(crate) fn insert_sql(
    table: &str,
    columns: &[DatabaseColumn],
    values: &Map<String, Value>,
) -> String {
    insert_sql_for(None, table, columns, values)
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
        formatted_cells: Vec::new(),
        is_null_cells: Vec::new(),
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
        text: dialog_text_style(cx),
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
    let mut command = std::process::Command::new(crate::resolve_helper_tool("php-db"));
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
    let mut command = std::process::Command::new(crate::resolve_helper_tool("php-db"));
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
        // Dialect-specific fallback: MySQL/MariaDB vs SQLite
        assert_eq!(
            insert_sql_for(Some("mysql"), "users", &columns(), &values),
            "insert into `users` () values ()"
        );
        assert_eq!(
            insert_sql_for(Some("mariadb"), "users", &columns(), &values),
            "insert into `users` () values ()"
        );
        assert_eq!(
            insert_sql_for(Some("sqlite"), "users", &columns(), &values),
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
