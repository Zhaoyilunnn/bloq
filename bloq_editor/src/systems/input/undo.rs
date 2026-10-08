//! Undo/redo keyboard handling.
use super::*;

/// Handles the undo/redo keyboard shortcuts.
pub(crate) fn undo_redo_system(
    mut intents: ResMut<UiIntentBuffer>,
    ui_input: Res<UiInputState>,
    keyboard_input: Res<ButtonInput<KeyCode>>,
) {
    if ui_input.blocks_viewport_keyboard_input() {
        return;
    }

    if keyboard_input.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
        && keyboard_input.just_pressed(KeyCode::KeyZ)
    {
        intents.push(
            if keyboard_input.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]) {
                UiIntent::Redo
            } else {
                UiIntent::Undo
            },
        );
    }
}
