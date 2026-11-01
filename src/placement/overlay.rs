//! Placement overlays: one full-screen layer surface per monitor.

use std::collections::HashSet;

use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::layout::{Geometry, MonitorInfo, Rect};
use crate::placement::Placement;
use crate::state::{Msg, PlacementMsg};

/// Distance from the bottom of the overlay surface to the hint. The overlay is
/// positioned at the usable-area origin, so a panel/dock can shift its visible
/// bottom; keep the controls clear of it.
const HINT_BOTTOM_MARGIN: i32 = 128;

pub(super) struct Overlay {
    pub(super) monitor: MonitorInfo,
    pub(super) window: gtk::Window,
    pub(super) fixed: gtk::Fixed,
    pub(super) hint: gtk::Box,
    pub(super) outlines: Vec<gtk::Widget>,
    pub(super) labels: Vec<gtk::Widget>,
}

impl Overlay {
    /// The usable area the compositor gave this overlay. Layer-shell widgets
    /// are positioned in the same space, so this is the correct coordinate
    /// system for outlines and drags.
    pub(super) fn usable_geometry(&self) -> Geometry {
        Geometry {
            x: 0,
            y: 0,
            width: self.window.width(),
            height: self.window.height(),
        }
    }

    /// True once the compositor has mapped the overlay and given it the
    /// usable-area size that layer-shell widgets use.
    pub(super) fn is_ready(&self) -> bool {
        self.window.is_mapped() && self.window.width() > 1 && self.window.height() > 1
    }

    /// Keep the hint above the outlines and inside the usable area.
    pub(super) fn layout_hint(&mut self) {
        let x = self.window.width().saturating_sub(360) / 2;
        let y = self.window.height().saturating_sub(HINT_BOTTOM_MARGIN);
        self.fixed.remove(&self.hint);
        self.fixed.put(&self.hint, x as f64, y as f64);
    }
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
        // Respect panels/docks the same way widget surfaces do (they use
        // exclusive_zone 0). With all four anchors the compositor stretches
        // this surface to the usable area, which is also the coordinate space
        // layer-shell widgets are positioned in.
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(0);
        window.set_monitor(Some(&monitor.monitor));
        window.set_default_size(monitor.width, monitor.height);

        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));

        let monitor_label = gtk::Label::new(Some(&monitor.label()));
        monitor_label.add_css_class("nitroget-monitor");
        monitor_label.set_can_target(false);
        fixed.put(&monitor_label, 16.0, 12.0);

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
            (monitor.height - HINT_BOTTOM_MARGIN) as f64,
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
            hint,
            outlines: Vec::new(),
            labels: Vec::new(),
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
                for child in overlay.labels.drain(..) {
                    overlay.fixed.remove(&child);
                }
                overlay.hint.set_visible(false);
                overlay.window.queue_draw();
            }
            return;
        }

        let mut created: Vec<(usize, String, Rect, gtk::Widget, gtk::Widget)> = Vec::new();

        {
            let items = &self.items;
            for overlay in self.overlays.values() {
                for child in &overlay.outlines {
                    overlay.fixed.remove(child);
                }
                for child in &overlay.labels {
                    overlay.fixed.remove(child);
                }
            }

            for (index, item) in items.iter().enumerate() {
                let outline = gtk::Box::new(gtk::Orientation::Vertical, 0);
                outline.add_css_class("nitroget-outline");
                outline.set_can_target(false);
                outline.set_size_request(item.rect.w, item.rect.h);

                // The label is a sibling in the overlay, not a child of the
                // outline: a long name must not enlarge the outline.
                let label = gtk::Label::new(Some(&format!(
                    "{} · {}×{}",
                    item.name, item.rect.w, item.rect.h
                )));
                label.add_css_class("nitroget-outline-label");
                label.set_halign(gtk::Align::Start);
                label.set_valign(gtk::Align::Start);
                label.set_can_target(false);

                created.push((
                    index,
                    item.monitor.clone(),
                    item.rect,
                    outline.upcast(),
                    label.upcast(),
                ));
            }
        }

        for overlay in self.overlays.values_mut() {
            overlay.outlines.clear();
            overlay.labels.clear();
        }

        for (index, connector, rect, outline, label) in &created {
            if let Some(overlay) = self.overlays.get_mut(connector) {
                overlay.fixed.put(outline, rect.x as f64, rect.y as f64);
                overlay.outlines.push(outline.clone());
            }
            if let Some(item) = self.items.get_mut(*index) {
                item.outline = Some(outline.clone());
                item.label = Some(label.clone());
            }
        }

        for (_, connector, rect, _, label) in &created {
            if let Some(overlay) = self.overlays.get_mut(connector) {
                overlay
                    .fixed
                    .put(label, rect.x as f64 + 8.0, rect.y as f64 + 6.0);
                overlay.labels.push(label.clone());
            }
        }

        for overlay in self.overlays.values_mut() {
            // Keep the hint above the outlines.
            overlay.layout_hint();
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
