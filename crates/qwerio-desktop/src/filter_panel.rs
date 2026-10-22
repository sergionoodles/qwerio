//! Record filters for a browsed table, shown in the sidebar. Filters run in
//! the database over the whole table, not over loaded rows. The condition
//! being edited applies after a short pause; "Add" keeps it.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, IconName, IndexPath, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div,
};
use qwerio_core::filter::{Filter, FilterOperand, FilterOperator, OperandArity};
use qwerio_core::value::Value;

/// Pause after typing before the edited condition applies.
const DRAFT_DEBOUNCE: Duration = Duration::from_millis(350);

pub enum FilterPanelEvent {
    Changed(Vec<Filter>),
}

pub struct FilterPanel {
    columns: Vec<SharedString>,
    column_select: Entity<SelectState<Vec<SharedString>>>,
    operator_select: Entity<SelectState<Vec<SharedString>>>,
    value_input: Entity<InputState>,
    pinned: Vec<Filter>,
    applied: Vec<Filter>,
    debounce: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FilterPanelEvent> for FilterPanel {}

fn operator_labels() -> Vec<SharedString> {
    FilterOperator::ALL
        .iter()
        .map(|operator| SharedString::from(operator.label()))
        .collect()
}

/// Human-readable form of a filter, e.g. `name contains "ann"`.
pub fn describe_filter(filter: &Filter) -> SharedString {
    let operand = match &filter.operand {
        FilterOperand::None => String::new(),
        FilterOperand::Single(value) => format!(" {}", quote_value(value)),
        FilterOperand::List(values) => format!(
            " ({})",
            values
                .iter()
                .map(quote_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    format!("{} {}{operand}", filter.column, filter.operator.label()).into()
}

fn quote_value(value: &Value) -> String {
    match value {
        Value::Text(text) => format!("“{text}”"),
        other => other.canonical_text().unwrap_or_else(|| "NULL".to_owned()),
    }
}

/// Builds the condition being edited, if it is complete.
pub fn draft_filter(column: Option<&str>, operator: FilterOperator, value: &str) -> Option<Filter> {
    let column = column?;

    let operand = match operator.operand_arity() {
        OperandArity::None => FilterOperand::None,
        OperandArity::Single if value.is_empty() => return None,
        OperandArity::Single => FilterOperand::Single(Value::Text(value.to_owned())),
        OperandArity::List => {
            let values: Vec<Value> = value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::Text(item.to_owned()))
                .collect();

            if values.is_empty() {
                return None;
            }

            FilterOperand::List(values)
        }
    };

    Some(Filter::new(column, operator, operand))
}

impl FilterPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let column_select =
            cx.new(|cx| SelectState::new(Vec::<SharedString>::new(), None, window, cx));
        let operator_select = cx
            .new(|cx| SelectState::new(operator_labels(), Some(IndexPath::default()), window, cx));
        let value_input = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));

        let subscriptions = vec![
            cx.subscribe(
                &column_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| this.apply_now(cx),
            ),
            cx.subscribe(
                &operator_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| this.apply_now(cx),
            ),
            cx.subscribe_in(
                &value_input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.apply_later(cx),
                    InputEvent::PressEnter { .. } => this.pin_draft(window, cx),
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            ),
        ];

        Self {
            columns: Vec::new(),
            column_select,
            operator_select,
            value_input,
            pinned: Vec::new(),
            applied: Vec::new(),
            debounce: None,
            _subscriptions: subscriptions,
        }
    }

    /// Offers the table's columns and restores saved filters.
    pub fn set_columns(
        &mut self,
        columns: Vec<SharedString>,
        filters: Vec<Filter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.columns = columns.clone();
        let first = (!columns.is_empty()).then(IndexPath::default);
        self.column_select.update(cx, |select, cx| {
            select.set_items(columns, window, cx);
            select.set_selected_index(first, window, cx);
        });
        self.pinned = filters.clone();
        self.applied = filters;
        cx.notify();
    }

    /// Adds a filter from elsewhere, such as "Filter by this value".
    pub fn add_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if !self.pinned.contains(&filter) {
            self.pinned.push(filter);
        }

        self.apply_now(cx);
    }

    fn draft(&self, cx: &Context<Self>) -> Option<Filter> {
        let column = self.column_select.read(cx).selected_value().cloned();
        let operator_label = self.operator_select.read(cx).selected_value().cloned()?;
        let operator = FilterOperator::ALL
            .into_iter()
            .find(|operator| operator.label() == operator_label.as_ref())?;
        let value = self.value_input.read(cx).value();

        draft_filter(column.as_deref(), operator, value.trim())
    }

    fn current_filters(&self, cx: &Context<Self>) -> Vec<Filter> {
        let mut filters = self.pinned.clone();

        if let Some(draft) = self.draft(cx)
            && !filters.contains(&draft)
        {
            filters.push(draft);
        }

        filters
    }

    fn apply_later(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_DEBOUNCE).await;
            this.update(cx, |this, cx| this.apply_now(cx)).ok();
        }));
    }

    fn apply_now(&mut self, cx: &mut Context<Self>) {
        self.debounce = None;
        let filters = self.current_filters(cx);

        if filters != self.applied {
            self.applied = filters.clone();
            cx.emit(FilterPanelEvent::Changed(filters));
        }

        cx.notify();
    }

    fn pin_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.draft(cx) else {
            return;
        };

        if !self.pinned.contains(&draft) {
            self.pinned.push(draft);
        }

        self.value_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.apply_now(cx);
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.pinned.len() {
            self.pinned.remove(index);
        }

        self.apply_now(cx);
    }

    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pinned.clear();
        self.value_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.apply_now(cx);
    }

    fn needs_value(&self, cx: &Context<Self>) -> bool {
        let Some(label) = self.operator_select.read(cx).selected_value() else {
            return true;
        };

        FilterOperator::ALL
            .into_iter()
            .find(|operator| operator.label() == label.as_ref())
            .is_none_or(|operator| operator.operand_arity() != OperandArity::None)
    }
}

impl Render for FilterPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let needs_value = self.needs_value(cx);
        let pinned = self.pinned.iter().enumerate().map(|(index, filter)| {
            h_flex()
                .gap_1()
                .text_sm()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(describe_filter(filter)),
                )
                .child(
                    Button::new(SharedString::from(format!("remove-filter-{index}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .accessibility_label("Remove filter")
                        .tooltip("Remove filter")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove(index, cx))),
                )
        });

        v_flex()
            .gap_2()
            .when(self.columns.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Columns load with the table."),
                )
            })
            .children(pinned)
            .child(
                Select::new(&self.column_select)
                    .small()
                    .placeholder("Column"),
            )
            .child(Select::new(&self.operator_select).small())
            .when(needs_value, |this| {
                this.child(Input::new(&self.value_input).small())
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("add-filter")
                            .small()
                            .outline()
                            .label("Add")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pin_draft(window, cx)),
                            ),
                    )
                    .when(!self.applied.is_empty(), |this| {
                        this.child(
                            Button::new("clear-filters")
                                .small()
                                .ghost()
                                .label("Clear")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.clear(window, cx)),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Filters apply to the whole table. Use commas for “in”."),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_need_a_column_and_a_value_unless_the_operator_has_none() {
        assert_eq!(draft_filter(None, FilterOperator::Equals, "1"), None);
        assert_eq!(draft_filter(Some("id"), FilterOperator::Equals, ""), None);
        assert_eq!(
            draft_filter(Some("id"), FilterOperator::IsNull, ""),
            Some(Filter::is_null("id"))
        );
    }

    #[test]
    fn in_lists_split_on_commas() {
        let filter = draft_filter(Some("id"), FilterOperator::In, " 1, 2,,3 ");

        assert_eq!(
            filter.map(|filter| filter.operand),
            Some(FilterOperand::List(vec![
                Value::Text("1".to_owned()),
                Value::Text("2".to_owned()),
                Value::Text("3".to_owned()),
            ]))
        );
    }

    #[test]
    fn filters_describe_themselves() {
        let filter = Filter::new(
            "name",
            FilterOperator::Contains,
            FilterOperand::Single(Value::Text("ann".to_owned())),
        );

        assert_eq!(describe_filter(&filter), "name contains “ann”");
    }
}
