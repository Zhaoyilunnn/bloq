//! The viewport half of action authoring: the pulse on the elements a pick would
//! accept, and the two ways a pick ends without a click (`Esc`, tab switch).
//! Picks are armed from the Actions window, so no key chord lives here.
use super::*;
use crate::resources::EditorTabId;

/// Seconds per full pulse of the candidate overlay.
const CANDIDATE_PULSE_PERIOD_SECS: f32 = 1.1;
const CANDIDATE_PULSE_MIN_ALPHA: f32 = 0.35;
const CANDIDATE_PULSE_MAX_ALPHA: f32 = 1.0;

/// Cancels a pick that can no longer be answered, and republishes the candidate
/// set the highlight sync pulses.
pub(crate) fn action_pick_system(
    mut editor_state: ResMut<EditorState>,
    graph_state: Res<GraphState>,
    mut action_edit: ResMut<ActionEditState>,
    tabs: Res<EditorTabs>,
    ui_input: Res<UiInputState>,
    keys: Res<ButtonInput<KeyCode>>,
    mut last_tab: Local<Option<EditorTabId>>,
) {
    // A draft names positions in the graph it was started on, so it cannot
    // survive a tab switch.
    if last_tab.is_some_and(|previous| previous != tabs.active) {
        action_edit.cancel();
    }
    *last_tab = Some(tabs.active);

    if !ui_input.blocks_viewport_keyboard_input() && keys.just_pressed(KeyCode::Escape) {
        action_edit.cancel();
    }

    // Mirrored onto `EditorState` so the highlight sync reads one resource;
    // written only on a real change, or every frame would look like an edit.
    let candidates = action_edit.pick_candidates(&graph_state.graph);
    if editor_state.action_candidate_elements != candidates {
        editor_state.action_candidate_elements = candidates;
    }
}

/// Breathes the candidate overlay's alpha while a pick is armed, so the elements
/// waiting to be clicked read as a prompt rather than a static selection.
pub(crate) fn pulse_action_candidate_material_system(
    time: Res<Time>,
    editor_state: Res<EditorState>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if editor_state.action_candidate_elements.is_empty() {
        return;
    }
    let Some(mut material) = materials.get_mut(&editor_state.action_candidate_material) else {
        return;
    };
    material
        .base_color
        .set_alpha(candidate_pulse_alpha(time.elapsed_secs()));
}

fn candidate_pulse_alpha(elapsed_secs: f32) -> f32 {
    let phase = (elapsed_secs / CANDIDATE_PULSE_PERIOD_SECS) * std::f32::consts::TAU;
    let unit = 0.5 * (1.0 - phase.cos());
    CANDIDATE_PULSE_MIN_ALPHA + unit * (CANDIDATE_PULSE_MAX_ALPHA - CANDIDATE_PULSE_MIN_ALPHA)
}

#[cfg(test)]
mod tests {
    use super::{CANDIDATE_PULSE_MAX_ALPHA, CANDIDATE_PULSE_MIN_ALPHA, candidate_pulse_alpha};

    /// The pulse has to stay inside its band and actually move; a constant alpha
    /// would read as a selection rather than a prompt.
    #[test]
    fn candidate_pulse_stays_in_band_and_moves() {
        let band = CANDIDATE_PULSE_MIN_ALPHA..=CANDIDATE_PULSE_MAX_ALPHA;
        let samples: Vec<f32> = (0..64)
            .map(|step| candidate_pulse_alpha(step as f32 * 0.05))
            .collect();

        assert!(samples.iter().all(|alpha| band.contains(alpha)));
        let min = samples.iter().copied().fold(f32::INFINITY, f32::min);
        let max = samples.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max - min > 0.5,
            "the pulse must be visible, got {min}..{max}"
        );
    }
}
