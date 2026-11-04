//! Read-only diff tab for a file recorded in a git stash.
//!
//! The right-hand side is the current worktree version (an empty buffer when
//! the file no longer exists); the left-hand side is the stash blob.

use anyhow::Result;
use buffer_diff::BufferDiff;
use editor::{Editor, EditorEvent, EditorSettings, SplittableEditor};
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable, IntoElement, Render,
    SharedString, Task, Window,
};
use language::{Buffer, Capability};
use multi_buffer::MultiBuffer;
use project::{Project, ProjectPath};
use settings::Settings;
use std::{
    any::{Any, TypeId},
    sync::Arc,
};
use ui::{Color, Icon, IconName};
use workspace::{
    Item, ItemNavHistory, Workspace,
    item::{ItemEvent, SaveOptions},
    searchable::SearchableItemHandle,
};

pub struct StashDiffView {
    diff_editor: Entity<SplittableEditor>,
    title: SharedString,
    path: SharedString,
}

impl StashDiffView {
    /// Builds a read-only diff view from an already loaded current version.
    pub fn open(
        title: SharedString,
        path: SharedString,
        current_buffer: Entity<Buffer>,
        diff: Entity<BufferDiff>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let project = workspace.project().clone();
        let workspace_entity = cx.entity();

        let multibuffer = cx.new(|cx| {
            let capability = current_buffer.read(cx).capability();
            let mut multibuffer = MultiBuffer::without_headers(capability);
            let range = language::Point::zero()..current_buffer.read(cx).max_point();
            multibuffer.set_excerpts_for_buffer(current_buffer, [range], 0, cx);
            multibuffer.add_diff(diff, cx);
            multibuffer
        });

        let diff_editor = cx.new(|cx| {
            let editor = SplittableEditor::new(
                EditorSettings::get_global(cx).diff_view_style,
                multibuffer,
                project,
                workspace_entity,
                window,
                cx,
            );
            editor.rhs_editor().update(cx, |editor, cx| {
                editor.set_should_serialize(false, cx);
                editor.set_read_only(true);
            });
            editor
        });

        let view = cx.new(|_| Self {
            diff_editor,
            title,
            path,
        });
        workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
        view
    }
}

impl EventEmitter<EditorEvent> for StashDiffView {}

impl Focusable for StashDiffView {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.diff_editor.focus_handle(cx)
    }
}

impl Item for StashDiffView {
    type Event = EditorEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Diff).color(Color::Muted))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(self.path.clone())
    }

    fn to_item_events(event: &EditorEvent, f: &mut dyn FnMut(ItemEvent)) {
        Editor::to_item_events(event, f)
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Stash Diff View Opened")
    }

    fn deactivated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.diff_editor
            .update(cx, |editor, cx| editor.deactivated(window, cx));
    }

    fn act_as_type<'a>(
        &'a self,
        type_id: TypeId,
        self_handle: &'a Entity<Self>,
        cx: &'a App,
    ) -> Option<gpui::AnyEntity> {
        if type_id == TypeId::of::<Self>() {
            Some(self_handle.clone().into())
        } else if type_id == TypeId::of::<SplittableEditor>() {
            Some(self.diff_editor.clone().into())
        } else if type_id == TypeId::of::<Editor>() {
            Some(self.diff_editor.read(cx).rhs_editor().clone().into())
        } else {
            None
        }
    }

    fn as_searchable(&self, _: &Entity<Self>, _: &App) -> Option<Box<dyn SearchableItemHandle>> {
        Some(Box::new(self.diff_editor.clone()))
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        f: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        self.diff_editor.read(cx).for_each_project_item(cx, f)
    }

    fn active_project_path(&self, cx: &App) -> Option<ProjectPath> {
        self.diff_editor.read(cx).active_project_path(cx)
    }

    fn set_nav_history(
        &mut self,
        nav_history: ItemNavHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rhs = self.diff_editor.read(cx).rhs_editor().clone();
        rhs.update(cx, |editor, _| {
            editor.set_nav_history(Some(nav_history));
        });
    }

    fn navigate(
        &mut self,
        data: Arc<dyn Any + Send>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.diff_editor
            .update(cx, |editor, cx| editor.navigate(data, window, cx))
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_editor.update(cx, |editor, cx| {
            editor.added_to_workspace(workspace, window, cx)
        });
    }

    fn can_save(&self, cx: &App) -> bool {
        self.diff_editor.read(cx).can_save(cx)
    }

    fn save(
        &mut self,
        options: SaveOptions,
        project: Entity<Project>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.diff_editor
            .update(cx, |editor, cx| editor.save(options, project, window, cx))
    }
}

impl Render for StashDiffView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.diff_editor.clone()
    }
}

/// Builds the read-only current-version buffer and diff entity for a stash
/// file. The caller is responsible for loading the stash blob text.
pub async fn build_stash_diff(
    stash_text: String,
    current_text: String,
    path: &std::path::Path,
    project: Entity<Project>,
    cx: &mut gpui::AsyncWindowContext,
) -> Result<(Entity<Buffer>, Entity<BufferDiff>)> {
    let languages = project.read_with(cx, |project, _| project.languages().clone());
    let language = languages.load_language_for_file_path(path).await.ok();

    let (current_buffer, base_snapshot, diff, base_text) = cx.update(|_window, cx| {
        let current_buffer = cx.new(|cx| {
            let mut buffer = Buffer::local(current_text, cx);
            buffer.set_language_registry(languages.clone());
            buffer.set_language(language.clone(), cx);
            buffer.set_capability(Capability::ReadOnly, cx);
            buffer
        });
        let base_buffer = cx.new(|cx| {
            let mut buffer = Buffer::local(stash_text, cx);
            buffer.set_language_registry(languages);
            buffer.set_language(language, cx);
            buffer.set_capability(Capability::ReadOnly, cx);
            buffer
        });

        let current_snapshot = current_buffer.read(cx).text_snapshot();
        let base_snapshot = base_buffer.read(cx).snapshot();
        let base_text = Arc::<str>::from(base_snapshot.text());
        let diff =
            cx.new(|cx| BufferDiff::new_with_base_text_buffer(&current_snapshot, base_buffer, cx));
        (current_buffer, base_snapshot, diff, base_text)
    })?;

    let snapshot = current_buffer.read_with(cx, |buffer, _| buffer.text_snapshot());
    let update = diff
        .update(cx, |diff, cx| {
            diff.update_diff(snapshot, &base_snapshot, Some(base_text), cx)
        })
        .await;
    diff.update(cx, |diff, cx| diff.set_snapshot(update, cx));

    Ok((current_buffer, diff))
}
