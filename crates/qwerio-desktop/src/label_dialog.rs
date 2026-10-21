//! Assign workspace labels to a connection, object, document, or folder.
use crate::theme::glass;
use crate::workspace_model::WorkspaceModel;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::{Selectable as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, px,
};
use qwerio_workspace::labels::LabelTarget;

pub fn open(model: Entity<WorkspaceModel>, target: LabelTarget, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let subscription = cx.observe(&model, |_, _, cx| cx.notify());
        LabelAssignments {
            model,
            target,
            is_saving: false,
            _subscription: subscription,
        }
    });
    glass::open_dialog(window, cx, move |dialog, _, _| {
        dialog
            .title("Labels")
            .w(px(360.))
            .child(view.clone())
            .footer(DialogClose::new().child(Button::new("labels-close").label("Close")))
    });
}

struct LabelAssignments {
    model: Entity<WorkspaceModel>,
    target: LabelTarget,
    is_saving: bool,
    _subscription: Subscription,
}

impl LabelAssignments {
    fn toggle(&mut self, label: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        let manifest = self.model.read(cx).manifest();
        let mut labels = manifest
            .label_assignments
            .iter()
            .find(|assignment| assignment.target == self.target)
            .map(|assignment| assignment.labels.clone())
            .unwrap_or_default();
        if labels.contains(&label) {
            labels.retain(|assigned| assigned != &label);
        } else {
            labels.push(label);
        }
        let target = self.target.clone();
        let save = self.model.update(cx, |model, cx| {
            model.update_workspace(cx, move |workspace| workspace.assign_labels(target, labels))
        });
        self.is_saving = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            this.update_in(cx, |this, window, cx| {
                this.is_saving = false;
                if let Err(error) = result {
                    window.push_notification(
                        gpui_kit::component::notification::Notification::error(error),
                        cx,
                    );
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for LabelAssignments {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let manifest = self.model.read(cx).manifest().clone();
        let assigned = manifest
            .label_assignments
            .iter()
            .find(|assignment| assignment.target == self.target);
        let buttons = manifest.labels.iter().map(|label| {
            let label = label.clone();
            Button::new(SharedString::from(format!("assign-label-{label}")))
                .outline()
                .label(label.clone())
                .selected(assigned.is_some_and(|assignment| assignment.labels.contains(&label)))
                .on_click(
                    cx.listener(move |this, _, window, cx| this.toggle(label.clone(), window, cx)),
                )
        });
        v_flex()
            .gap_2()
            .child("Click labels to assign or remove them. Changes save automatically.")
            .children(buttons)
    }
}
