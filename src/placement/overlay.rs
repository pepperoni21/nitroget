//! Placement overlays: one full-screen layer surface per monitor.

use std::collections::HashSet;

use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::layout::{MonitorInfo, Rect};
use crate::placement::Placement;
use crate::state::{Msg, PlacementMsg};

pub(super) struct Overlay {
    pub(super) monitor: MonitorInfo,
    pub(super) window: gtk::Window,
    pub(super) fixed: gtk::Fixed,
    pub(super) ghost: gtk::Box,
    pub(super) hint: gtk::Box,
    pub(super) outlines: Vec<gtk::Widget>,
}

impl Placement {
    pub(super) fn ensure_overlays(&mut self, monitors: &[MonitorInfo]) {
        if !self.css_ready {
            install_css();
            self.css_ready = true;
        }

        for monitor in monitors {
            if !self.overlays.contains_key(&monitor.connector) {
                let overlay = self.create_overlay(monitor);
                self.overlays.insert(monitor.connector.clone(), overlay);
            }
        }
    }

    fn create_overlay(&mut self, monitor: &MonitorInfo) -> Overlay {
        let tx = self.tx.clone();
        let connector = monitor.connector.clone();

        let window = gtk::Window::new();
        window.add_css_class("nitroget-placement");
        window.set_decorated(false);

        window.init_layer_shell();
        window.set_namespace(Some("nitroget-placement"));
        window.set_layer(Layer::Overlay);
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_monitor(Some(&monitor.monitor));
        window.set_default_size(monitor.width, monitor.height);

        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));

        let monitor_label = gtk::Label::new(Some(&monitor.label()));
        monitor_label.add_css_class("nitroget-monitor");
        monitor_label.set_can_target(false);
        fixed.put(&monitor_label, 16.0, 12.0);

        let ghost = gtk::Box::new(gtk::Orientation::Vertical, 0);
        ghost.add_css_class("nitroget-ghost");
        ghost.set_can_target(false);
        ghost.set_visible(false);
        fixed.put(&ghost, 0.0, 0.0);

        let hint = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hint.add_css_class("nitroget-hint");
        let text = gtk::Label::new(Some("Drag to move"));
        let cancel = gtk::Button::with_label("Cancel");
        let done = gtk::Button::with_label("Done");
        done.add_css_class("suggested-action");
        hint.append(&text);
        hint.append(&cancel);
        hint.append(&done);
        hint.set_size_request(360, -1);
        fixed.put(
            &hint,
            ((monitor.width - 360) / 2) as f64,
            (monitor.height - 64) as f64,
        );

        {
            let tx = tx.clone();
            cancel.connect_clicked(move |_| {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Cancel));
            });
        }
        {
            let tx = tx.clone();
            done.connect_clicked(move |_| {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Done));
            });
        }

        // Drag handling. Outlines are not input targets, so the `GtkFixed` sees
        // every press and motion not consumed by the hint buttons.
        let gesture = gtk::GestureClick::new();
        {
            let tx = tx.clone();
            let connector = connector.clone();
            gesture.connect_pressed(move |_, _, x, y| {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Press {
                    connector: connector.clone(),
                    x,
                    y,
                }));
            });
        }
        {
            let tx = tx.clone();
            gesture.connect_released(move |_, _, _, _| {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Release));
            });
        }
        fixed.add_controller(gesture);

        {
            let tx = tx.clone();
            let connector = connector.clone();
            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(move |_, x, y| {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Motion {
                    connector: connector.clone(),
                    x,
                    y,
                }));
            });
            fixed.add_controller(motion);
        }

        {
            let tx = tx.clone();
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| match key {
                gtk::gdk::Key::Escape => {
                    let _ = tx.try_send(Msg::Placement(PlacementMsg::Cancel));
                    gtk::glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    let _ = tx.try_send(Msg::Placement(PlacementMsg::Done));
                    gtk::glib::Propagation::Stop
                }
                _ => gtk::glib::Propagation::Proceed,
            });
            window.add_controller(keys);
        }

        window.present();

        Overlay {
            monitor: monitor.clone(),
            window,
            fixed,
            ghost,
            hint,
            outlines: Vec::new(),
        }
    }

    pub(super) fn set_ui_active(&mut self, active: bool) {
        let current: HashSet<String> = self
            .monitors
            .iter()
            .map(|monitor| monitor.connector.clone())
            .collect();

        for (connector, overlay) in self.overlays.iter_mut() {
            let active = active && current.contains(connector);

            overlay.window.set_child(Some(&overlay.fixed));
            overlay.fixed.set_visible(active);
            overlay.hint.set_visible(active);
            overlay.ghost.set_visible(false);

            if active {
                overlay.window.remove_css_class("nitroget-placement-parked");
            } else {
                overlay.window.add_css_class("nitroget-placement-parked");
            }
            // Keep opacity at 1: an opacity of 0 can make GTK skip drawing, which
            // leaves the previous (visible) buffer on screen.
            overlay.window.set_opacity(1.0);
            overlay.window.queue_draw();
            overlay.window.set_can_target(active);
            overlay.window.set_keyboard_mode(if active {
                KeyboardMode::Exclusive
            } else {
                KeyboardMode::None
            });

            // `set_can_target` alone does not shrink the layer surface's Wayland
            // input region: the parked full-screen surface would still swallow
            // every click. An empty region passes input through.
            if let Some(surface) = overlay.window.surface() {
                if active {
                    surface.set_input_region(None);
                } else {
                    let empty = gtk::cairo::Region::create();
                    surface.set_input_region(Some(&empty));
                }
            }
        }
    }

    pub(super) fn render(&mut self) {
        if !self.active {
            for overlay in self.overlays.values_mut() {
                for child in overlay.outlines.drain(..) {
                    overlay.fixed.remove(&child);
                }
                overlay.ghost.set_visible(false);
                overlay.hint.set_visible(false);
                overlay.window.queue_draw();
            }
            return;
        }

        let mut created: Vec<(String, Rect, gtk::Widget)> = Vec::new();

        {
            let items = &self.items;
            for overlay in self.overlays.values() {
                for child in &overlay.outlines {
                    overlay.fixed.remove(child);
                }
            }

            for item in items {
                let outline = gtk::Box::new(gtk::Orientation::Vertical, 0);
                outline.add_css_class("nitroget-outline");
                outline.set_can_target(false);
                outline.set_size_request(item.rect.w, item.rect.h);

                let label = gtk::Label::new(Some(&format!(
                    "{} · {}×{}",
                    item.name, item.rect.w, item.rect.h
                )));
                label.add_css_class("nitroget-outline-label");
                label.set_halign(gtk::Align::Start);
                label.set_valign(gtk::Align::Start);
                label.set_margin_start(8);
                label.set_margin_top(6);
                outline.append(&label);

                created.push((item.monitor.clone(), item.rect, outline.upcast()));
            }
        }

        for overlay in self.overlays.values_mut() {
            overlay.outlines.clear();
            overlay.ghost.set_visible(false);
        }

        for (connector, rect, widget) in created {
            if let Some(overlay) = self.overlays.get_mut(&connector) {
                overlay.fixed.put(&widget, rect.x as f64, rect.y as f64);
                overlay.outlines.push(widget);
            }
        }

        for overlay in self.overlays.values_mut() {
            // Keep ghosts and hints above the outlines.
            overlay.fixed.remove(&overlay.ghost);
            overlay.fixed.put(&overlay.ghost, 0.0, 0.0);
            overlay.fixed.remove(&overlay.hint);
            overlay.fixed.put(
                &overlay.hint,
                ((overlay.monitor.width - 360) / 2) as f64,
                (overlay.monitor.height - 64) as f64,
            );
        }
    }

    pub(super) fn set_hint(&mut self, message: &str) {
        for overlay in self.overlays.values() {
            if let Some(label) = overlay
                .hint
                .first_child()
                .and_then(|child| child.downcast::<gtk::Label>().ok())
            {
                label.set_text(message);
            }
        }
    }
}

fn install_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        r#"
        window.nitroget-placement {
            background-color: rgba(0, 0, 0, 0.12);
        }

        window.nitroget-placement.nitroget-placement-parked {
            background-color: rgba(0, 0, 0, 0.005);
        }

        .nitroget-outline {
            border: 2px solid rgba(255, 255, 255, 0.9);
            border-radius: 12px;
            background-color: rgba(99, 102, 241, 0.12);
            box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.35);
        }

        .nitroget-outline-label {
            color: #ffffff;
            font-size: 12px;
            font-weight: 700;
            background-color: rgba(0, 0, 0, 0.65);
            padding: 2px 7px;
            border-radius: 6px;
        }

        .nitroget-ghost {
            border: 2px solid #818cf8;
            border-radius: 12px;
            background-color: rgba(99, 102, 241, 0.25);
        }

        .nitroget-monitor {
            color: rgba(255, 255, 255, 0.8);
            font-size: 12px;
            font-weight: 700;
            background-color: rgba(0, 0, 0, 0.55);
            padding: 3px 8px;
            border-radius: 6px;
        }

        .nitroget-hint {
            background-color: rgba(12, 12, 18, 0.92);
            border-radius: 12px;
            padding: 8px 12px;
        }

        .nitroget-hint label {
            color: #d4d4d8;
            font-size: 12px;
        }

        .nitroget-hint button {
            background-color: rgba(255, 255, 255, 0.10);
            color: #f4f4f5;
            border-radius: 8px;
            padding: 4px 12px;
            font-size: 12px;
        }

        .nitroget-hint button.suggested-action {
            background-color: #6366f1;
            color: #ffffff;
            font-weight: 700;
        }
        "#,
    );

    #[allow(deprecated)]
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
