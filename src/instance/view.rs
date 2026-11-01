//! Window, CSS, parking and error UI for one widget.

use anyhow::Result;
use gtk::prelude::*;
use mlua::Lua;

use crate::instance::Instance;

impl Instance {
    pub(super) fn apply_css(&mut self) -> Result<()> {
        let scoped = crate::css::scope(&self.comp.css, &self.scope_class());
        let full = format!(
            "window.nitroget {{ background-color: transparent; }}\n\
             window.nitroget.nitroget-parked {{ background-color: rgba(0, 0, 0, 0.004); }}\n\
             window.nitroget > box.nitroget-root {{ background-color: transparent; }}\n\
             {scoped}"
        );

        let provider = gtk::CssProvider::new();
        provider.load_from_string(&full);

        #[allow(deprecated)]
        gtk::style_context_add_provider_for_display(
            &self.display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );

        if let Some(old) = self.provider.replace(provider) {
            #[allow(deprecated)]
            gtk::style_context_remove_provider_for_display(&self.display, &old);
        }

        Ok(())
    }

    /// Hides a widget without destroying its surface. Used when a widget file
    /// is removed or disabled; the surface is reused if it comes back.
    pub fn park(&mut self) {
        self.drain_timers();
        self.tree = None;
        self.textures.clear();
        self.window.set_child(None::<&gtk::Widget>);
        // A fully transparent empty window can make GTK skip the commit,
        // leaving the last frame on screen. A nearly transparent background
        // forces a fresh, blank buffer.
        self.window.add_css_class("nitroget-parked");
        self.window.set_opacity(1.0);
        self.window.set_can_target(false);
        self.window.queue_draw();

        if let Some(provider) = self.provider.take() {
            #[allow(deprecated)]
            gtk::style_context_remove_provider_for_display(&self.display, &provider);
        }

        if let Some(surface) = self.window.surface() {
            let empty = gtk::cairo::Region::create();
            surface.set_input_region(Some(&empty));
        }

        self.lua = Lua::new();
    }

    /// Frees the widget's content and Lua heap while keeping the surface
    /// mapped. Destroying a layer surface disconnects the client on some
    /// compositors, so replaced widgets are kept as empty surfaces.
    pub fn retire(&mut self) {
        self.park();
        self.comp.template.clear();
        self.comp.css.clear();
        self.comp.script.clear();
        self.comp.notes.clear();
    }

    pub(super) fn show_error(&mut self, message: &str) {
        self.drain_timers();
        self.tree = None;

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.add_css_class("nitroget-error");
        root.set_margin_top(10);
        root.set_margin_bottom(10);
        root.set_margin_start(10);
        root.set_margin_end(10);

        let title = gtk::Label::new(Some("widget error"));
        title.add_css_class("nitroget-error-title");
        title.set_xalign(0.0);

        let body = gtk::Label::new(Some(message));
        body.set_wrap(true);
        body.set_xalign(0.0);

        root.append(&title);
        root.append(&body);
        self.window.set_child(Some(&root));
        self.unpark();
        self.window.queue_draw();
    }

    pub(super) fn unpark(&self) {
        self.window.remove_css_class("nitroget-parked");
        self.window.set_opacity(1.0);
        self.window.set_can_target(true);
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(None);
        }
    }
}
