//! Monitor geometry, effective placement and snapping helpers.
//!
//! This is the single place that turns `anchor + offset + output` into pixels,
//! shared by the renderer (via `main`) and the mouse placement mode.

use gtk::prelude::*;

use crate::component::{Component, DEFAULT_HEIGHT, DEFAULT_WIDTH, Placement};
use crate::config::Config;

#[derive(Clone)]
pub struct MonitorInfo {
    pub index: i32,
    pub connector: String,
    pub alias: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub monitor: gtk::gdk::Monitor,
}

impl MonitorInfo {
    /// Alias if the machine defines one, else the connector.
    pub fn key(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.connector.clone())
    }

    pub fn label(&self) -> String {
        match &self.alias {
            Some(alias) => format!("{alias} · {}", self.connector),
            None => self.connector.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl MonitorInfo {
    pub fn geometry(&self) -> Geometry {
        Geometry {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

pub fn monitors(config: &Config) -> Vec<MonitorInfo> {
    let Some(display) = gtk::gdk::Display::default() else {
        return Vec::new();
    };
    let list = display.monitors();
    let mut out = Vec::new();

    for index in 0..list.n_items() {
        let Some(object) = list.item(index) else {
            continue;
        };
        let Ok(monitor) = object.downcast::<gtk::gdk::Monitor>() else {
            continue;
        };

        let connector = monitor
            .connector()
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("monitor-{index}"));
        let geometry = monitor.geometry();

        out.push(MonitorInfo {
            index: index as i32,
            alias: config.alias_for(&connector).map(str::to_string),
            connector,
            x: geometry.x(),
            y: geometry.y(),
            width: geometry.width(),
            height: geometry.height(),
            monitor,
        });
    }

    out
}

/// Stable signature of the current monitor layout for the hot-reload watcher.
pub fn monitor_signature() -> String {
    let Some(display) = gtk::gdk::Display::default() else {
        return String::new();
    };
    let list = display.monitors();
    let mut out = String::new();

    for index in 0..list.n_items() {
        if let Some(object) = list.item(index)
            && let Ok(monitor) = object.downcast::<gtk::gdk::Monitor>()
        {
            out.push_str(
                monitor
                    .connector()
                    .map(|name| name.to_string())
                    .unwrap_or_default()
                    .as_str(),
            );
            let geometry = monitor.geometry();
            out.push_str(&format!(
                ":{}x{}@{},{};",
                geometry.width(),
                geometry.height(),
                geometry.x(),
                geometry.y()
            ));
        }
    }

    out
}

pub fn rect_for(placement: &Placement, geometry: Geometry) -> Rect {
    let (top, bottom, left, right) = crate::instance::parse_anchor(placement.anchor());
    let (ox, oy) = placement.offset();
    let w = placement
        .width
        .unwrap_or(DEFAULT_WIDTH)
        .min(geometry.width.max(1));
    let h = placement
        .height
        .unwrap_or(DEFAULT_HEIGHT)
        .min(geometry.height.max(1));

    let x = if left {
        ox
    } else if right {
        geometry.width - w - ox
    } else {
        (geometry.width - w) / 2
    };

    let y = if top {
        oy
    } else if bottom {
        geometry.height - h - oy
    } else {
        (geometry.height - h) / 2
    };

    Rect { x, y, w, h }
}

pub fn resolve_monitor(placement: &Placement, monitors: &[MonitorInfo]) -> Option<usize> {
    if let Some(index) = placement.output_index {
        return monitors.iter().position(|monitor| monitor.index == index);
    }

    match placement.output.as_deref() {
        None | Some("" | "auto" | "focused" | "primary" | "all") => Some(0),
        Some(name) => monitors
            .iter()
            .position(|monitor| monitor.connector == name || monitor.connector.contains(name)),
    }
}

/// Expands `output = "all"` into one component per monitor and applies
/// per-monitor overrides. Single-output widgets also pick up an override for
/// their resolved monitor.
pub fn expand(
    components: Vec<Component>,
    config: &Config,
    monitors: &[MonitorInfo],
) -> Vec<Component> {
    let mut out = Vec::with_capacity(components.len());

    for mut comp in components {
        if comp.placement.output.as_deref() == Some("all") {
            if monitors.is_empty() {
                comp.placement.output = None;
                out.push(comp);
                continue;
            }

            for monitor in monitors {
                let mut copy = comp.clone();
                copy.placement.output = None;
                copy.placement.output_index = Some(monitor.index);
                apply_override(&mut copy, config, monitor);
                out.push(copy);
            }
        } else {
            if let Some(index) = resolve_monitor(&comp.placement, monitors) {
                apply_override(&mut comp, config, &monitors[index]);
            }
            out.push(comp);
        }
    }

    out
}

fn apply_override(comp: &mut Component, config: &Config, monitor: &MonitorInfo) {
    let alias = monitor.alias.clone();
    let mut keys = Vec::with_capacity(2);
    if let Some(alias) = alias.as_deref() {
        keys.push(alias);
    }
    keys.push(monitor.connector.as_str());

    config.apply_output_override(comp, &keys);
}

/// Chooses the nearest edge/corner and the margins from those edges.
pub fn nearest_anchor(rect: Rect, geometry: Geometry) -> (String, (i32, i32)) {
    let left = rect.x.max(0);
    let right = (geometry.width - rect.x - rect.w).max(0);
    let top = rect.y.max(0);
    let bottom = (geometry.height - rect.y - rect.h).max(0);

    let (horizontal, ox) = if left <= right {
        ("left", left)
    } else {
        ("right", right)
    };
    let (vertical, oy) = if top <= bottom {
        ("top", top)
    } else {
        ("bottom", bottom)
    };

    let anchor = match (vertical, horizontal) {
        ("top", "left") => "top-left",
        ("top", "right") => "top-right",
        ("bottom", "left") => "bottom-left",
        _ => "bottom-right",
    };

    (anchor.to_string(), (ox, oy))
}

/// Rounds an arbitrary widget position to the nearest grid and snaps flush to
/// the monitor edges when close enough.
pub fn snap_rect(mut rect: Rect, geometry: Geometry, grid: i32, edge: i32) -> Rect {
    let grid = grid.max(1);
    rect.x = ((rect.x as f64 / grid as f64).round() as i32) * grid;
    rect.y = ((rect.y as f64 / grid as f64).round() as i32) * grid;

    if rect.x <= edge {
        rect.x = 0;
    }
    if rect.y <= edge {
        rect.y = 0;
    }
    if (geometry.width - rect.w - rect.x).abs() <= edge {
        rect.x = geometry.width - rect.w;
    }
    if (geometry.height - rect.h - rect.y).abs() <= edge {
        rect.y = geometry.height - rect.h;
    }

    rect.x = rect.x.clamp(0, (geometry.width - rect.w).max(0));
    rect.y = rect.y.clamp(0, (geometry.height - rect.h).max(0));
    rect
}

pub fn monitor_at(global_x: i32, global_y: i32, monitors: &[MonitorInfo]) -> Option<usize> {
    monitors.iter().position(|monitor| {
        global_x >= monitor.x
            && global_x < monitor.x + monitor.width
            && global_y >= monitor.y
            && global_y < monitor.y + monitor.height
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry() -> Geometry {
        Geometry {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        }
    }

    fn placement(anchor: &str, offset: (i32, i32)) -> Placement {
        Placement {
            anchor: Some(anchor.into()),
            offset: Some(offset),
            width: Some(100),
            height: Some(50),
            ..Default::default()
        }
    }

    #[test]
    fn computes_corner_rects() {
        assert_eq!(
            rect_for(&placement("top-left", (10, 20)), geometry()),
            Rect {
                x: 10,
                y: 20,
                w: 100,
                h: 50
            }
        );
        assert_eq!(
            rect_for(&placement("top-right", (10, 20)), geometry()),
            Rect {
                x: 690,
                y: 20,
                w: 100,
                h: 50
            }
        );
        assert_eq!(
            rect_for(&placement("bottom-right", (10, 20)), geometry()),
            Rect {
                x: 690,
                y: 530,
                w: 100,
                h: 50
            }
        );
    }

    #[test]
    fn centers_when_unanchored() {
        let p = placement("center", (0, 0));
        assert_eq!(
            rect_for(&p, geometry()),
            Rect {
                x: 350,
                y: 275,
                w: 100,
                h: 50
            }
        );
    }

    #[test]
    fn chooses_nearest_edges() {
        let rect = Rect {
            x: 20,
            y: 500,
            w: 100,
            h: 50,
        };
        let (anchor, offset) = nearest_anchor(rect, geometry());
        assert_eq!(anchor, "bottom-left");
        assert_eq!(offset, (20, 50));

        let rect = Rect {
            x: 650,
            y: 20,
            w: 100,
            h: 50,
        };
        let (anchor, offset) = nearest_anchor(rect, geometry());
        assert_eq!(anchor, "top-right");
        assert_eq!(offset, (50, 20));
    }

    #[test]
    fn snaps_to_grid_and_edges() {
        let rect = Rect {
            x: 7,
            y: 9,
            w: 100,
            h: 50,
        };
        assert_eq!(
            snap_rect(rect, geometry(), 8, 0),
            Rect {
                x: 8,
                y: 8,
                w: 100,
                h: 50
            }
        );

        let near_right = Rect {
            x: 694,
            y: 100,
            w: 100,
            h: 50,
        };
        assert_eq!(snap_rect(near_right, geometry(), 8, 14).x, 700);
    }

    #[test]
    fn automatic_axes_use_layout_fallbacks() {
        let placement = Placement {
            anchor: Some("top-left".into()),
            ..Default::default()
        };
        let rect = rect_for(&placement, geometry());
        assert_eq!((rect.w, rect.h), (DEFAULT_WIDTH, DEFAULT_HEIGHT));
    }
}
