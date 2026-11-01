//! Drag handling for placement mode.

use gtk::prelude::*;

use crate::config::PlacementChange;
use crate::layout::{self, Rect};
use crate::placement::{Drag, Key, Placement};

const GRID: i32 = 8;
const EDGE_SNAP: i32 = 14;

impl Placement {
    pub(crate) fn press(&mut self, connector: &str, x: f64, y: f64) {
        if !self.overlays.contains_key(connector) {
            return;
        }
        let local = (x as i32, y as i32);

        let drag = self
            .items
            .iter()
            .enumerate()
            .rev()
            .find(|(_, item)| {
                item.monitor == connector
                    && local.0 >= item.rect.x
                    && local.1 >= item.rect.y
                    && local.0 < item.rect.x + item.rect.w
                    && local.1 < item.rect.y + item.rect.h
            })
            .map(|(index, item)| Drag {
                item: index,
                grab_x: x - item.rect.x as f64,
                grab_y: y - item.rect.y as f64,
                target: connector.to_string(),
                rect: item.rect,
            });

        if let Some(drag) = drag {
            self.drag = Some(drag);
        }
    }

    pub(crate) fn drag_motion(&mut self, connector: &str, x: f64, y: f64) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        let item = self.items[drag.item].clone();

        let Some(origin) = self
            .overlays
            .get(connector)
            .map(|overlay| (overlay.monitor.x, overlay.monitor.y))
        else {
            return;
        };

        let global_x = origin.0 + x as i32;
        let global_y = origin.1 + y as i32;

        let monitors = self.monitors.clone();

        let target_index = if item.is_all {
            monitors
                .iter()
                .position(|monitor| monitor.connector == item.monitor)
                .unwrap_or(0)
        } else {
            layout::monitor_at(global_x, global_y, &monitors).unwrap_or_else(|| {
                monitors
                    .iter()
                    .position(|monitor| monitor.connector == drag.target)
                    .unwrap_or(0)
            })
        };

        let Some(target) = monitors.get(target_index) else {
            return;
        };

        let local_x = global_x - target.x;
        let local_y = global_y - target.y;

        let geometry = self
            .overlays
            .get(&target.connector)
            .map(|overlay| overlay.usable_geometry())
            .unwrap_or_else(|| target.geometry());

        let rect = layout::snap_rect(
            Rect {
                x: (local_x as f64 - drag.grab_x).round() as i32,
                y: (local_y as f64 - drag.grab_y).round() as i32,
                w: item.rect.w,
                h: item.rect.h,
            },
            geometry,
            GRID,
            EDGE_SNAP,
        );

        if let Some(drag) = &mut self.drag {
            drag.target = target.connector.clone();
            drag.rect = rect;
        }

        let Some(outline) = item.outline.clone() else {
            return;
        };
        let Some(label) = item.label.clone() else {
            return;
        };

        if let Some(parent) = outline
            .parent()
            .and_then(|parent| parent.downcast::<gtk::Fixed>().ok())
        {
            parent.remove(&outline);
        }
        if let Some(parent) = label
            .parent()
            .and_then(|parent| parent.downcast::<gtk::Fixed>().ok())
        {
            parent.remove(&label);
        }

        for overlay in self.overlays.values_mut() {
            overlay.outlines.retain(|widget| widget != &outline);
            overlay.labels.retain(|widget| widget != &label);
        }

        if let Some(overlay) = self.overlays.get_mut(&target.connector) {
            overlay.fixed.put(&outline, rect.x as f64, rect.y as f64);
            overlay
                .fixed
                .put(&label, rect.x as f64 + 8.0, rect.y as f64 + 6.0);
            overlay.outlines.push(outline);
            overlay.labels.push(label);
            overlay.layout_hint();
        }
    }

    pub(crate) fn release(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };

        let item = self.items[drag.item].clone();

        let Some(target) = self
            .overlays
            .get(&drag.target)
            .map(|overlay| overlay.monitor.clone())
        else {
            return;
        };

        let (anchor, offset) = layout::nearest_anchor(
            drag.rect,
            self.overlays
                .get(&drag.target)
                .map(|overlay| overlay.usable_geometry())
                .unwrap_or_else(|| target.geometry()),
        );
        let key = if item.is_all {
            Key::PerOutput(target.key())
        } else {
            Key::Base
        };

        let change = PlacementChange {
            name: item.name.clone(),
            output: if item.is_all {
                None
            } else {
                Some(target.key())
            },
            per_output: if item.is_all {
                Some(target.key())
            } else {
                None
            },
            anchor,
            offset,
        };

        self.staged.insert((item.name.clone(), key.clone()), change);

        if let Some(slot) = self.items.get_mut(drag.item) {
            slot.rect = drag.rect;
            if !item.is_all {
                slot.monitor = target.connector.clone();
            }
            slot.key = key;
        }

        self.render();
    }
}
