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
    /// Usable-area size last observed for each monitor. Used to wait for a
    /// stable size before painting, and to render immediately on later
    /// placement sessions.
    observed: HashMap<String, (i32, i32)>,
    reflow_scheduled: bool,
    reflow_attempts: u8,
    stable_checks: u8,
    stable: bool,
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
            observed: HashMap::new(),
            reflow_scheduled: false,
            reflow_attempts: 0,
            stable_checks: 0,
            stable: false,
        }
    }

    pub fn toggle(&mut self, live: &HashMap<String, LiveWidget>) {
        if self.active {
            self.cancel();
        } else {
            self.start(live);
        }
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
        self.reflow_scheduled = false;
        self.reflow_attempts = 0;
        self.stable_checks = 0;
        self.stable = false;
        self.set_ui_active(true);

        // On a later placement session the overlays are already mapped, so
        // their size should still match the last stable observation and we can
        // paint immediately. On the first session, wait for a stable size.
        let current = self.current_geometry();
        if current
            .as_ref()
            .is_some_and(|geometry| *geometry == self.observed)
        {
            self.stable = true;
            if let Some(geometry) = &current {
                self.update_rects(geometry);
            }
            self.render();
        } else {
            self.schedule_reflow();
        }
    }

    /// Wait for every overlay to report a usable size, then require that size
    /// to remain unchanged for one polling interval before painting. This
    /// avoids the outline being drawn at an intermediate compositor size and
    /// then corrected.
    pub fn reflow(&mut self) {
        self.reflow_scheduled = false;

        if !self.active || self.drag.is_some() {
            return;
        }

        let Some(current) = self.current_geometry() else {
            self.stable = false;
            if self.reflow_attempts < 40 {
                self.reflow_attempts += 1;
                self.schedule_reflow();
            }
            return;
        };

        if self.observed != current {
            self.observed = current.clone();
            self.stable = false;
            self.stable_checks = 0;
            self.reflow_attempts = 0;
            self.update_rects(&current);
            for overlay in self.overlays.values_mut() {
                overlay.layout_hint();
            }
            self.schedule_reflow();
            return;
        }

        if self.stable {
            return;
        }

        // Give the compositor time to finish configuring a freshly mapped
        // overlay. A short stable window avoids painting at an intermediate
        // size and then correcting the outlines.
        self.stable_checks += 1;
        if self.stable_checks < 8 {
            self.update_rects(&current);
            for overlay in self.overlays.values_mut() {
                overlay.layout_hint();
            }
            self.schedule_reflow();
            return;
        }

        self.update_rects(&current);
        for overlay in self.overlays.values_mut() {
            overlay.layout_hint();
        }
        self.render();
        self.stable = true;
        self.reflow_attempts = 0;
    }

    fn current_geometry(&self) -> Option<HashMap<String, (i32, i32)>> {
        let mut geometry = HashMap::new();

        for item in &self.items {
            let overlay = self.overlays.get(&item.monitor)?;
            if !overlay.is_ready() {
                return None;
            }
            let usable = overlay.usable_geometry();
            geometry.insert(item.monitor.clone(), (usable.width, usable.height));
        }

        Some(geometry)
    }

    fn update_rects(&mut self, geometry: &HashMap<String, (i32, i32)>) {
        for item in &mut self.items {
            let Some(&(width, height)) = geometry.get(&item.monitor) else {
                continue;
            };
            item.rect = layout::rect_for(
                &item.placement,
                layout::Geometry {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
            );
        }
    }

    fn schedule_reflow(&mut self) {
        if self.reflow_scheduled {
            return;
        }
        self.reflow_scheduled = true;

        let tx = self.tx.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(50), move || {
            let _ = tx.try_send(Msg::Placement(PlacementMsg::Reflow));
        });
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
