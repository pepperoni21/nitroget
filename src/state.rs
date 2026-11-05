//! Single-owner application state and the message loop.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use async_channel::Sender;
use gtk::prelude::*;

use crate::component::Placement as ComponentPlacement;
use crate::instance::{Instance, MENU_ANCHOR, MenuAnchor, WidgetRuntime};
use crate::placement::Placement;
use crate::reconcile;

pub type WidgetKey = (PathBuf, Option<i32>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerScope {
    Global,
    Output,
}

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

struct ViewInfo {
    key: WidgetKey,
    runtime: Rc<RefCell<WidgetRuntime>>,
    connector: String,
    index: Option<i32>,
    width: i32,
    height: i32,
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
        index: Option<i64>,
        button: u32,
        x: f64,
        y: f64,
    },
    MenuAction {
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
        path: PathBuf,
    },
    Timer {
        path: PathBuf,
        id: u64,
    },
    RegisterTimer {
        path: PathBuf,
        id: u64,
        ms: u64,
        once: bool,
        scope: TimerScope,
    },
    ClearTimer {
        path: PathBuf,
        id: u64,
    },
    ClearStream {
        path: PathBuf,
        id: u64,
    },
    StreamLine {
        path: PathBuf,
        id: u64,
        line: String,
    },
    StreamDone {
        path: PathBuf,
        id: u64,
        ok: bool,
        error: String,
    },
    ExecDone {
        path: PathBuf,
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
    pub runtimes: HashMap<PathBuf, Rc<RefCell<WidgetRuntime>>>,
    pub retired: Vec<Instance>,
    pending_renders: HashSet<PathBuf>,
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
            runtimes: HashMap::new(),
            retired: Vec::new(),
            pending_renders: HashSet::new(),
            placement,
            socket,
        }
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Sync => self.sync(),
            Msg::Click {
                key,
                code,
                index,
                button,
                x,
                y,
            } => self.run_click(&key, &code, index, button, x, y),
            Msg::MenuAction { key, code } => self.run(&key, &code, None),
            Msg::Change { key, code, value } => self.run(&key, &code, Some(value)),
            Msg::Text { key, code, value } => self.run_text(&key, &code, &value),
            Msg::Render { path } => {
                self.pending_renders.remove(&path);
                self.render_path(&path);
            }
            Msg::Timer { path, id } => self.fire_timer(&path, id),
            Msg::RegisterTimer {
                path,
                id,
                ms,
                once,
                scope,
            } => self.register_timer(&path, id, ms, once, scope),
            Msg::ClearTimer { path, id } => self.clear_timer(&path, id),
            Msg::ClearStream { path, id } => self.clear_stream(&path, id),
            Msg::StreamLine { path, id, line } => self.stream_line(&path, id, &line),
            Msg::StreamDone {
                path,
                id,
                ok,
                error,
            } => self.stream_done(&path, id, ok, &error),
            Msg::ExecDone { path, id, ok, text } => self.exec_done(&path, id, ok, &text),
            Msg::Placement(message) => self.placement(message),
            Msg::Quit => {
                // Drop the socket guard while the process still has a chance
                // to clean up; GLib does not drop local futures on shutdown.
                self.socket.take();
                for runtime in self.runtimes.values() {
                    runtime.borrow_mut().drain_streams();
                }
                self.app.quit();
            }
        }
    }

    pub fn sync(&mut self) {
        reconcile::sync(
            &mut self.widgets,
            &mut self.runtimes,
            &mut self.retired,
            self.target.as_deref(),
            &self.config_path,
            self.windowed,
            &self.app,
            &self.tx,
        );
    }

    fn run_click(
        &mut self,
        key: &WidgetKey,
        code: &str,
        index: Option<i64>,
        button: u32,
        x: f64,
        y: f64,
    ) {
        let data = self
            .widgets
            .get(key)
            .filter(|running| running.state == WidgetState::Active)
            .map(|running| {
                (
                    running.instance.runtime.clone(),
                    running.instance.window.clone(),
                    running
                        .instance
                        .comp
                        .placement
                        .connector
                        .clone()
                        .unwrap_or_default(),
                    running.instance.window.width(),
                    running.instance.window.height(),
                    running.instance.use_layer_shell,
                    running
                        .instance
                        .tree
                        .as_ref()
                        .map(|tree| tree.root.clone())
                        .unwrap_or_else(|| running.instance.window.clone().upcast()),
                )
            });
        let Some((runtime, window, connector, width, height, layer_shell, parent)) = data else {
            return;
        };

        if let Ok(mut runtime) = runtime.try_borrow_mut() {
            let _ = runtime.set_output_context(&connector, key.1, width, height);
            MENU_ANCHOR.with(|slot| {
                *slot.borrow_mut() = Some(MenuAnchor {
                    window: window.clone(),
                    parent,
                    key: key.clone(),
                    x,
                    y,
                    layer_shell,
                });
            });
            runtime.run_click(code, index, button);
            MENU_ANCHOR.with(|slot| *slot.borrow_mut() = None);
        }
        self.render_path(&key.0);
    }

    fn run(&mut self, key: &WidgetKey, code: &str, event: Option<f64>) {
        let Some(view) = self.view_context(key) else {
            return;
        };
        if let Ok(mut runtime) = view.runtime.try_borrow_mut() {
            let _ =
                runtime.set_output_context(&view.connector, view.index, view.width, view.height);
            runtime.run_code(code, event);
        }
        self.render_path(&key.0);
    }

    fn run_text(&mut self, key: &WidgetKey, code: &str, value: &str) {
        let Some(view) = self.view_context(key) else {
            return;
        };
        if let Ok(mut runtime) = view.runtime.try_borrow_mut() {
            let _ =
                runtime.set_output_context(&view.connector, view.index, view.width, view.height);
            runtime.run_text(code, value);
        }
        self.render_path(&key.0);
    }

    fn stream_line(&mut self, path: &Path, id: u64, line: &str) {
        let Some(runtime) = self.runtimes.get(path).cloned() else {
            return;
        };
        let fired = if let Ok(mut runtime) = runtime.try_borrow_mut() {
            runtime.fire_stream_line(id, line)
        } else {
            false
        };
        if fired {
            self.schedule_render(path);
        }
    }

    fn stream_done(&mut self, path: &Path, id: u64, ok: bool, error: &str) {
        let Some(runtime) = self.runtimes.get(path).cloned() else {
            return;
        };
        let fired = if let Ok(mut runtime) = runtime.try_borrow_mut() {
            runtime.finish_stream(id, ok, error)
        } else {
            false
        };
        if fired {
            self.pending_renders.remove(path);
            self.render_path(path);
        }
    }

    fn schedule_render(&mut self, path: &Path) {
        if !self.pending_renders.insert(path.to_path_buf()) {
            return;
        }

        let tx = self.tx.clone();
        let path = path.to_path_buf();
        gtk::glib::timeout_add_local_once(Duration::from_millis(33), move || {
            let _ = tx.send_blocking(Msg::Render { path });
        });
    }

    fn clear_stream(&mut self, path: &Path, id: u64) {
        if let Some(runtime) = self.runtimes.get(path)
            && let Ok(mut runtime) = runtime.try_borrow_mut()
        {
            runtime.clear_stream(id);
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

    fn view_context(&self, key: &WidgetKey) -> Option<ViewInfo> {
        let running = self
            .widgets
            .get(key)
            .filter(|running| running.state == WidgetState::Active)?;
        Some(ViewInfo {
            key: key.clone(),
            runtime: running.instance.runtime.clone(),
            connector: running
                .instance
                .comp
                .placement
                .connector
                .clone()
                .unwrap_or_default(),
            index: key.1,
            width: running.instance.window.width(),
            height: running.instance.window.height(),
        })
    }

    fn view_contexts(&self, path: &Path) -> Vec<ViewInfo> {
        self.widgets
            .iter()
            .filter(|(key, running)| key.0 == path && running.state == WidgetState::Active)
            .map(|(key, running)| ViewInfo {
                key: key.clone(),
                runtime: running.instance.runtime.clone(),
                connector: running
                    .instance
                    .comp
                    .placement
                    .connector
                    .clone()
                    .unwrap_or_default(),
                index: key.1,
                width: running.instance.window.width(),
                height: running.instance.window.height(),
            })
            .collect()
    }

    fn render_path(&mut self, path: &Path) {
        let keys: Vec<WidgetKey> = self
            .widgets
            .iter()
            .filter(|(key, running)| key.0 == path && running.state == WidgetState::Active)
            .map(|(key, _)| key.clone())
            .collect();

        for key in keys {
            if let Some(running) = self.active(&key) {
                running.instance.render_now();
            }
        }
    }

    fn fire_timer(&mut self, path: &Path, id: u64) {
        let Some(runtime) = self.runtimes.get(path).cloned() else {
            return;
        };
        let Some(scope) = runtime.borrow().timer_scope(id) else {
            return;
        };

        match scope {
            TimerScope::Global => {
                let once = {
                    let Ok(mut runtime) = runtime.try_borrow_mut() else {
                        return;
                    };
                    let _ = runtime.clear_output_context();
                    runtime.fire_timer(id)
                };
                if once && let Ok(mut runtime) = runtime.try_borrow_mut() {
                    runtime.clear_timer(id);
                }
                self.render_path(path);
            }
            TimerScope::Output => {
                let contexts = self.view_contexts(path);
                let mut once = false;

                for view in contexts {
                    if let Ok(mut runtime) = runtime.try_borrow_mut() {
                        let _ = runtime.set_output_context(
                            &view.connector,
                            view.index,
                            view.width,
                            view.height,
                        );
                        once = runtime.fire_timer(id);
                    }
                    if let Some(running) = self.active(&view.key) {
                        running.instance.render_now();
                    }
                }

                if once && let Ok(mut runtime) = runtime.try_borrow_mut() {
                    runtime.clear_timer(id);
                }
            }
        }
    }

    fn register_timer(&mut self, path: &Path, id: u64, ms: u64, once: bool, scope: TimerScope) {
        if let Some(runtime) = self.runtimes.get(path)
            && let Ok(mut runtime) = runtime.try_borrow_mut()
        {
            runtime.register_timer(id, ms, once, scope);
        }
    }

    fn clear_timer(&mut self, path: &Path, id: u64) {
        if let Some(runtime) = self.runtimes.get(path)
            && let Ok(mut runtime) = runtime.try_borrow_mut()
        {
            runtime.clear_timer(id);
        }
    }

    fn exec_done(&mut self, path: &Path, id: u64, ok: bool, text: &str) {
        let Some(runtime) = self.runtimes.get(path).cloned() else {
            return;
        };
        let fired = if let Ok(mut runtime) = runtime.try_borrow_mut() {
            runtime.exec_done(id, ok, text)
        } else {
            false
        };
        if fired {
            self.render_path(path);
        }
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

    fn live_widgets(&self) -> HashMap<WidgetKey, LiveWidget> {
        let display = gtk::gdk::Display::default();
        let mut map = HashMap::new();

        for running in self
            .widgets
            .values()
            .filter(|running| running.state == WidgetState::Active)
        {
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
                instance.key.clone(),
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
