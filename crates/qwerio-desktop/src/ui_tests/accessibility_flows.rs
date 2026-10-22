//! Controls drawn without visible text still have a name for screen readers.

use gpui_kit::test::TestQueryExt as _;
use gpui_kit::{Role, TestAppContext};

use super::schema_flows::open_connection;
use super::*;

#[gpui_kit::test]
fn every_button_in_the_workbench_has_a_name(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    let window = open_connection(&fixture, true, cx);

    interact(window, cx, |window, _| {
        let unnamed = window
            .find_all_by_role(Role::Button)
            .into_iter()
            .filter(|button| button.label().is_none_or(str::is_empty))
            .map(|button| format!("{:?}", button.path().last()))
            .collect::<Vec<_>>();

        assert!(unnamed.is_empty(), "unnamed buttons: {unnamed:#?}");
    });
}
