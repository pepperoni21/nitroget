//! Lua runtime, timers and exec callbacks for one widget.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

use crate::component::Component;
use crate::state::{Msg, TimerScope};

use super::menu::{MENU_ANCHOR, MenuAnchor, close_menu, show_menu, update_menu};

const CALLBACKS: &str = "nitroget.callbacks";
const STREAM_CALLBACKS: &str = "nitroget.stream_callbacks";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn callback_table(lua: &Lua) -> mlua::Result<Table> {
    if let Ok(table) = lua.named_registry_value::<Table>(CALLBACKS) {
        return Ok(table);
    }

    let table = lua.create_table()?;
    lua.set_named_registry_value(CALLBACKS, table.clone())?;
    Ok(table)
}

fn store_callback(lua: &Lua, id: u64, func: Function) -> mlua::Result<()> {
    callback_table(lua)?.set(id as i64, func)?;
    Ok(())
}

fn get_callback(lua: &Lua, id: u64) -> Option<Function> {
    callback_table(lua)
        .ok()?
        .get::<Option<Function>>(id as i64)
        .ok()
        .flatten()
}

fn take_callback(lua: &Lua, id: u64) -> Option<Function> {
    let table = callback_table(lua).ok()?;
    let func = table.get::<Option<Function>>(id as i64).ok().flatten();
    let _ = table.set(id as i64, Value::Nil);
    func
}

fn remove_callback(lua: &Lua, id: u64) -> mlua::Result<()> {
    callback_table(lua)?.set(id as i64, Value::Nil)?;
    Ok(())
}

fn stream_callback_table(lua: &Lua) -> mlua::Result<Table> {
    if let Ok(table) = lua.named_registry_value::<Table>(STREAM_CALLBACKS) {
        return Ok(table);
    }

    let table = lua.create_table()?;
    lua.set_named_registry_value(STREAM_CALLBACKS, table.clone())?;
    Ok(table)
}

fn store_stream(lua: &Lua, id: u64, line: Function, done: Function) -> mlua::Result<()> {
    let entry = lua.create_table()?;
    entry.set("line", line)?;
    entry.set("done", done)?;
    stream_callback_table(lua)?.set(id as i64, entry)?;
    Ok(())
}

fn get_stream_line(lua: &Lua, id: u64) -> Option<Function> {
    let table = stream_callback_table(lua).ok()?;
    let entry = table.get::<Option<Table>>(id as i64).ok()??;
    entry.get::<Option<Function>>("line").ok()?
}

fn take_stream_done(lua: &Lua, id: u64) -> Option<Function> {
    let table = stream_callback_table(lua).ok()?;
    let entry = table.get::<Option<Table>>(id as i64).ok()??;
    let _ = table.set(id as i64, Value::Nil);
    entry.get::<Option<Function>>("done").ok()?
}

fn remove_stream_callbacks(lua: &Lua, id: u64) -> mlua::Result<()> {
    stream_callback_table(lua)?.set(id as i64, Value::Nil)?;
    Ok(())
}

fn spawn_stream(
    command: String,
    input: String,
    tx: async_channel::Sender<Msg>,
    path: PathBuf,
    id: u64,
) -> std::io::Result<Arc<Mutex<Child>>> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(&command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = process.spawn()?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let shared = Arc::new(Mutex::new(child));
    let worker = shared.clone();

    thread::spawn(move || {
        let writer = stdin.map(|mut stdin| {
            thread::spawn(move || {
                let _ = stdin.write_all(input.as_bytes());
            })
        });

        let stderr_thread = stderr.map(|stderr| {
            thread::spawn(move || {
                let mut text = String::new();
                let mut reader = BufReader::new(stderr);
                let _ = reader.read_to_string(&mut text);
                text
            })
        });

        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let _ = tx.send_blocking(Msg::StreamLine {
                    path: path.clone(),
                    id,
                    line,
                });
            }
        }

        let status = worker.lock().ok().and_then(|mut child| child.wait().ok());
        let error = stderr_thread
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        let _ = writer.and_then(|handle| handle.join().ok());
        let ok = status.is_some_and(|status| status.success());

        let _ = tx.send_blocking(Msg::StreamDone {
            path,
            id,
            ok,
            error: error.trim().to_string(),
        });
    });

    Ok(shared)
}

fn configure_package(lua: &Lua, dirs: &[PathBuf]) -> mlua::Result<()> {
    let package: Table = lua.globals().get("package")?;
    let existing: String = package.get("path").unwrap_or_default();
    let mut paths = Vec::new();

    for dir in dirs {
        let dir = dir.to_string_lossy();
        paths.push(format!("{dir}/?.lua"));
        paths.push(format!("{dir}/?/init.lua"));
    }
    if !existing.is_empty() {
        paths.push(existing);
    }

    package.set("path", paths.join(";"))?;
    Ok(())
}

pub struct TimerEntry {
    pub source: gtk::glib::SourceId,
    pub once: bool,
    pub fired: Rc<Cell<bool>>,
    pub scope: TimerScope,
}

impl TimerEntry {
    fn cancel(self) {
        if self.once && self.fired.get() {
            return;
        }
        self.source.remove();
    }
}

pub struct WidgetRuntime {
    pub path: PathBuf,
    pub lua: Lua,
    pub timers: HashMap<u64, TimerEntry>,
    pub streams: Rc<RefCell<HashMap<u64, Arc<Mutex<Child>>>>>,
    pub exec_contexts: Rc<RefCell<HashMap<u64, Option<String>>>>,
    pub stream_contexts: Rc<RefCell<HashMap<u64, Option<String>>>>,
    pub current_output: Rc<RefCell<Option<String>>>,
    pub outputs: Option<Table>,
    pub menu_anchor: Rc<RefCell<Option<MenuAnchor>>>,
    pub menu_anchors: Rc<RefCell<HashMap<String, MenuAnchor>>>,
    pub tx: async_channel::Sender<Msg>,
    pub error: Option<String>,
    pub mtime: SystemTime,
    pub paused: bool,
}

impl WidgetRuntime {
    pub fn new(
        comp: Component,
        path: PathBuf,
        mtime: SystemTime,
        tx: async_channel::Sender<Msg>,
    ) -> Result<Self> {
        let mut runtime = Self {
            path,
            lua: Lua::new(),
            timers: HashMap::new(),
            streams: Rc::new(RefCell::new(HashMap::new())),
            exec_contexts: Rc::new(RefCell::new(HashMap::new())),
            stream_contexts: Rc::new(RefCell::new(HashMap::new())),
            current_output: Rc::new(RefCell::new(None)),
            outputs: None,
            menu_anchor: Rc::new(RefCell::new(None)),
            menu_anchors: Rc::new(RefCell::new(HashMap::new())),
            tx,
            error: None,
            mtime,
            paused: false,
        };

        runtime.load(&comp)?;
        Ok(runtime)
    }

    pub fn reload(&mut self, comp: &Component, mtime: SystemTime) -> Result<()> {
        self.load(comp)?;
        self.mtime = mtime;
        Ok(())
    }

    pub fn pause(&mut self) {
        self.drain_timers();
        self.drain_streams();
        self.exec_contexts.borrow_mut().clear();
        self.stream_contexts.borrow_mut().clear();
        self.lua = Lua::new();
        self.outputs = None;
        self.error = None;
        *self.current_output.borrow_mut() = None;
        self.paused = true;
    }

    fn load(&mut self, comp: &Component) -> Result<()> {
        self.drain_timers();
        self.drain_streams();
        self.exec_contexts.borrow_mut().clear();
        self.stream_contexts.borrow_mut().clear();
        self.lua = Lua::new();
        self.outputs = None;
        self.error = None;
        *self.current_output.borrow_mut() = None;
        self.paused = false;

        self.register_lua(&comp.lua_dirs)?;

        if let Err(e) = self.lua.load(&comp.script).set_name("script").exec() {
            self.error = Some(format!("script error in {}: {e}", comp.path.display()));
            self.drain_streams();
        }

        Ok(())
    }

    fn register_lua(&mut self, dirs: &[PathBuf]) -> Result<()> {
        let lua = &self.lua;
        configure_package(lua, dirs)?;
        let nt = lua.create_table()?;
        let outputs = lua.create_table()?;
        nt.set("outputs", outputs.clone())?;
        self.outputs = Some(outputs);

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "refresh",
                lua.create_function(move |_, ()| {
                    let _ = tx.try_send(Msg::Render { path: path.clone() });
                    Ok(())
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "every",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        path: path.clone(),
                        id,
                        ms: ms.max(16),
                        once: false,
                        scope: TimerScope::Global,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "after",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        path: path.clone(),
                        id,
                        ms: ms.max(1),
                        once: true,
                        scope: TimerScope::Global,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "every_output",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        path: path.clone(),
                        id,
                        ms: ms.max(16),
                        once: false,
                        scope: TimerScope::Output,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "after_output",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        path: path.clone(),
                        id,
                        ms: ms.max(1),
                        once: true,
                        scope: TimerScope::Output,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            nt.set(
                "clear",
                lua.create_function(move |lua, id: u64| {
                    remove_callback(lua, id)?;
                    let _ = tx.try_send(Msg::ClearTimer {
                        path: path.clone(),
                        id,
                    });
                    let _ = tx.try_send(Msg::ClearStream {
                        path: path.clone(),
                        id,
                    });
                    Ok(())
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            let exec_contexts = self.exec_contexts.clone();
            let current_output = self.current_output.clone();
            nt.set(
                "exec",
                lua.create_function(move |lua, (cmd, func): (String, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let context = current_output.borrow().clone();
                    exec_contexts.borrow_mut().insert(id, context);

                    let tx = tx.clone();
                    let path = path.clone();
                    thread::spawn(move || {
                        let output = std::process::Command::new("sh")
                            .arg("-c")
                            .arg(&cmd)
                            .output();

                        let (ok, text) = match output {
                            Ok(o) => {
                                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                                if !o.stderr.is_empty() {
                                    if !s.is_empty() {
                                        s.push('\n');
                                    }
                                    s.push_str(&String::from_utf8_lossy(&o.stderr));
                                }
                                (o.status.success(), s)
                            }
                            Err(e) => (false, e.to_string()),
                        };

                        let _ = tx.send_blocking(Msg::ExecDone { path, id, ok, text });
                    });

                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let path = self.path.clone();
            let streams = self.streams.clone();
            let stream_contexts = self.stream_contexts.clone();
            let current_output = self.current_output.clone();
            nt.set(
                "stream",
                lua.create_function(
                    move |lua, (cmd, input, line, done): (String, String, Function, Function)| {
                        let id = next_id();
                        let context = current_output.borrow().clone();
                        stream_contexts.borrow_mut().insert(id, context);
                        let shared = match spawn_stream(cmd, input, tx.clone(), path.clone(), id) {
                            Ok(shared) => shared,
                            Err(e) => {
                                stream_contexts.borrow_mut().remove(&id);
                                return Err(mlua::Error::external(e));
                            }
                        };
                        streams.borrow_mut().insert(id, shared.clone());
                        if let Err(e) = store_stream(lua, id, line, done) {
                            streams.borrow_mut().remove(&id);
                            stream_contexts.borrow_mut().remove(&id);
                            if let Ok(mut child) = shared.lock() {
                                let _ = child.kill();
                            }
                            return Err(e);
                        }
                        Ok(id)
                    },
                )?,
            )?;
        }

        nt.set(
            "json_encode",
            lua.create_function(|lua, value: Value| {
                let json: serde_json::Value = lua
                    .from_value(value)
                    .map_err(|e| mlua::Error::runtime(format!("cannot encode JSON: {e}")))?;
                serde_json::to_string(&json)
                    .map_err(|e| mlua::Error::runtime(format!("cannot encode JSON: {e}")))
            })?,
        )?;

        nt.set(
            "json_decode",
            lua.create_function(|lua, text: String| {
                let json: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|e| mlua::Error::runtime(format!("cannot decode JSON: {e}")))?;
                lua.to_value(&json)
                    .map_err(|e| mlua::Error::runtime(format!("cannot decode JSON: {e}")))
            })?,
        )?;

        nt.set(
            "run",
            lua.create_function(|_, cmd: String| {
                thread::spawn(move || {
                    let _ = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .status();
                });
                Ok(())
            })?,
        )?;

        {
            let tx = self.tx.clone();
            let outputs = self.outputs.clone();
            let current_output = self.current_output.clone();
            let menu_anchor = self.menu_anchor.clone();
            let menu_anchors = self.menu_anchors.clone();
            nt.set(
                "menu",
                lua.create_function(move |lua, (items, options): (Table, Option<Table>)| {
                    let output_key = current_output.borrow().clone();
                    let anchor = MENU_ANCHOR
                        .with(|slot| slot.borrow().clone())
                        .or_else(|| {
                            output_key
                                .as_ref()
                                .and_then(|key| menu_anchors.borrow().get(key).cloned())
                        })
                        .or_else(|| menu_anchor.borrow().clone());
                    let Some(anchor) = anchor else {
                        return Err(mlua::Error::runtime(
                            "menu requires a rendered widget or click event",
                        ));
                    };

                    let output = current_output.borrow().as_ref().and_then(|key| {
                        outputs.as_ref().and_then(|table| {
                            table.get::<Option<Table>>(key.as_str()).ok().flatten()
                        })
                    });

                    show_menu(
                        items,
                        anchor,
                        options,
                        lua.clone(),
                        output,
                        current_output.clone(),
                        tx.clone(),
                    )
                })?,
            )?;
        }

        nt.set(
            "close_menu",
            lua.create_function(|_, ()| {
                close_menu();
                Ok(())
            })?,
        )?;

        nt.set(
            "update_menu",
            lua.create_function(|_, items: Table| update_menu(items))?,
        )?;

        nt.set(
            "log",
            lua.create_function(|_, msg: String| {
                println!("nitroget: {msg}");
                Ok(())
            })?,
        )?;

        {
            let tx = self.tx.clone();
            nt.set(
                "quit",
                lua.create_function(move |_, ()| {
                    let _ = tx.try_send(Msg::Quit);
                    Ok(())
                })?,
            )?;
        }

        nt.set("version", env!("CARGO_PKG_VERSION"))?;

        lua.globals().set("nitroget", nt)?;

        Ok(())
    }

    pub fn set_menu_anchor(&mut self, anchor: MenuAnchor) {
        let key = self.current_output.borrow().clone().unwrap_or_default();
        self.menu_anchors.borrow_mut().insert(key, anchor.clone());
        *self.menu_anchor.borrow_mut() = Some(anchor);
    }

    pub fn set_output_context(
        &mut self,
        connector: &str,
        index: Option<i32>,
        width: i32,
        height: i32,
    ) -> mlua::Result<()> {
        let Some(outputs) = &self.outputs else {
            return Ok(());
        };

        let key = if connector.is_empty() {
            format!("#{}", index.unwrap_or(-1))
        } else {
            connector.to_string()
        };

        let table = match outputs.get::<Option<Table>>(key.as_str())? {
            Some(table) => table,
            None => {
                let table = self.lua.create_table()?;
                outputs.set(key.as_str(), table.clone())?;
                table
            }
        };

        table.set("connector", connector)?;
        table.set("index", index)?;
        table.set("width", width)?;
        table.set("height", height)?;
        self.lua.globals().set("output", table)?;
        *self.current_output.borrow_mut() = Some(key);

        Ok(())
    }

    pub fn clear_output_context(&mut self) -> mlua::Result<()> {
        self.lua.globals().set("output", Value::Nil)?;
        *self.current_output.borrow_mut() = None;
        Ok(())
    }

    fn activate_output(&mut self, key: &str) -> mlua::Result<()> {
        let Some(outputs) = &self.outputs else {
            return Ok(());
        };

        if let Some(table) = outputs.get::<Option<Table>>(key)? {
            self.lua.globals().set("output", table)?;
            *self.current_output.borrow_mut() = Some(key.to_string());
        } else {
            self.clear_output_context()?;
        }

        Ok(())
    }

    pub fn run_click(&mut self, code: &str, index: Option<i64>, button: u32) {
        if let Ok(event) = self.lua.create_table() {
            let _ = event.set("index", index);
            let _ = event.set("button", button);
            let _ = self.lua.globals().set("event", event);
        }

        if let Err(e) = self.lua.load(code).set_name(code).exec() {
            eprintln!("nitroget: script error in `{code}`: {e}");
        }
    }

    pub fn run_code(&mut self, code: &str, event: Option<f64>) {
        if let Some(value) = event
            && let Ok(event) = self.lua.create_table()
        {
            let _ = event.set("value", value);
            let _ = self.lua.globals().set("event", event);
        }

        if let Err(e) = self.lua.load(code).set_name(code).exec() {
            eprintln!("nitroget: script error in `{code}`: {e}");
        }
    }

    pub fn run_text(&mut self, code: &str, value: &str) {
        if let Ok(event) = self.lua.create_table() {
            let _ = event.set("value", value);
            let _ = self.lua.globals().set("event", event);
        }

        if let Err(e) = self.lua.load(code).set_name(code).exec() {
            eprintln!("nitroget: script error in `{code}`: {e}");
        }
    }

    pub fn register_timer(&mut self, id: u64, ms: u64, once: bool, scope: TimerScope) {
        if self.error.is_some() {
            return;
        }
        if let Some(previous) = self.timers.remove(&id) {
            previous.cancel();
        }

        let tx = self.tx.clone();
        let path = self.path.clone();
        let millis = ms.max(1);
        let fired = Rc::new(Cell::new(false));

        let source = if once {
            let fired = fired.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(millis), move || {
                fired.set(true);
                let _ = tx.try_send(Msg::Timer {
                    path: path.clone(),
                    id,
                });
            })
        } else {
            gtk::glib::timeout_add_local(Duration::from_millis(millis), move || {
                let _ = tx.try_send(Msg::Timer {
                    path: path.clone(),
                    id,
                });
                gtk::glib::ControlFlow::Continue
            })
        };

        self.timers.insert(
            id,
            TimerEntry {
                source,
                once,
                fired,
                scope,
            },
        );
    }

    pub fn timer_scope(&self, id: u64) -> Option<TimerScope> {
        self.timers.get(&id).map(|entry| entry.scope)
    }

    pub fn clear_timer(&mut self, id: u64) {
        if let Some(entry) = self.timers.remove(&id) {
            entry.cancel();
        }
        if let Err(e) = remove_callback(&self.lua, id) {
            eprintln!("nitroget: cannot clear callback {id}: {e}");
        }
    }

    /// Runs a timer callback. Returns `true` when the timer is one-shot and
    /// should be removed by the owner.
    pub fn fire_timer(&mut self, id: u64) -> bool {
        let once = self
            .timers
            .get(&id)
            .map(|entry| entry.once)
            .unwrap_or(false);

        let output: Value = self.lua.globals().get("output").unwrap_or(Value::Nil);

        if let Some(func) = get_callback(&self.lua, id)
            && let Err(e) = func.call::<()>(output)
        {
            eprintln!("nitroget: timer error: {e}");
        }

        once
    }

    pub fn exec_done(&mut self, id: u64, ok: bool, text: &str) -> bool {
        let context = self.exec_contexts.borrow_mut().remove(&id).flatten();
        let Some(func) = take_callback(&self.lua, id) else {
            return false;
        };

        if let Some(key) = context {
            let _ = self.activate_output(&key);
        } else {
            let _ = self.clear_output_context();
        }

        if let Err(e) = func.call::<()>((ok, text.to_string())) {
            eprintln!("nitroget: exec callback error: {e}");
        }

        true
    }

    pub fn fire_stream_line(&mut self, id: u64, line: &str) -> bool {
        let Some(func) = get_stream_line(&self.lua, id) else {
            return false;
        };
        let context = self.stream_contexts.borrow().get(&id).cloned().flatten();

        if let Some(key) = context {
            let _ = self.activate_output(&key);
        } else {
            let _ = self.clear_output_context();
        }

        if let Err(e) = func.call::<()>(line.to_string()) {
            eprintln!("nitroget: stream error: {e}");
        }

        true
    }

    pub fn finish_stream(&mut self, id: u64, ok: bool, error: &str) -> bool {
        self.streams.borrow_mut().remove(&id);
        let context = self.stream_contexts.borrow_mut().remove(&id).flatten();
        let Some(func) = take_stream_done(&self.lua, id) else {
            return false;
        };

        if let Some(key) = context {
            let _ = self.activate_output(&key);
        } else {
            let _ = self.clear_output_context();
        }

        if let Err(e) = func.call::<()>((ok, error.to_string())) {
            eprintln!("nitroget: stream callback error: {e}");
        }

        true
    }

    pub fn clear_stream(&mut self, id: u64) {
        if let Some(child) = self.streams.borrow_mut().remove(&id)
            && let Ok(mut child) = child.lock()
        {
            let _ = child.kill();
        }
        self.stream_contexts.borrow_mut().remove(&id);
        if let Err(e) = remove_stream_callbacks(&self.lua, id) {
            eprintln!("nitroget: cannot clear stream {id}: {e}");
        }
    }

    pub fn drain_timers(&mut self) {
        for (_, entry) in self.timers.drain() {
            entry.cancel();
        }
    }

    pub fn drain_streams(&mut self) {
        for (_, child) in self.streams.borrow_mut().drain() {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
        self.stream_contexts.borrow_mut().clear();
    }
}

impl Drop for WidgetRuntime {
    fn drop(&mut self) {
        self.drain_timers();
        self.drain_streams();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn require_loads_nested_modules() {
        let dir = std::env::temp_dir().join(format!("nitroget-require-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("foo.lua"), "return \"ok\"").unwrap();

        let lua = Lua::new();
        configure_package(&lua, std::slice::from_ref(&dir)).unwrap();
        let value: String = lua.load("return require(\"lib.foo\")").eval().unwrap();

        assert_eq!(value, "ok");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn json_bridge_round_trips_tables() {
        let lua = Lua::new();
        let value: Value = lua
            .load(
                "return { model = \"test\", stream = true, messages = { { role = \"user\", content = \"hi\" } } }",
            )
            .eval()
            .unwrap();

        let json: serde_json::Value = lua.from_value(value).unwrap();
        assert_eq!(json["model"], "test");
        assert_eq!(json["messages"][0]["content"], "hi");

        let text = serde_json::to_string(&json).unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&text).unwrap();
        lua.globals()
            .set("decoded", lua.to_value(&decoded).unwrap())
            .unwrap();

        let model: String = lua.load("return decoded.model").eval().unwrap();
        let content: String = lua
            .load("return decoded.messages[1].content")
            .eval()
            .unwrap();
        assert_eq!(model, "test");
        assert_eq!(content, "hi");
    }

    #[test]
    fn stream_delivers_lines_and_completion() {
        let (tx, rx) = async_channel::unbounded();
        let path = PathBuf::from("stream.nitroget");
        let _child = spawn_stream(
            "printf 'one\\ntwo\\n'".to_string(),
            String::new(),
            tx,
            path.clone(),
            7,
        )
        .unwrap();

        let mut lines = Vec::new();
        loop {
            match rx.recv_blocking().unwrap() {
                Msg::StreamLine {
                    path: line_path,
                    id,
                    line,
                } => {
                    assert_eq!(line_path, path);
                    assert_eq!(id, 7);
                    lines.push(line);
                }
                Msg::StreamDone {
                    path: done_path,
                    id,
                    ok,
                    error,
                } => {
                    assert_eq!(done_path, path);
                    assert_eq!(id, 7);
                    assert!(ok, "{error}");
                    break;
                }
                _ => {}
            }
        }

        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn stream_forwards_stdin() {
        let (tx, rx) = async_channel::unbounded();
        let path = PathBuf::from("stream.nitroget");
        let _child =
            spawn_stream("cat".to_string(), "one\ntwo\n".to_string(), tx, path, 8).unwrap();

        let mut lines = Vec::new();
        loop {
            match rx.recv_blocking().unwrap() {
                Msg::StreamLine { line, .. } => lines.push(line),
                Msg::StreamDone { ok, .. } => {
                    assert!(ok);
                    break;
                }
                _ => {}
            }
        }

        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn output_context_is_shared_and_scoped() {
        let comp = crate::component::parse(
            "<widget name=\"w\"><label>x</label></widget><script>answer = 42</script>",
            Path::new("w.nitroget"),
        )
        .unwrap();
        let (tx, _rx) = async_channel::unbounded();
        let mut runtime =
            WidgetRuntime::new(comp, PathBuf::from("w.nitroget"), SystemTime::now(), tx).unwrap();

        runtime
            .set_output_context("eDP-1", Some(0), 1920, 1080)
            .unwrap();
        let connector: String = runtime.lua.load("return output.connector").eval().unwrap();
        let width: i32 = runtime
            .lua
            .load("return nitroget.outputs['eDP-1'].width")
            .eval()
            .unwrap();
        let shared: i32 = runtime.lua.load("return answer").eval().unwrap();

        assert_eq!(connector, "eDP-1");
        assert_eq!(width, 1920);
        assert_eq!(shared, 42);

        runtime.clear_output_context().unwrap();
        let cleared: bool = runtime.lua.load("return output == nil").eval().unwrap();
        assert!(cleared);
    }
}
