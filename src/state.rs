//! Single-owner application state and the message loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use async_channel::Sender;
use gtk::prelude::*;

use crate::component::Placement as ComponentPlacement;
use crate::instance::Instance;
use crate::placement::Placement;
use crate::reconcile;

pub type WidgetKey = (PathBuf, Option<i32>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetState {
    Active,
    Parked,
}

pub struct Running {
    pub instance: Instance,
    pub mtime: SystemTime,
    pub placement: ComponentPlacement,
    pub state: WidgetState,
}

/// Where a running widget actually is, and how large GTK actually made it.
pub struct LiveWidget {
    pub connector: String,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug)]
pub enum Msg {
    /// Re-scan widgets and the config.
    Sync,
    /// A pointer/signal event with Lua code to run.
    Click {
        key: WidgetKey,
        code: String,
    },
    Change {
        key: WidgetKey,
        code: String,
        value: f64,
    },
    Text {
        key: WidgetKey,
        code: String,
        value: String,
    },
    /// Forced re-render from `nitroget.refresh`.
    Render {
        key: WidgetKey,
    },
    Timer {
        key: WidgetKey,
        id: u64,
    },
    RegisterTimer {
        key: WidgetKey,
        id: u64,
        ms: u64,
        once: bool,
    },
    ClearTimer {
        key: WidgetKey,
        id: u64,
    },
    ClearStream {
        key: WidgetKey,
        id: u64,
    },
    StreamLine {
        key: WidgetKey,
        id: u64,
        line: String,
    },
    StreamDone {
        key: WidgetKey,
        id: u64,
        ok: bool,
        error: String,
    },
    ExecDone {
        key: WidgetKey,
        id: u64,
        ok: bool,
        text: String,
    },
    Placement(PlacementMsg),
    Quit,
}

#[derive(Debug)]
pub enum PlacementMsg {
    Toggle,
    Start,
    Cancel,
    Done,
    Press {
        connector: String,
        x: f64,
        y: f64,
    },
    Motion {
        connector: String,
        x: f64,
        y: f64,
    },
    Release,
    /// Recompute outlines after the overlays have been mapped and their
    /// usable-area size is known.
    Reflow,
}

pub struct AppState {
    pub target: Option<PathBuf>,
    pub config_path: PathBuf,
    pub windowed: bool,
    pub app: gtk::Application,
    pub tx: Sender<Msg>,
    pub widgets: HashMap<WidgetKey, Running>,
    pub retired: Vec<Instance>,
    pub placement: Placement,
    /// Removes the control socket when the daemon shuts down cleanly.
    socket: Option<crate::ipc::SocketGuard>,
}

impl AppState {
    pub fn new(
        target: Option<PathBuf>,
        config_path: PathBuf,
        windowed: bool,
        app: gtk::Application,
        tx: Sender<Msg>,
        socket: Option<crate::ipc::SocketGuard>,
    ) -> Self {
        let placement = Placement::new(target.clone(), config_path.clone(), windowed, tx.clone());

        Self {
            target,
            config_path,
            windowed,
            app,
            tx,
            widgets: HashMap::new(),
            retired: Vec::new(),
            placement,
            socket,
        }
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Sync => self.sync(),
            Msg::Click { key, code } => self.run(&key, &code, None),
            Msg::Change { key, code, value } => self.run(&key, &code, Some(value)),
            Msg::Text { key, code, value } => self.run_text(&key, &code, &value),
            Msg::Render { key } => self.render(&key),
            Msg::Timer { key, id } => self.fire_timer(&key, id),
            Msg::RegisterTimer { key, id, ms, once } => self.register_timer(&key, id, ms, once),
            Msg::ClearTimer { key, id } => self.clear_timer(&key, id),
            Msg::ClearStream { key, id } => self.clear_stream(&key, id),
            Msg::StreamLine { key, id, line } => self.stream_line(&key, id, &line),
            Msg::StreamDone { key, id, ok, error } => self.stream_done(&key, id, ok, &error),
            Msg::ExecDone { key, id, ok, text } => self.exec_done(&key, id, ok, &text),
            Msg::Placement(message) => self.placement(message),
            Msg::Quit => {
                // Drop the socket guard while the process still has a chance
                // to clean up; GLib does not drop local futures on shutdown.
                self.socket.take();
                for running in self.widgets.values_mut() {
                    running.instance.drain_streams();
                }
                self.app.quit();
            }
        }
    }

    pub fn sync(&mut self) {
        reconcile::sync(
            &mut self.widgets,
            &mut self.retired,
            self.target.as_deref(),
            &self.config_path,
            self.windowed,
            &self.app,
            &self.tx,
        );
    }

    fn run(&mut self, key: &WidgetKey, code: &str, event: Option<f64>) {
        let Some(running) = self.active(key) else {
            return;
        };
        running.instance.run_code(code, event);
        running.instance.render_now();
    }

    fn run_text(&mut self, key: &WidgetKey, code: &str, value: &str) {
        let Some(running) = self.active(key) else {
            return;
        };
        running.instance.run_text(code, value);
        running.instance.render_now();
    }

    fn stream_line(&mut self, key: &WidgetKey, id: u64, line: &str) {
        let Some(running) = self.active(key) else {
            return;
        };
        running.instance.fire_stream_line(id, line);
        running.instance.render_now();
    }

    fn stream_done(&mut self, key: &WidgetKey, id: u64, ok: bool, error: &str) {
        let Some(running) = self.active(key) else {
            return;
        };
        running.instance.finish_stream(id, ok, error);
        running.instance.render_now();
    }

    fn clear_stream(&mut self, key: &WidgetKey, id: u64) {
        if let Some(running) = self.active(key) {
            running.instance.cancel_stream(id);
        }
    }

    /// Returns the widget only while it is on screen. Stale messages queued
    /// before a widget was parked (a click, a timer, an exec result) must not
    /// rebuild its tree and make the surface visible again.
    fn active(&mut self, key: &WidgetKey) -> Option<&mut Running> {
        self.widgets
            .get_mut(key)
            .filter(|running| running.state == WidgetState::Active)
    }

    fn render(&mut self, key: &WidgetKey) {
        if let Some(running) = self.active(key) {
            running.instance.render_now();
        }
    }

    fn fire_timer(&mut self, key: &WidgetKey, id: u64) {
        let Some(running) = self.active(key) else {
            return;
        };
        let once = running.instance.fire_timer(id);
        if once {
            running.instance.clear_timer(id);
        }
        running.instance.render_now();
    }

    fn register_timer(&mut self, key: &WidgetKey, id: u64, ms: u64, once: bool) {
        if let Some(running) = self.active(key) {
            running.instance.register_timer(id, ms, once);
        }
    }

    fn clear_timer(&mut self, key: &WidgetKey, id: u64) {
        if let Some(running) = self.active(key) {
            running.instance.clear_timer(id);
        }
    }

    fn exec_done(&mut self, key: &WidgetKey, id: u64, ok: bool, text: &str) {
        let Some(running) = self.active(key) else {
            return;
        };
        running.instance.exec_done(id, ok, text);
        running.instance.render_now();
    }

    fn placement(&mut self, message: PlacementMsg) {
        match message {
            PlacementMsg::Toggle => {
                let live = self.live_widgets();
                self.placement.toggle(&live);
            }
            PlacementMsg::Start => {
                let live = self.live_widgets();
                self.placement.start(&live);
            }
            PlacementMsg::Cancel => self.placement.cancel(),
            PlacementMsg::Done => match self.placement.done() {
                Ok(()) => self.sync(),
                Err(e) => eprintln!("nitroget: {e:#}"),
            },
            PlacementMsg::Press { connector, x, y } => self.placement.press(&connector, x, y),
            PlacementMsg::Motion { connector, x, y } => {
                self.placement.drag_motion(&connector, x, y)
            }
            PlacementMsg::Release => self.placement.release(),
            PlacementMsg::Reflow => self.placement.reflow(),
        }
    }

    fn live_widgets(&self) -> HashMap<String, LiveWidget> {
        let display = gtk::gdk::Display::default();
        let mut map = HashMap::new();

        for running in self.widgets.values() {
            let instance = &running.instance;
            let Some(display) = display.as_ref() else {
                continue;
            };
            let Some(surface) = instance.window.surface() else {
                continue;
            };
            let Some(monitor) = display.monitor_at_surface(&surface) else {
                continue;
            };
            let Some(connector) = monitor.connector() else {
                continue;
            };
            map.insert(
                instance.comp.name.clone(),
                LiveWidget {
                    connector: connector.to_string(),
                    width: instance.window.width(),
                    height: instance.window.height(),
                },
            );
        }

        map
    }
}
