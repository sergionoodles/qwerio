//! Custom label definitions; changes save immediately.
use crate::services::Services;
use crate::workspace_model::WorkspaceModel;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Window, div,
};
use qwerio_workspace::labels::{default_labels, validate_label};

use super::setting_card;

const NO_LABELS: &str = "No labels yet. Add one below.";

pub struct LabelSettings {
    model: Option<Entity<WorkspaceModel>>,
    input: Entity<InputState>,
    labels: Vec<String>,
    global_labels: Vec<String>,
    is_loading: bool,
    error: Option<SharedString>,
    is_saving: bool,
}

pub const LABELS_SETTING: &str = "label_definitions";

impl LabelSettings {
    pub fn new(
        model: Option<Entity<WorkspaceModel>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let labels = model
            .as_ref()
            .map(|model| model.read(cx).manifest().labels.clone())
            .unwrap_or_else(default_labels);
        let local = Services::app(cx).local_state.clone();
        let load = cx.background_spawn(async move { local.setting::<Vec<String>>(LABELS_SETTING) });
        cx.spawn(async move |this, cx| {
            let result = load.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Some(labels)) => {
                        this.global_labels = labels.clone();
                        for label in labels {
                            if !this.labels.contains(&label) {
                                this.labels.push(label);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                this.is_loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
        Self {
            model,
            input: cx.new(|cx| InputState::new(window, cx).placeholder("New label")),
            labels,
            global_labels: default_labels(),
            is_loading: true,
            error: None,
            is_saving: false,
        }
    }

    fn add_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving || self.is_loading {
            return;
        }
        let name = match validate_label(self.input.read(cx).value().as_str()) {
            Ok(name) => name,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let labels = if self.labels.contains(&name) {
            self.labels.clone()
        } else {
            self.labels.iter().cloned().chain([name.clone()]).collect()
        };
        let global_labels = if self.global_labels.contains(&name) {
            self.global_labels.clone()
        } else {
            self.global_labels
                .iter()
                .cloned()
                .chain([name.clone()])
                .collect::<Vec<_>>()
        };
        let model = self.model.clone();
        let local = Services::app(cx).local_state.clone();
        let update = model.as_ref().map(|model| {
            model.update(cx, |model, cx| {
                model.update_workspace(cx, move |workspace| workspace.define_label(&name))
            })
        });
        self.is_saving = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = match update {
                Some(update) => update.await,
                None => Ok(()),
            };
            let result = match result {
                Ok(()) => {
                    let saved_labels = global_labels.clone();
                    cx.background_spawn(async move {
                        local
                            .set_setting(LABELS_SETTING, &saved_labels)
                            .map_err(|error| SharedString::from(error.to_string()))
                    })
                    .await
                }
                Err(error) => Err(error),
            };
            this.update_in(cx, |this, window, cx| {
                this.is_saving = false;
                match result {
                    Ok(()) => {
                        this.labels = labels;
                        this.global_labels = global_labels;
                        this.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl LabelSettings {
    fn render_definitions(&self, cx: &App) -> impl IntoElement {
        let tags = self
            .labels
            .iter()
            .map(|label| Tag::secondary().small().child(label.clone()));
        let definitions = h_flex()
            .id("label-definitions")
            .flex_wrap()
            .gap_1p5()
            .children(tags)
            .when(self.labels.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(NO_LABELS),
                )
            });

        v_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .child(div().text_sm().font_medium().child("Defined labels"))
            .child(definitions)
    }

    fn render_new_label(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_2()
            .px_4()
            .py_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).id("new-label-name")),
            )
            .child(
                Button::new("add-label")
                    .primary()
                    .icon(IconName::Plus)
                    .label("Add Label")
                    .loading(self.is_saving || self.is_loading)
                    .on_click(cx.listener(|this, _, window, cx| this.add_label(window, cx))),
            )
    }
}

impl Render for LabelSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_2()
            .child(setting_card(
                [
                    self.render_definitions(cx).into_any_element(),
                    self.render_new_label(cx).into_any_element(),
                ],
                cx,
            ))
            .children(
                self.error
                    .clone()
                    .map(|error| div().text_sm().text_color(cx.theme().danger).child(error)),
            )
    }
}
