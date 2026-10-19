//! Choose two readable lines for the fixed-width icon labels.

use gpui_kit::{IntoElement, ParentElement as _, SharedString, Styled as _, Window, div};

// The tile is 8rem wide with 0.5rem padding on each side.
const LABEL_WIDTH_REMS: f32 = 7.0;
const LABEL_FONT_SIZE_REMS: f32 = 0.875;
const MIN_LINE_CHARACTERS: usize = 2;

pub(super) fn render(name: &str, window: &Window) -> impl IntoElement {
    let name = name.replace(['\r', '\n'], " ");
    let style = window.text_style();
    let line = window.text_system().shape_line(
        SharedString::from(name.clone()),
        window.rem_size() * LABEL_FONT_SIZE_REMS,
        &[style.to_run(name.len())],
        None,
    );
    let split = split_index(
        &name,
        line.width.into(),
        (window.rem_size() * LABEL_WIDTH_REMS).into(),
        |index| line.x_for_index(index).into(),
    );
    let (first, second) = name.split_at(split.unwrap_or(name.len()));

    div().w_full().text_sm().text_center().children(
        [first, second]
            .into_iter()
            .filter(|line| !line.is_empty())
            .map(|line| div().w_full().truncate().child(line.to_owned())),
    )
}

fn split_index(
    name: &str,
    width: f32,
    available_width: f32,
    position_at: impl Fn(usize) -> f32,
) -> Option<usize> {
    let characters = name.chars().count();
    if width <= available_width || characters < MIN_LINE_CHARACTERS * 2 {
        return None;
    }

    let mut balanced = None;
    let mut separated = None;
    let mut balanced_difference = f32::INFINITY;
    let mut separated_difference = f32::INFINITY;

    for (character_index, (byte_index, character)) in name.char_indices().enumerate() {
        let first_characters = character_index + 1;
        if first_characters < MIN_LINE_CHARACTERS
            || characters - first_characters < MIN_LINE_CHARACTERS
        {
            continue;
        }

        let index = byte_index + character.len_utf8();
        let first_width = position_at(index);
        let second_width = width - first_width;
        let difference = (first_width - second_width).abs();
        if difference < balanced_difference {
            balanced = Some(index);
            balanced_difference = difference;
        }

        let is_separator = matches!(character, '_' | '-' | '.') || character.is_whitespace();
        if is_separator
            && first_width <= available_width
            && second_width <= available_width
            && difference < separated_difference
        {
            separated = Some(index);
            separated_difference = difference;
        }
    }

    separated.or(balanced)
}

#[cfg(test)]
mod tests {
    use super::split_index;

    fn split(name: &str, available_width: f32) -> (&str, &str) {
        let index = split_index(
            name,
            name.chars().count() as f32,
            available_width,
            |index| name[..index].chars().count() as f32,
        )
        .unwrap_or(name.len());
        name.split_at(index)
    }

    #[test]
    fn keeps_a_final_letter_with_the_rest_of_its_word() {
        assert_eq!(split("import_job_stage", 15.0), ("import_", "job_stage"));
    }

    #[test]
    fn leaves_names_that_fit_on_one_line() {
        assert_eq!(split("customers", 9.0), ("customers", ""));
        assert_eq!(split("", 9.0), ("", ""));
    }

    #[test]
    fn balances_names_without_separators() {
        assert_eq!(split("abcdefghijklmnop", 15.0), ("abcdefgh", "ijklmnop"));
    }

    #[test]
    fn uses_a_character_boundary_for_unicode_names() {
        assert_eq!(split("éééééé", 5.0), ("ééé", "ééé"));
    }

    #[test]
    fn avoids_a_separator_that_would_leave_an_overflowing_line() {
        assert_eq!(split("a_bcdefghijklmnop", 9.0), ("a_bcdefg", "hijklmnop"));
    }

    #[test]
    fn balances_by_rendered_width_rather_than_character_count() {
        let name = "WWWiiiiii";
        let index = split_index(name, 15.0, 14.0, |index| {
            name[..index]
                .chars()
                .map(|character| if character == 'W' { 3.0 } else { 1.0 })
                .sum()
        });
        assert_eq!(index, Some(2));
    }

    #[test]
    fn balances_very_long_names_and_keeps_short_names_together() {
        assert_eq!(split("abcdefghijklmnop", 4.0), ("abcdefgh", "ijklmnop"));
        assert_eq!(split("abc", 2.0), ("abc", ""));
    }
}
