//! Quiet, recoverable notifications: one popup and a session history center.

use std::borrow::Cow;
use web_time::Instant;

use crate::resources::{Notifications, Toast, ToastLevel};
use crate::theme::{ThemePalette, bold_family};
use bevy_egui::egui;

/// Expiry hides popups, preserving unread messages in the notification center.
pub(crate) fn prune_expired_toasts(notifications: &mut Notifications) {
    let now = Instant::now();
    for toast in &mut notifications.toasts {
        if toast.expires_at.is_some_and(|deadline| deadline <= now) {
            toast.popup = false;
        }
    }
}

/// The status-bar entry remains available even when the history is empty.
pub(crate) fn draw_notification_button(
    ui: &mut egui::Ui,
    notifications: &mut Notifications,
    palette: &ThemePalette,
) {
    let unread = notifications.unread_count();
    let color = notifications
        .toasts
        .iter()
        .filter(|toast| !toast.read)
        .map(|toast| toast.level)
        .max()
        .map_or(palette.text_dim, |level| severity(level, palette).2);
    let label = if unread == 0 {
        "Notifications".to_owned()
    } else {
        format!("Notifications · {unread}")
    };
    if ui
        .add(
            egui::Button::new(egui::RichText::new(label).small().color(color))
                .frame(false)
                .selected(notifications.center_open),
        )
        .on_hover_text("Open notification history")
        .clicked()
    {
        if notifications.center_open {
            notifications.center_open = false;
        } else {
            notifications.open_center();
        }
    }
}

/// Draws a compact popup or the full center, above the bottom status bar.
pub(crate) fn draw_notifications(
    ctx: &egui::Context,
    viewport: egui::Rect,
    notifications: &mut Notifications,
    palette: &ThemePalette,
) {
    let center_open = notifications.center_open;
    let popup_id = notifications.visible_toast().map(|toast| toast.id);
    if !center_open && popup_id.is_none() {
        return;
    }
    // Expanded side panels must not squeeze notifications into unreadable cards.
    let viewport = if viewport.width() < 300.0 {
        egui::Rect::from_min_max(
            egui::pos2(ctx.content_rect().left(), viewport.top()),
            egui::pos2(ctx.content_rect().right(), viewport.bottom()),
        )
    } else {
        viewport
    };
    if viewport.width() < 100.0 || viewport.height() < 100.0 {
        return;
    }
    let width = (viewport.width() - 48.0).min(380.0);
    let area = egui::Area::new(egui::Id::new("notifications"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(viewport.right_bottom() + egui::vec2(-12.0, -12.0))
        .show(ctx, |ui| {
            // Area otherwise offers the previous popup's remembered height.
            ui.set_max_height(viewport.height() - 24.0);
            egui::Frame::new()
                .fill(palette.bg_panel)
                .stroke(egui::Stroke::new(1.0, palette.border))
                .corner_radius(10)
                .shadow(ui.visuals().popup_shadow)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.set_width(width);
                    if center_open {
                        draw_center(ui, viewport.height(), notifications, palette);
                    } else {
                        draw_popup(ui, viewport.height(), notifications, palette);
                    }
                });
        });
    // Keep timed popups readable while the pointer or keyboard focus is inside.
    let focused = ctx.memory(egui::Memory::focused).is_some_and(|id| {
        ctx.read_response(id)
            .is_some_and(|response| area.response.rect.intersects(response.rect))
    });
    if let Some(id) = popup_id
        && let Some(toast) = notifications.toasts.iter_mut().find(|toast| toast.id == id)
    {
        if area.response.contains_pointer() || focused {
            toast.expires_at = toast
                .level
                .popup_duration()
                .map(|duration| Instant::now() + duration);
        }
        if let Some(deadline) = toast.expires_at {
            ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
    }
    if center_open
        && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        notifications.center_open = false;
    }
}

fn draw_center(
    ui: &mut egui::Ui,
    height: f32,
    notifications: &mut Notifications,
    palette: &ThemePalette,
) {
    let mut close = false;
    let mut clear = false;
    let mut remove = None;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Notifications")
                .family(bold_family())
                .color(palette.text_bright),
        );
        ui.label(
            egui::RichText::new(notifications.toasts.len().to_string())
                .small()
                .color(palette.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = close_button(ui, "Close notification center").clicked();
        });
    });
    ui.horizontal(|ui| {
        if ui
            .selectable_label(notifications.quiet, "Quiet mode")
            .on_hover_text("Silence info and warning popups. Errors still appear.")
            .clicked()
        {
            notifications.quiet = !notifications.quiet;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            clear = ui
                .add_enabled(
                    !notifications.toasts.is_empty(),
                    egui::Button::new("Clear all").small().frame(false),
                )
                .clicked();
        });
    });
    ui.separator();
    if notifications.toasts.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("You're all caught up").color(palette.text_bright));
            ui.label(
                egui::RichText::new("New notifications will appear here.")
                    .small()
                    .color(palette.text_dim),
            );
        });
        ui.add_space(20.0);
    } else {
        egui::ScrollArea::vertical()
            .id_salt("notification_history")
            .max_height((height - 140.0).clamp(20.0, 420.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for toast in notifications.toasts.iter().rev() {
                    ui.push_id(toast.id, |ui| {
                        egui::Frame::new()
                            .fill(palette.bg_surface)
                            .corner_radius(6)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_width((ui.available_width() - 20.0).max(1.0));
                                notification_heading(ui, toast, palette, &mut remove);
                                ui.add(egui::Label::new(&toast.message).wrap().selectable(true));
                                copy_button(ui, toast);
                            });
                        ui.add_space(4.0);
                    });
                }
            });
    }
    if clear {
        notifications.toasts.clear();
    } else if let Some(id) = remove {
        notifications.toasts.retain(|toast| toast.id != id);
    }
    if close {
        notifications.center_open = false;
    }
}

fn draw_popup(
    ui: &mut egui::Ui,
    height: f32,
    notifications: &mut Notifications,
    palette: &ThemePalette,
) {
    let Some(toast) = notifications.visible_toast() else {
        return;
    };
    let mut hide = None;
    let mut open = false;
    notification_heading(ui, toast, palette, &mut hide);
    egui::ScrollArea::vertical()
        .id_salt(("notification_preview", toast.id))
        .max_height((height - 110.0).clamp(20.0, 100.0))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(message_preview(&toast.message))
                    .wrap()
                    .selectable(true),
            );
        });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        open = ui
            .add(egui::Button::new("Show details").small().frame(false))
            .clicked();
        copy_button(ui, toast);
    });
    if let Some(id) = hide {
        notifications.hide_popup(id);
    }
    if open {
        notifications.open_center();
    }
}

fn notification_heading(
    ui: &mut egui::Ui,
    toast: &Toast,
    palette: &ThemePalette,
    dismiss: &mut Option<u64>,
) {
    let (icon, label, color) = severity(toast.level, palette);
    ui.horizontal(|ui| {
        ui.colored_label(color, icon);
        ui.label(
            egui::RichText::new(label)
                .family(bold_family())
                .color(palette.text_bright),
        );
        if toast.occurrences > 1 {
            ui.label(
                egui::RichText::new(format!("×{}", toast.occurrences))
                    .small()
                    .color(palette.text_dim),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if close_button(ui, "Dismiss notification").clicked() {
                *dismiss = Some(toast.id);
            }
            let seconds = toast.created_at.elapsed().as_secs();
            let age = if seconds < 60 {
                "Just now".to_owned()
            } else {
                format!("{}m ago", seconds / 60)
            };
            ui.label(egui::RichText::new(age).small().color(palette.text_dim));
        });
    });
    ui.add_space(4.0);
}

fn close_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let response = ui
        .add(egui::Button::new("\u{f00d}").frame(false))
        .on_hover_text(label);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    response
}

fn copy_button(ui: &mut egui::Ui, toast: &Toast) {
    if ui
        .add(egui::Button::new("Copy").small().frame(false))
        .on_hover_text("Copy full message")
        .clicked()
    {
        ui.ctx().copy_text(toast.message.clone());
    }
}

fn severity(
    level: ToastLevel,
    palette: &ThemePalette,
) -> (&'static str, &'static str, egui::Color32) {
    match level {
        ToastLevel::Info => ("\u{f111}", "Info", palette.accent_primary),
        ToastLevel::Warn => ("\u{f071}", "Warning", palette.accent_warn),
        ToastLevel::Error => ("\u{f057}", "Error", palette.accent_error),
    }
}

fn message_preview(message: &str) -> Cow<'_, str> {
    match message.char_indices().nth(180) {
        Some((end, _)) => Cow::Owned(format!("{}…", &message[..end])),
        None => Cow::Borrowed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_time::Duration;

    #[test]
    fn notification_controls_copy_full_text_and_recover_dismissed_messages() {
        let ctx = egui::Context::default();
        crate::theme::setup_theme(&ctx, crate::theme::ThemePreset::Light);
        let palette = crate::theme::palette(crate::theme::ThemePreset::Light);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut notifications = Notifications::default();
        let message = "A full error cause chain. ".repeat(20);
        notifications.push_error(&message);
        let frame = |notifications: &mut Notifications, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    draw_notification_button(ui, notifications, palette);
                    draw_notifications(ui.ctx(), screen.shrink(40.0), notifications, palette);
                },
            )
        };
        let click = |notifications: &mut Notifications, output: &egui::FullOutput, label: &str| {
            let pos = output
                .shapes
                .iter()
                .find_map(|shape| text_rect(&shape.shape, label))
                .unwrap_or_else(|| panic!("missing control {label}"))
                .center();
            let events = |pressed| {
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ]
            };
            frame(notifications, events(true)).drop_without_applying_deltas();
            frame(notifications, events(false))
        };
        for _ in 0..2 {
            frame(&mut notifications, Vec::new()).drop_without_applying_deltas();
        }
        let output = frame(&mut notifications, Vec::new());
        let output = click(&mut notifications, &output, "Copy");
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &message)
        ));
        let output = click(&mut notifications, &output, "Show details");
        assert!(notifications.center_open);
        assert_eq!(notifications.unread_count(), 0);
        output.drop_without_applying_deltas();
        frame(&mut notifications, Vec::new()).drop_without_applying_deltas();
        let output = frame(&mut notifications, Vec::new());
        let center_rect = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("notifications")))
            .unwrap();
        let details_rect = output
            .shapes
            .iter()
            .find_map(|shape| text_rect(&shape.shape, &message))
            .unwrap();
        assert!(
            center_rect.contains_rect(details_rect),
            "history must grow beyond the popup so its full details remain readable"
        );
        click(&mut notifications, &output, "Quiet mode").drop_without_applying_deltas();
        assert!(notifications.quiet);
        frame(
            &mut notifications,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        )
        .drop_without_applying_deltas();
        assert!(!notifications.center_open);
        assert!(
            notifications.capture_keyboard,
            "Escape must also be captured by the Bevy viewport input gate"
        );
        notifications.push_error("Another failure");
        frame(&mut notifications, Vec::new()).drop_without_applying_deltas();
        let output = frame(&mut notifications, Vec::new());
        click(&mut notifications, &output, "\u{f00d}").drop_without_applying_deltas();
        assert!(notifications.visible_toast().is_none());
        assert!(notifications.contains_message("Another failure"));
        let output = frame(&mut notifications, Vec::new());
        click(&mut notifications, &output, "Notifications").drop_without_applying_deltas();
        assert!(notifications.center_open);
        assert_eq!(notifications.toasts.len(), 2);
    }

    fn text_rect(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Rect> {
        match shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().find_map(|shape| text_rect(shape, label))
            }
            _ => None,
        }
    }

    #[test]
    fn popup_expiry_preserves_unread_history_and_persistent_errors() {
        let mut notifications = Notifications::default();
        notifications.push_info("saved");
        notifications.push_warn("warning");
        notifications.push_error("failure");
        for toast in &mut notifications.toasts {
            if toast.level != ToastLevel::Error {
                toast.expires_at = Some(Instant::now() - Duration::from_secs(1));
            }
        }
        prune_expired_toasts(&mut notifications);
        assert_eq!(notifications.toasts.len(), 3);
        assert_eq!(notifications.unread_count(), 3);
        assert_eq!(notifications.visible_toast().unwrap().message, "failure");
        assert!(!notifications.toasts[0].popup);
        assert!(!notifications.toasts[1].popup);
        let message = "测".repeat(200);
        let preview = message_preview(&message);
        assert_eq!(preview.chars().count(), 181);
    }
}
