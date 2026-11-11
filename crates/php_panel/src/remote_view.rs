//! PhpStorm-like full Remote Server / Deployment workspace item.
//!
//! Renders a spacious Remote Host overview, deployment actions, and remote
//! file browser in the center workspace tab.

use crate::PhpPanel;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, WeakEntity, Window, div,
};
use ui::{Color, Icon, IconName, Label, prelude::*};
use workspace::{Item, WorkspaceId};
use workspace::item::ItemEvent;

pub struct RemoteView {
    pub(crate) panel: WeakEntity<PhpPanel>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl RemoteView {
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

impl EventEmitter<ItemEvent> for RemoteView {}

impl Focusable for RemoteView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for RemoteView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Server).color(Color::Accent))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        if let Some(panel) = self.panel.upgrade() {
            let panel = panel.read(cx);
            if let Some(server) = &panel.active_server {
                return format!("Remote: {}", server).into();
            }
        }
        "Remote Host".into()
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

impl Render for RemoteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| {
                panel.render_remote_center(window, cx)
            })
        } else {
            div()
                .size_full()
                .items_center()
                .justify_center()
                .child(Label::new("Remote panel closed").color(Color::Muted))
                .into_any_element()
        }
    }
}
