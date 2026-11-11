//! PhpStorm/Postman-like full HTTP client workspace item.
//!
//! Renders the HTTP request builder in the top half (50%) and the HTTP
//! response inspector in the bottom half (50%), with full width and height
//! inside a center workspace tab.

use crate::PhpPanel;
use editor::actions as editor_actions;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div,
};
use ui::{Color, Icon, IconName, Label, prelude::*};
use workspace::{Item, WorkspaceId};
use workspace::item::ItemEvent;

pub struct HttpClientView {
    pub(crate) panel: WeakEntity<PhpPanel>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl HttpClientView {
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

impl EventEmitter<ItemEvent> for HttpClientView {}

impl Focusable for HttpClientView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for HttpClientView {
    type Event = ItemEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Server).color(Color::Accent))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        if let Some(panel) = self.panel.upgrade() {
            let panel = panel.read(cx);
            if let Some(req) = &panel.http_active_request {
                if !req.name.is_empty() {
                    format!("HTTP: {}", req.name).into()
                } else if !req.url.is_empty() {
                    format!("HTTP: {}", req.url).into()
                } else {
                    format!("HTTP: {}", panel.http_method).into()
                }
            } else if !panel.http_active_url.is_empty() {
                let url = panel.http_active_url.trim();
                let short_url = if let Some(pos) = url.find("://") {
                    let rest = &url[pos + 3..];
                    if let Some(slash) = rest.find('/') {
                        &rest[slash..]
                    } else {
                        rest
                    }
                } else {
                    url
                };
                format!("HTTP: {} {}", panel.http_method, short_url).into()
            } else if !panel.http_method.is_empty() {
                format!("HTTP: {}", panel.http_method).into()
            } else {
                "HTTP Client".into()
            }
        } else {
            "HTTP Client".into()
        }
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some("PhpWorkbench HTTP Client — Top: Request Body, Bottom: Response".into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("HTTP Client Opened")
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
        if let Some(panel) = self.panel.upgrade() {
            Task::ready(Some(Self::open(panel, cx)))
        } else {
            Task::ready(None)
        }
    }

    fn to_item_events(_event: &ItemEvent, _f: &mut dyn FnMut(ItemEvent)) {}
}

impl Render for HttpClientView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let Some(panel) = self.panel.upgrade() else {
            return div()
                .id("http-client-view-empty")
                .size_full()
                .items_center()
                .justify_center()
                .bg(colors.background)
                .child(
                    Label::new("HTTP client unavailable (PHP panel closed)")
                        .color(Color::Muted),
                )
                .into_any_element();
        };

        div()
            .id("http-client-view")
            .size_full()
            .bg(colors.background)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &editor_actions::Tab, window, cx| {
                if let Some(panel) = this.panel.upgrade() {
                    panel.update(cx, |panel, cx| {
                        panel.focus_next_http_field(window, cx);
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &editor_actions::Backtab, window, cx| {
                if let Some(panel) = this.panel.upgrade() {
                    panel.update(cx, |panel, cx| {
                        panel.focus_prev_http_field(window, cx);
                    });
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" {
                    cx.stop_propagation();
                    if let Some(panel) = this.panel.upgrade() {
                        panel.update(cx, |panel, cx| {
                            if event.keystroke.modifiers.shift {
                                panel.focus_prev_http_field(window, cx);
                            } else {
                                panel.focus_next_http_field(window, cx);
                            }
                        });
                    }
                }
            }))
            .child(panel.update(cx, |panel, cx| {
                panel.render_http_center(window, cx)
            }))
            .into_any_element()
    }
}
