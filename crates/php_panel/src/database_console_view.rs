//! PhpStorm-like full Database Console / Table workspace item.
//!
//! Renders a spacious SQL Console in the top half and interactive data results
//! table in the bottom half inside a center workspace tab.

use crate::PhpPanel;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, WeakEntity, Window, div,
};
use ui::{Color, Icon, IconName, Label, prelude::*};
use workspace::{Item, WorkspaceId};
use workspace::item::ItemEvent;

pub struct DatabaseConsoleView {
    pub(crate) panel: WeakEntity<PhpPanel>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl DatabaseConsoleView {
    pub fn open(panel: Entity<PhpPanel>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let focus_handle = cx.focus_handle();
            let subscription = cx.observe(&panel, |_this: &mut Self, _, cx| {
                cx.emit(ItemEvent::UpdateTab);
                cx.notify();
            });
            Self {
                panel: panel.downgrade(),
                focus_handle,
                _subscription: subscription,
            }
        })
    }
}

impl EventEmitter<ItemEvent> for DatabaseConsoleView {}

impl Focusable for DatabaseConsoleView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for DatabaseConsoleView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::DatabaseZap).color(Color::Accent))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        if let Some(panel) = self.panel.upgrade() {
            let panel = panel.read(cx);
            if let Some(conn) = &panel.db_active_connection {
                return format!("Console: {}", conn).into();
            } else if let Some(first) = panel.databases.first() {
                return format!("Console: {}", first.name).into();
            }
        }
        "Database Console".into()
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
        if let Some(panel) = self.panel.upgrade() {
            Task::ready(Some(Self::open(panel, cx)))
        } else {
            Task::ready(None)
        }
    }

    fn can_split(&self) -> bool {
        true
    }
}

impl Render for DatabaseConsoleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| {
                panel.render_database_center(window, cx)
            })
        } else {
            div()
                .size_full()
                .items_center()
                .justify_center()
                .child(Label::new("Database panel closed").color(Color::Muted))
                .into_any_element()
        }
    }
}
