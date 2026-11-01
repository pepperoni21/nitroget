//! One live widget instance: window, Lua state, timers, CSS provider.

mod runtime;
mod view;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{Context, Result};
use gtk::prelude::*;
use mlua::Lua;

use crate::component::{Component, Placement};
use crate::state::{Msg, WidgetKey};

pub struct TimerEntry {
    pub source: gtk::glib::SourceId,
    pub once: bool,
    /// Set by the source itself just before a one-shot timer is destroyed.
    /// Removing an already-fired `SourceId` panics, so `cancel` checks this.
    pub fired: Rc<Cell<bool>>,
}

impl TimerEntry {
    fn cancel(self) {
        if self.once && self.fired.get() {
            return;
        }
        self.source.remove();
    }
}

pub struct Instance {
    pub key: WidgetKey,
    pub comp: Component,
    pub window: gtk::Window,
    pub lua: Lua,

    pub timers: HashMap<u64, TimerEntry>,

    /// Persistent render tree; rebuilt only when the component changes.
    pub tree: Option<crate::render::Tree>,
    /// Decoded textures, keyed by file path.
    pub textures: HashMap<String, gtk::gdk::Texture>,

    pub display: gtk::gdk::Display,
    pub provider: Option<gtk::CssProvider>,
    pub use_layer_shell: bool,
    pub tx: async_channel::Sender<Msg>,
}

impl Instance {
    pub fn create(
        comp: Component,
        key: WidgetKey,
        windowed: bool,
        app: &gtk::Application,
        tx: async_channel::Sender<Msg>,
    ) -> Result<Instance> {
        let display = gtk::gdk::Display::default().context("no Wayland display")?;

        // wlr-layer-shell is implemented by wlroots/Smithay compositors, KDE and
        // Mir — not by GNOME. Never assume a specific compositor: probe the
        // protocol and degrade to a normal window when it is missing.
        let use_layer_shell = !windowed && gtk4_layer_shell::is_supported();
        if !windowed && !use_layer_shell {
            eprintln!(
                "nitroget: this compositor does not implement wlr-layer-shell; \
                 drawing `{}` as a normal window",
                comp.name
            );
        }

        let window = new_window(&comp.name, app, use_layer_shell);
        apply_placement(&window, None, &comp.placement, use_layer_shell);

        let mut instance = Instance {
            key,
            comp,
            window,
            lua: Lua::new(),
            timers: HashMap::new(),
            tree: None,
            textures: HashMap::new(),
            display,
            provider: None,
            use_layer_shell,
            tx,
        };

        instance.init()?;
        Ok(instance)
    }

    fn init(&mut self) -> Result<()> {
        self.register_lua()?;

        let script = self.lua.load(&self.comp.script).set_name("script").exec();

        if let Err(e) = script {
            let _ = self.apply_css();
            self.show_error(&format!("{e:#}"));
            self.window.present();
            return Ok(());
        }

        self.apply_css()?;
        self.render_now();
        self.window.present();

        Ok(())
    }

    pub fn scope_class(&self) -> String {
        format!("n-{}", sanitize(&self.comp.name))
    }

    /// Updates only the placement of an already-running widget. Used when the
    /// config (e.g. placement mode) moves a widget without touching its file.
    pub fn set_placement(&mut self, placement: Placement) {
        let previous = std::mem::replace(&mut self.comp.placement, placement.clone());
        apply_placement(
            &self.window,
            Some(&previous),
            &placement,
            self.use_layer_shell,
        );
    }

    /// Rebuilds the Lua runtime and content **in the existing window**.
    ///
    /// Destroying a layer surface and creating a new one trips a teardown bug
    /// in some compositors (notably COSMIC with gtk4-layer-shell < 1.4), which
    /// drops the client's Wayland connection. Reusing the surface keeps hot
    /// reload and config changes safe — and avoids a visible flicker.
    pub fn reload(&mut self, comp: Component) -> Result<()> {
        // Compile before tearing anything down: a syntax error must leave the
        // currently running widget intact.
        {
            let check = Lua::new();
            check
                .load(&comp.script)
                .set_name("script")
                .into_function()
                .with_context(|| format!("script error in {}", comp.path.display()))?;
        }

        self.drain_timers();
        self.tree = None;
        self.textures.clear();

        let previous = std::mem::replace(&mut self.comp, comp);
        apply_placement(
            &self.window,
            Some(&previous.placement),
            &self.comp.placement,
            self.use_layer_shell,
        );

        self.lua = Lua::new();

        if let Err(e) = self.register_lua() {
            let _ = self.apply_css();
            self.show_error(&format!("{e:#}"));
            return Err(e);
        }

        if let Err(e) = self.lua.load(&self.comp.script).set_name("script").exec() {
            let message = format!("script error in {}: {e}", self.comp.path.display());
            let _ = self.apply_css();
            self.show_error(&message);
            return Err(anyhow::anyhow!(message));
        }

        self.apply_css()?;
        self.unpark();
        self.render_now();

        Ok(())
    }

    fn drain_timers(&mut self) {
        for (_, entry) in self.timers.drain() {
            entry.cancel();
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.drain_timers();
        if let Some(provider) = self.provider.take() {
            #[allow(deprecated)]
            gtk::style_context_remove_provider_for_display(&self.display, &provider);
        }
        self.window.close();
    }
}

fn new_window(name: &str, app: &gtk::Application, use_layer_shell: bool) -> gtk::Window {
    use gtk4_layer_shell::{KeyboardMode, LayerShell};

    let window = gtk::Window::new();
    window.add_css_class("nitroget");
    window.set_title(Some(name));
    window.set_decorated(!use_layer_shell);
    window.set_resizable(!use_layer_shell);
    window.set_application(Some(app));

    if use_layer_shell {
        window.init_layer_shell();
        window.set_namespace(Some("nitroget"));
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_exclusive_zone(0);
    }

    window
}

/// Applies placement to an existing window. Layer-shell anchors, margins and
/// layer can change on a mapped surface, so reloads do not need a new surface.
fn apply_placement(
    window: &gtk::Window,
    previous: Option<&Placement>,
    placement: &Placement,
    use_layer_shell: bool,
) {
    window.set_default_size(placement.width, placement.height);

    if !use_layer_shell {
        return;
    }

    use gtk4_layer_shell::{Edge, Layer, LayerShell};

    if previous.is_none_or(|old| old.layer != placement.layer) {
        window.set_layer(match placement.layer.as_str() {
            "background" => Layer::Background,
            "top" => Layer::Top,
            "overlay" => Layer::Overlay,
            _ => Layer::Bottom,
        });
    }

    if previous.is_none_or(|old| {
        old.output != placement.output || old.output_index != placement.output_index
    }) {
        if let Some(index) = placement.output_index {
            match monitor_at(index) {
                Some(monitor) => window.set_monitor(Some(&monitor)),
                None => eprintln!("nitroget: monitor {index} is no longer available"),
            }
        } else {
            match placement.output.as_deref() {
                None | Some("" | "auto" | "focused") => {
                    // Let the compositor place it on its focused output.
                }
                Some("primary") => match primary_monitor() {
                    Some(monitor) => window.set_monitor(Some(&monitor)),
                    None => eprintln!("nitroget: no primary output found"),
                },
                Some(name) => match find_monitor(name) {
                    Some(monitor) => window.set_monitor(Some(&monitor)),
                    None => eprintln!("nitroget: no output named `{name}`"),
                },
            }
        }
    }

    let (top, bottom, left, right) = parse_anchor(placement.anchor());
    window.set_anchor(Edge::Top, top);
    window.set_anchor(Edge::Bottom, bottom);
    window.set_anchor(Edge::Left, left);
    window.set_anchor(Edge::Right, right);

    let (ox, oy) = placement.offset();
    window.set_margin(Edge::Left, if left { ox } else { 0 });
    window.set_margin(Edge::Right, if right { ox } else { 0 });
    window.set_margin(Edge::Top, if top { oy } else { 0 });
    window.set_margin(Edge::Bottom, if bottom { oy } else { 0 });
}

pub fn parse_anchor(anchor: &str) -> (bool, bool, bool, bool) {
    let a = anchor.to_lowercase();
    if a.contains("center") {
        return (false, false, false, false);
    }
    (
        a.contains("top"),
        a.contains("bottom"),
        a.contains("left"),
        a.contains("right"),
    )
}

pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Wayland has no "primary output" concept; GDK's first monitor is the
/// compositor's default, which is the closest portable equivalent.
fn primary_monitor() -> Option<gtk::gdk::Monitor> {
    monitor_at(0)
}

fn monitor_at(index: i32) -> Option<gtk::gdk::Monitor> {
    let display = gtk::gdk::Display::default()?;
    let monitors = display.monitors();

    monitors
        .item(index as u32)
        .and_then(|object| object.downcast::<gtk::gdk::Monitor>().ok())
}

/// Resolves an output by connector name (`DP-1`), model, or a unique
/// substring. GDK-only, so it is independent of the compositor.
fn find_monitor(name: &str) -> Option<gtk::gdk::Monitor> {
    let display = gtk::gdk::Display::default()?;
    let monitors = display.monitors();

    for i in 0..monitors.n_items() {
        let Some(object) = monitors.item(i) else {
            continue;
        };
        let Ok(monitor) = object.downcast::<gtk::gdk::Monitor>() else {
            continue;
        };

        let connector = monitor
            .connector()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let model = monitor.model().map(|s| s.to_string()).unwrap_or_default();

        if connector == name || model == name || connector.contains(name) {
            return Some(monitor);
        }
    }

    None
}
