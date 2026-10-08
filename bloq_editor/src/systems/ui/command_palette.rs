//! The command palette: a fuzzy-searchable overlay over the [`Command`]
//! registry, opened with `Ctrl+Shift+P` or `F1`.
//!
//! The open chord is consumed here, not in `systems::input`, so it fires even
//! while a panel's text field holds focus and egui swallows it before the
//! viewport shortcuts see it.

use bevy::prelude::Resource;
use bevy_egui::egui;

use super::commands::{Availability, Command, CommandContext, all_commands};
use super::intents::UiIntentBuffer;
use super::{activated, fuzzy_score};
use crate::theme::ThemePalette;

const PALETTE_WIDTH: f32 = 620.0;
const PALETTE_TOP_MARGIN: f32 = 96.0;
const PALETTE_ROW_HEIGHT: f32 = 30.0;
const PALETTE_MAX_ROWS: f32 = 12.0;
/// Commands kept in the empty query's "recently used" block.
const RECENT_LIMIT: usize = 5;

const OPEN_SHORTCUT: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::P,
);

/// Open/closed state, the query, and the most-recently-used list. The MRU list
/// is per session: persisting it would mean two backends (a file natively,
/// `localStorage` on the web) for a convenience.
#[derive(Resource, Default)]
pub(crate) struct CommandPaletteState {
    open: bool,
    /// Includes the closing frame: Bevy receives keys independently of egui.
    pub(super) capture_keyboard: bool,
    focus_search: bool,
    query: String,
    selected: usize,
    recent: Vec<Command>,
}

impl CommandPaletteState {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.focus_search = true;
    }

    fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
    }

    fn remember(&mut self, command: Command) {
        self.recent.retain(|entry| *entry != command);
        self.recent.insert(0, command);
        self.recent.truncate(RECENT_LIMIT);
    }
}

/// One scored, presentable row.
struct Row {
    command: Command,
    availability: Availability,
    label: String,
    score: i32,
    /// Position in the MRU list, or `usize::MAX` when never run.
    recency: usize,
}

/// Draws the palette, handling its own open/close chords.
pub(crate) fn draw_command_palette(
    ctx: &egui::Context,
    state: &mut CommandPaletteState,
    command_ctx: &CommandContext<'_>,
    intents: &mut UiIntentBuffer,
    palette: &ThemePalette,
) {
    state.capture_keyboard = state.open;
    let mut scroll_to_selected = false;
    if ctx.input_mut(|input| {
        input.consume_shortcut(&OPEN_SHORTCUT)
            || input.consume_key(egui::Modifiers::NONE, egui::Key::F1)
    }) {
        // Closing clears the query, so opening needs no reset of its own.
        if state.open {
            state.close();
        } else {
            state.open();
            scroll_to_selected = true;
        }
    }
    state.capture_keyboard |= state.open;

    if !state.open {
        return;
    }

    // Navigation keys are consumed before the text field sees them, so the
    // caret keeps its own Left/Right while Up/Down drive the list.
    let (mut step, mut run, mut cancel) = (0isize, false, false);
    ctx.input_mut(|input| {
        step -= isize::from(input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
        step += isize::from(input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
        step -= isize::from(input.consume_key(egui::Modifiers::COMMAND, egui::Key::P));
        step += isize::from(input.consume_key(egui::Modifiers::COMMAND, egui::Key::N));
        run = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        cancel = input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
    });
    if cancel {
        state.close();
        return;
    }
    let screen = ctx.content_rect();
    let width = PALETTE_WIDTH.min((screen.width() - 48.0).max(1.0));
    let top = PALETTE_TOP_MARGIN.min(screen.height() * 0.12);
    let mut chosen = None;

    let modal = egui::Modal::new(egui::Id::new("command_palette"))
        .area(
            egui::Modal::default_area(egui::Id::new("command_palette"))
                .anchor(egui::Align2::CENTER_TOP, [0.0, top]),
        )
        .frame(
            egui::Frame::new()
                .fill(palette.bg_panel)
                .stroke(egui::Stroke::new(1.0, palette.border_bright))
                .corner_radius(8)
                .inner_margin(12),
        )
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.strong("Commands");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    cancel |= ui.button("Close").clicked();
                });
            });
            let search = ui.add(
                egui::TextEdit::singleline(&mut state.query)
                    .desired_width(f32::INFINITY)
                    .hint_text("Search commands, modes, examples..."),
            );
            if std::mem::take(&mut state.focus_search) {
                search.request_focus();
            }
            if search.changed() {
                state.selected = 0;
                scroll_to_selected = true;
            }

            // Rank after TextEdit consumes this frame's text, including paste + Enter.
            let rows = rank_commands(&state.query, command_ctx, &state.recent);
            state.selected = state.selected.min(rows.len().saturating_sub(1));
            if step != 0 && !rows.is_empty() {
                state.selected =
                    (state.selected as isize + step).rem_euclid(rows.len() as isize) as usize;
                scroll_to_selected = true;
            }
            ui.add_space(4.0);
            let row_pitch = PALETTE_ROW_HEIGHT + ui.spacing().item_spacing.y;
            let list_height = (screen.height() - top - 170.0)
                .max(24.0)
                .min(row_pitch * PALETTE_MAX_ROWS);
            egui::ScrollArea::vertical()
                .min_scrolled_height(list_height.min(row_pitch * 5.0))
                .max_height(list_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    if rows.is_empty() {
                        ui.label("No matching commands. Try a shorter search.");
                    }
                    for (index, row) in rows.iter().enumerate() {
                        if draw_row(
                            ui,
                            row,
                            index == state.selected,
                            scroll_to_selected,
                            palette,
                        ) {
                            chosen = Some(row.command);
                        }
                    }
                });
            ui.separator();
            if let Some(row) = rows.get(state.selected)
                && let Availability::Unavailable(reason) = row.availability
            {
                ui.colored_label(palette.accent_warn, reason);
            }
            ui.small("Up/Down to choose · Enter to run · Esc to close");
            if run {
                chosen = rows
                    .get(state.selected)
                    .filter(|row| row.availability.is_available())
                    .map(|row| row.command);
            }
        });
    if cancel || modal.should_close() {
        state.close();
        return;
    }

    if let Some(command) = chosen {
        intents.push(command.into_intent(command_ctx));
        state.remember(command);
        state.close();
    }
}

/// Draws one row and reports whether it was activated.
fn draw_row(
    ui: &mut egui::Ui,
    row: &Row,
    selected: bool,
    scroll_to_selected: bool,
    palette: &ThemePalette,
) -> bool {
    let enabled = row.availability.is_available();
    let text_color = if !enabled {
        palette.text_dim
    } else if selected {
        palette.text_bright
    } else {
        palette.text_primary
    };

    let mut button = egui::Button::new(egui::RichText::new(&row.label).color(text_color))
        .selected(selected)
        .truncate()
        .fill(if selected {
            palette.bg_active
        } else {
            egui::Color32::TRANSPARENT
        })
        .stroke(egui::Stroke::NONE)
        .min_size(egui::vec2(ui.available_width(), PALETTE_ROW_HEIGHT));
    if let Some(keys) = row.command.keybind() {
        button = button.shortcut_text(egui::RichText::new(keys).small());
    }
    let response = ui.add_enabled(enabled, button).on_hover_text(&row.label);

    if selected && scroll_to_selected {
        response.scroll_to_me(None);
    }
    match row.availability {
        Availability::Available => activated(&response),
        Availability::Unavailable(reason) => {
            response.on_disabled_hover_text(reason);
            false
        }
    }
}

/// Filters and orders the registry for `query`. Unavailable commands sink to
/// the bottom but are never hidden: omitting what you searched for teaches
/// nothing.
fn rank_commands(query: &str, ctx: &CommandContext<'_>, recent: &[Command]) -> Vec<Row> {
    let (text, plane_height) = split_trailing_int(query);
    let mut commands = all_commands();
    commands.push(Command::SetPlaneHeight(plane_height));

    let mut rows: Vec<Row> = commands
        .into_iter()
        .filter_map(|command| {
            let label = command.search_label();
            Some(Row {
                score: if text.is_empty() {
                    0
                } else {
                    fuzzy_score(&label, text)?
                },
                recency: recent
                    .iter()
                    .position(|entry| *entry == command)
                    .unwrap_or(usize::MAX),
                availability: command.availability(ctx),
                command,
                label,
            })
        })
        .collect();

    rows.sort_by(|left, right| {
        right
            .availability
            .is_available()
            .cmp(&left.availability.is_available())
            .then_with(|| right.score.cmp(&left.score))
            .then_with(|| left.recency.cmp(&right.recency))
            // Category order, not alphabetical, so an empty query opens on `File`.
            .then_with(|| left.command.category().cmp(&right.command.category()))
            .then_with(|| left.label.cmp(&right.label))
    });
    rows
}

/// Splits a trailing integer off a query — `"plane 3"` becomes
/// `("plane", Some(3))`. The whole of the palette's argument support.
fn split_trailing_int(query: &str) -> (&str, Option<i32>) {
    let query = query.trim();
    let Some((head, tail)) = query.rsplit_once(char::is_whitespace) else {
        return (query, None);
    };
    match tail.parse::<i32>() {
        Ok(value) => (head.trim_end(), Some(value)),
        Err(_) => (query, None),
    }
}

#[cfg(test)]
mod tests {
    use super::super::intents::UiIntent;
    use super::*;
    use crate::resources::GraphState;
    use crate::resources::ImportExportState;
    use crate::resources::{EditorMode, EditorState, EditorTabs};
    use crate::systems::jobs::EditorJobs;
    use crate::theme::{self, ThemePreset};
    use bloq_graph::GalleryItem;
    use std::collections::HashSet;

    #[test]
    fn paste_and_enter_runs_the_current_query_in_a_short_viewport() {
        let ctx = egui::Context::default();
        theme::setup_theme(&ctx, ThemePreset::Light);
        let fixture = Fixture::default();
        let mut state = CommandPaletteState::default();
        let mut intents = UiIntentBuffer::default();
        state.open();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 400.0));
        for events in [
            vec![],
            vec![],
            vec![
                egui::Event::Paste("view reset camera".into()),
                egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ] {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    draw_command_palette(
                        ui.ctx(),
                        &mut state,
                        &fixture.context(),
                        &mut intents,
                        theme::palette(ThemePreset::Light),
                    );
                },
            )
            .drop_without_applying_deltas();
            let rect = ctx
                .memory(|memory| memory.area_rect(egui::Id::new("command_palette")))
                .unwrap();
            assert!(screen.contains_rect(rect), "palette overflowed: {rect:?}");
        }
        assert!(!state.open);
        assert!(
            intents
                .drain()
                .any(|intent| matches!(intent, UiIntent::ResetCamera))
        );
    }

    /// Owns the borrowed halves of a [`CommandContext`] so a test can build one
    /// without keeping five bindings alive by hand.
    #[derive(Default)]
    struct Fixture {
        editor_state: EditorState,
        graph: GraphState,
        tabs: EditorTabs,
        jobs: EditorJobs,
        import_export: ImportExportState,
    }

    impl Fixture {
        fn context(&self) -> CommandContext<'_> {
            CommandContext {
                editor_state: &self.editor_state,
                graph_state: &self.graph,
                tabs: &self.tabs,
                jobs: &self.jobs,
                import_export: &self.import_export,
            }
        }
    }

    /// Runs one palette frame with a single key delivered as raw input.
    fn frame(
        ctx: &egui::Context,
        state: &mut CommandPaletteState,
        intents: &mut UiIntentBuffer,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) {
        let fixture = Fixture::default();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                }],
                ..Default::default()
            },
            |ui| {
                draw_command_palette(
                    ui.ctx(),
                    state,
                    &fixture.context(),
                    intents,
                    theme::palette(ThemePreset::default()),
                );
            },
        )
        .drop_without_applying_deltas();
    }

    /// The whole keyboard round trip: the chord opens the palette, Enter runs
    /// the selection through the intent bus, and running closes it.
    #[test]
    fn the_open_chord_runs_a_command_and_closes() {
        let ctx = egui::Context::default();
        let mut state = CommandPaletteState::default();
        let mut intents = UiIntentBuffer::default();
        let none = egui::Modifiers::NONE;
        let chord = egui::Modifiers {
            ctrl: true,
            shift: true,
            command: true,
            ..Default::default()
        };

        frame(&ctx, &mut state, &mut intents, egui::Key::P, chord);
        assert!(state.open, "Ctrl+Shift+P opens the palette");

        state.query = "view reset camera".to_string();
        frame(&ctx, &mut state, &mut intents, egui::Key::Enter, none);
        assert!(!state.open, "running a command closes the palette");
        assert_eq!(state.recent, vec![Command::ResetCamera]);
        assert!(
            intents
                .drain()
                .any(|intent| matches!(intent, UiIntent::ResetCamera))
        );

        frame(&ctx, &mut state, &mut intents, egui::Key::F1, none);
        assert!(state.open, "F1 is the web-safe alias");
        frame(&ctx, &mut state, &mut intents, egui::Key::Escape, none);
        assert!(!state.open, "Esc closes without running anything");
        assert!(
            state.capture_keyboard,
            "closing must not clear viewport selection"
        );
        frame(&ctx, &mut state, &mut intents, egui::Key::ArrowRight, none);
        assert!(
            !state.capture_keyboard,
            "the next frame releases viewport keys"
        );
    }

    /// Unavailable commands stay listed with a reason, below everything that can
    /// actually run. The `mode` query also proves the category prefix is
    /// searchable — no command title contains it.
    #[test]
    fn unavailable_commands_carry_a_reason_and_sink_to_the_bottom() {
        let fixture = Fixture::default();
        let ctx = fixture.context();
        assert_eq!(
            Command::SetMode(fixture.editor_state.mode).availability(&ctx),
            Availability::Unavailable("already the active mode")
        );
        assert!(
            Command::SetMode(EditorMode::Bloq)
                .availability(&ctx)
                .is_available()
        );
        // A fresh editor has one empty tab and an empty graph.
        assert!(!Command::NextTab.availability(&ctx).is_available());
        assert!(!Command::ValidateGraph.availability(&ctx).is_available());
        assert!(
            !Command::SetPlaneHeight(None)
                .availability(&ctx)
                .is_available()
        );

        let rows = rank_commands("mode", &ctx, &[]);
        let first_unavailable = rows
            .iter()
            .position(|row| !row.availability.is_available())
            .expect("the active mode is listed but unavailable");
        assert!(
            rows[first_unavailable..]
                .iter()
                .all(|row| !row.availability.is_available())
        );

        let loaded = Fixture {
            graph: GraphState {
                graph: GalleryItem::CNOT.build().flatten().unwrap(),
                ..Default::default()
            },
            ..Fixture::default()
        };
        assert!(
            Command::ValidateGraph
                .availability(&loaded.context())
                .is_available()
        );
    }

    #[test]
    fn trailing_integer_becomes_a_plane_height_argument() {
        assert_eq!(split_trailing_int("plane 3"), ("plane", Some(3)));
        assert_eq!(split_trailing_int("plane -2"), ("plane", Some(-2)));
        assert_eq!(split_trailing_int("plane"), ("plane", None));
        assert_eq!(split_trailing_int("t_gate"), ("t_gate", None));
        assert_eq!(split_trailing_int(""), ("", None));
    }

    #[test]
    fn close_command_requires_confirmation() {
        let fixture = Fixture::default();
        assert!(matches!(
            Command::CloseTab.into_intent(&fixture.context()),
            UiIntent::RequestCloseTab(_)
        ));
    }

    #[test]
    fn port_tags_and_selection_duplication_are_registered() {
        let commands = all_commands();
        assert!(commands.contains(&Command::TogglePortTags));
        assert!(commands.contains(&Command::DuplicateSelection));

        let mut fixture = Fixture {
            graph: GraphState {
                graph: GalleryItem::CNOT.build().flatten().unwrap(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            Command::TogglePortTags.into_intent(&fixture.context()),
            UiIntent::SetPortTagVisibility(true)
        ));
        fixture
            .editor_state
            .select_all_elements(&fixture.graph.graph);
        assert!(matches!(
            Command::DuplicateSelection.into_intent(&fixture.context()),
            UiIntent::InsertGraph(graph) if graph.blocks().count() == fixture.graph.graph.blocks().count()
        ));
    }

    /// A nameless command cannot be run, and duplicate labels are ambiguous.
    #[test]
    fn every_command_has_a_unique_non_empty_label() {
        let mut seen = HashSet::new();
        for command in all_commands() {
            let label = command.search_label();
            assert!(!command.title().is_empty(), "{label} has no title");
            assert!(seen.insert(label.clone()), "duplicate label {label}");
        }
    }
}
