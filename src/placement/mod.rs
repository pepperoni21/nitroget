//! Mouse placement mode.
//!
//! A modal overlay on the `overlay` layer, one per monitor, shows dashed
//! outlines at each widget's position. Dragging moves the outline; dropping
//! stages the change. `Done` writes everything to `config.toml` through
//! `toml_edit`.
//!
//! Widgets themselves are never moved during the drag: layer surfaces cannot be
//! repositioned freely and remapping them is both slow and, on some
//! compositors, fatal to the client connection.

mod drag;
mod overlay;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use async_channel::Sender;

use crate::component::{Component, Placement as ComponentPlacement};
use crate::config::{self, PlacementChange};
use crate::layout::{self, MonitorInfo, Rect};
use crate::state::{LiveWidget, Msg, PlacementMsg};

use overlay::Overlay;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Key {
    Base,
    PerOutput(String),
}

#[derive(Clone)]
pub(super) struct Item {
    pub(super) name: String,
    pub(super) is_all: bool,
    /// Connector of the monitor this outline is on.
    pub(super) monitor: String,
    pub(super) rect: Rect,
    pub(super) key: Key,
    /// Effective placement (including the actual mapped size).
    pub(super) placement: ComponentPlacement,
    /// The outline and label currently drawn for this item.
    pub(super) outline: Option<gtk::Widget>,
    pub(super) label: Option<gtk::Widget>,
}

#[derive(Clone)]
pub(super) struct Drag {
    pub(super) item: usize,
    pub(super) grab_x: f64,
    pub(super) grab_y: f64,
    pub(super) target: String,
    pub(super) rect: Rect,
}

pub struct Placement {
    active: bool,
    overlays: HashMap<String, Overlay>,
    items: Vec<Item>,
    monitors: Vec<MonitorInfo>,
    staged: HashMap<(String, Key), PlacementChange>,
    config_path: PathBuf,
    target: Option<PathBuf>,
    windowed: bool,
    drag: Option<Drag>,
    css_ready: bool,
    tx: Sender<Msg>,
}

impl Placement {
    pub fn new(
        target: Option<PathBuf>,
        config_path: PathBuf,
        windowed: bool,
        tx: Sender<Msg>,
    ) -> Self {
        Self {
            active: false,
            overlays: HashMap::new(),
            items: Vec::new(),
            monitors: Vec::new(),
            staged: HashMap::new(),
            config_path,
            target,
            windowed,
            drag: None,
            css_ready: false,
            tx,
        }
    }

    pub fn toggle(&mut self, live: &HashMap<String, LiveWidget>) {
        if self.active {
            self.cancel();
        } else {
            self.start(live);
        }
    }

    /// Create and park overlays while the daemon is idle. By the time the user
    /// enters placement, the compositor has already mapped them and reported
    /// their usable-area size, so outlines can be painted once in the final
    /// position.
    pub fn prepare(&mut self) {
        if self.active || self.windowed || !gtk4_layer_shell::is_supported() {
            return;
        }

        let Ok(config) = config::load(&self.config_path) else {
            return;
        };
        let monitors = layout::monitors(&config);
        if monitors.is_empty() {
            return;
        }

        self.monitors = monitors;
        let monitors = self.monitors.clone();
        self.ensure_overlays(&monitors);
        self.set_ui_active(false);
    }

    pub fn start(&mut self, live: &HashMap<String, LiveWidget>) {
        if self.active {
            return;
        }

        if self.windowed {
            eprintln!("nitroget: placement mode is not available with --windowed");
            return;
        }

        if !gtk4_layer_shell::is_supported() {
            eprintln!("nitroget: placement mode requires wlr-layer-shell");
            return;
        }

        let (config, mut components, monitors) =
            match load(self.target.as_deref(), &self.config_path) {
                Ok(value) => value,
                Err(e) => {
                    eprintln!("nitroget: cannot start placement mode: {e:#}");
                    return;
                }
            };

        if monitors.is_empty() {
            eprintln!("nitroget: no monitors found");
            return;
        }

        // Widgets with no explicit output were placed by the compositor. Ask
        // the running instances which monitor they actually landed on.
        for comp in &mut components {
            if comp.placement.output.is_none()
                && let Some(info) = live.get(&comp.name)
            {
                comp.placement.output = Some(info.connector.clone());
            }
        }

        let all_names: HashSet<String> = components
            .iter()
            .filter(|comp| config.is_all(&comp.name))
            .map(|comp| comp.name.clone())
            .collect();

        let expanded = layout::expand(components, &config, &monitors);

        self.monitors = monitors;
        self.staged.clear();
        self.drag = None;
        self.active = true;

        let monitors = self.monitors.clone();
        self.ensure_overlays(&monitors);

        let mut items = Vec::new();
        for comp in expanded {
            let Some(index) = layout::resolve_monitor(&comp.placement, &monitors) else {
                eprintln!(
                    "nitroget: `{}` targets a monitor that is not attached; skipping",
                    comp.name
                );
                continue;
            };
            let monitor = &monitors[index];
            // Prefer the size GTK actually gave the mapped surface. The
            // declared size is only a request and can differ after content or
            // theme changes; an outline based on it would not match the widget.
            let mut placement = comp.placement.clone();
            if let Some(info) = live.get(&comp.name)
                && info.width > 0
                && info.height > 0
            {
                placement.width = Some(info.width);
                placement.height = Some(info.height);
            }

            // Use the overlay's usable area when it is already mapped (a
            // repeated placement session). On the first session it is mapped
            // asynchronously by the reflow below.
            let geometry = self
                .overlays
                .get(&monitor.connector)
                .filter(|overlay| overlay.is_ready())
                .map(|overlay| overlay.usable_geometry())
                .unwrap_or_else(|| monitor.geometry());

            items.push(Item {
                name: comp.name.clone(),
                is_all: all_names.contains(&comp.name),
                monitor: monitor.connector.clone(),
                rect: layout::rect_for(&placement, geometry),
                key: if all_names.contains(&comp.name) {
                    Key::PerOutput(monitor.key())
                } else {
                    Key::Base
                },
                placement: placement.clone(),
                outline: None,
                label: None,
            });
        }

        self.items = items;
        self.set_ui_active(true);

        // Do not paint outlines with the provisional monitor geometry: on the
        // first placement session the overlay is not mapped yet, so its usable
        // area (and therefore the correct positions) is unknown.
        let ready = self.items.iter().all(|item| {
            self.overlays
                .get(&item.monitor)
                .is_some_and(|overlay| overlay.is_ready())
        });
        if ready {
            self.render();
        }

        // The map signal normally triggers this immediately; the timers are a
        // fallback for compositors that configure late.
        for delay in [100, 400] {
            let tx = self.tx.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(delay), move || {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Reflow));
            });
        }
    }

    /// Recompute outlines in the overlays' actual coordinate space.
    pub fn reflow(&mut self) {
        if !self.active || self.drag.is_some() {
            return;
        }

        if !self.items.iter().all(|item| {
            self.overlays
                .get(&item.monitor)
                .is_some_and(|overlay| overlay.is_ready())
        }) {
            return;
        }

        let mut changed = false;
        for item in &mut self.items {
            let Some(overlay) = self.overlays.get(&item.monitor) else {
                continue;
            };
            let usable = overlay.usable_geometry();
            if usable.width <= 0 || usable.height <= 0 {
                continue;
            }
            let rect = layout::rect_for(&item.placement, usable);
            if rect != item.rect {
                item.rect = rect;
                changed = true;
            }
        }

        for overlay in self.overlays.values_mut() {
            overlay.layout_hint();
        }

        if changed {
            self.render();
        }
    }

    pub fn cancel(&mut self) {
        self.staged.clear();
        self.drag = None;
        self.active = false;
        self.set_ui_active(false);
        self.render();
    }

    pub fn done(&mut self) -> Result<()> {
        let changes: Vec<PlacementChange> = self.staged.values().cloned().collect();

        if !changes.is_empty()
            && let Err(e) = config::save(&self.config_path, &changes)
        {
            self.set_hint("Could not save — see the terminal for details");
            return Err(e);
        }

        self.staged.clear();
        self.drag = None;
        self.active = false;
        self.set_ui_active(false);

        Ok(())
    }
}

fn load(
    target: Option<&Path>,
    config_path: &Path,
) -> Result<(config::Config, Vec<Component>, Vec<MonitorInfo>)> {
    let paths = crate::discovery::collect_paths(target)?;
    let config = config::load(config_path)?;

    let mut components = Vec::new();
    for path in &paths {
        match crate::component::load(path) {
            Ok(mut comp) => {
                if config.apply(&mut comp) {
                    components.push(comp);
                }
            }
            Err(e) => eprintln!("nitroget: {}: {e:#}", path.display()),
        }
    }

    config::auto_place(&mut components, &config);
    let monitors = layout::monitors(&config);

    Ok((config, components, monitors))
}
