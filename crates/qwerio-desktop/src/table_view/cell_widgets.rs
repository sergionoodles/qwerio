//! GPUI input and calendar widgets for a cell draft.
use super::cell_editor::{CellEditing, DEFAULT_LABEL, Grid, GridContext, focus_grid};
use chrono::{Datelike as _, NaiveDate};
use gpui_kit::component::calendar::{CalendarEvent, CalendarState};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::{AppContext as _, Entity, SharedString, Subscription, Window};
use qwerio_app::table_edit_session::CellDraft;
use qwerio_core::cell_input::{CellEditor, parse_date};
use qwerio_core::display::NULL_LABEL;
use qwerio_core::value::Value;

/// The open text editor of one cell.
pub struct ActiveEdit {
    /// Grid row: new rows first, then loaded rows.
    pub row: usize,
    /// Index into the browser's columns, not the visible columns.
    pub column: usize,
    pub(super) draft: CellDraft,
    pub(super) input: Entity<InputState>,
    pub(super) calendar: Option<Entity<CalendarState>>,
    pub(super) is_calendar_open: bool,
    pub(super) error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl ActiveEdit {
    pub(super) fn new(
        row: usize,
        column: usize,
        editing: CellEditing,
        value: Option<&Value>,
        window: &mut Window,
        cx: &mut GridContext,
    ) -> Self {
        let draft = CellDraft::new(editing.kind, editing.is_nullable, value);
        let initial_text = draft.initial_text.clone();
        let placeholder = match value {
            None => DEFAULT_LABEL,
            Some(value) if value.is_null() => NULL_LABEL,
            Some(_) => "",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |input, cx| {
            input.set_value(initial_text.clone(), window, cx);
            input.select_all(window, cx);
            input.focus(window, cx);
        });

        let mut subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |table: &mut Grid, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value();
                    if let Some(edit) = table.delegate_mut().editing.as_mut() {
                        edit.draft.record_input_change(text.as_ref());
                        cx.notify();
                    }
                }
                if matches!(event, InputEvent::PressEnter { .. })
                    && table.delegate_mut().commit_edit(cx)
                {
                    focus_grid(table, window, cx);
                }
            },
        )];

        let has_calendar = matches!(editing.kind, CellEditor::Date | CellEditor::DateTime { .. });
        let calendar = has_calendar.then(|| {
            let calendar = cx.new(|cx| CalendarState::new(window, cx));

            if let Some(date) = initial_date(&initial_text) {
                calendar.update(cx, |calendar, cx| calendar.set_date(date, window, cx));
            }

            subscriptions.push(cx.subscribe_in(
                &calendar,
                window,
                |table: &mut Grid, _, event: &CalendarEvent, window, cx| {
                    let CalendarEvent::Selected(date) = event;
                    let Some(date) = date.start() else {
                        return;
                    };

                    let is_done = table.delegate_mut().pick_date(date, window, cx);

                    if is_done && table.delegate_mut().commit_edit(cx) {
                        focus_grid(table, window, cx);
                    }
                },
            ));

            calendar
        });

        Self {
            row,
            column,
            draft,
            input,
            calendar,
            is_calendar_open: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }
}

/// The date at the start of an editor's text, if it is a valid date.
fn initial_date(text: &str) -> Option<NaiveDate> {
    let (year, month, day) = parse_date(text.get(..10)?)?;
    NaiveDate::from_ymd_opt(year, month, day)
}

/// The editor text after picking `date`: the date alone, or for date-times
/// the date followed by the current time of day (midnight when there is
/// none yet).
pub(super) fn text_with_date(kind: CellEditor, current: &str, date: NaiveDate) -> String {
    let day = format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day());

    if kind == CellEditor::Date {
        return day;
    }

    let time = current
        .get(10..)
        .map(str::trim_start)
        .filter(|rest| !rest.is_empty())
        .map(|rest| rest.trim_start_matches(['T', 't']))
        .unwrap_or("00:00:00");

    format!("{day} {time}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("date")
    }

    #[test]
    fn picking_a_date_keeps_the_time_of_date_times() {
        let date_time = CellEditor::DateTime { has_zone: true };

        assert_eq!(
            text_with_date(CellEditor::Date, "2020-01-01", date(2024, 5, 9)),
            "2024-05-09"
        );
        assert_eq!(
            text_with_date(date_time, "2020-01-01 13:45:00+02", date(2024, 5, 9)),
            "2024-05-09 13:45:00+02"
        );
        assert_eq!(
            text_with_date(date_time, "2020-01-01T08:00", date(2024, 5, 9)),
            "2024-05-09 08:00"
        );
        assert_eq!(
            text_with_date(date_time, "", date(2024, 5, 9)),
            "2024-05-09 00:00:00"
        );
    }

    #[test]
    fn the_calendar_opens_at_the_cells_date() {
        assert_eq!(initial_date("2024-02-29 10:00"), Some(date(2024, 2, 29)));
        assert_eq!(initial_date("garbage"), None);
        assert_eq!(initial_date(""), None);
    }
}
