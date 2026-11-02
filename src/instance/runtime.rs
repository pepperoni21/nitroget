//! Lua runtime, timers and exec callbacks for one widget.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

use crate::instance::{Instance, TimerEntry};
use crate::state::{Msg, WidgetKey};

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

pub(super) fn remove_stream_callbacks(lua: &Lua, id: u64) -> mlua::Result<()> {
    stream_callback_table(lua)?.set(id as i64, Value::Nil)?;
    Ok(())
}

fn spawn_stream(
    command: String,
    input: String,
    tx: async_channel::Sender<Msg>,
    key: WidgetKey,
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
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let shared = Arc::new(Mutex::new(child));
    let worker = shared.clone();

    thread::spawn(move || {
        let stderr_thread = stderr.map(|stderr| {
            thread::spawn(move || {
                let mut text = String::new();
                let mut reader = BufReader::new(stderr);
                let _ = reader.read_to_string(&mut text);
                text
            })
        });

        if let Some(stdin) = stdin.as_mut() {
            let _ = stdin.write_all(input.as_bytes());
        }
        drop(stdin);

        if let Some(stdout) = stdout {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let _ = tx.send_blocking(Msg::StreamLine {
                    key: key.clone(),
                    id,
                    line,
                });
            }
        }

        let status = worker.lock().ok().and_then(|mut child| child.wait().ok());
        let error = stderr_thread
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        let ok = status.is_some_and(|status| status.success());

        let _ = tx.send_blocking(Msg::StreamDone {
            key,
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

impl Instance {
    pub(super) fn register_lua(&mut self) -> Result<()> {
        let lua = &self.lua;
        configure_package(lua, &self.comp.lua_dirs)?;
        let nt = lua.create_table()?;

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            nt.set(
                "refresh",
                lua.create_function(move |_, ()| {
                    let _ = tx.try_send(Msg::Render { key: key.clone() });
                    Ok(())
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            nt.set(
                "every",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        key: key.clone(),
                        id,
                        ms: ms.max(16),
                        once: false,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            nt.set(
                "after",
                lua.create_function(move |lua, (ms, func): (u64, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;
                    let _ = tx.try_send(Msg::RegisterTimer {
                        key: key.clone(),
                        id,
                        ms: ms.max(1),
                        once: true,
                    });
                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            nt.set(
                "clear",
                lua.create_function(move |lua, id: u64| {
                    remove_callback(lua, id)?;
                    let _ = tx.try_send(Msg::ClearTimer {
                        key: key.clone(),
                        id,
                    });
                    let _ = tx.try_send(Msg::ClearStream {
                        key: key.clone(),
                        id,
                    });
                    Ok(())
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            nt.set(
                "exec",
                lua.create_function(move |lua, (cmd, func): (String, Function)| {
                    let id = next_id();
                    store_callback(lua, id, func)?;

                    let tx = tx.clone();
                    let key = key.clone();
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

                        let _ = tx.send_blocking(Msg::ExecDone { key, id, ok, text });
                    });

                    Ok(id)
                })?,
            )?;
        }

        {
            let tx = self.tx.clone();
            let key = self.key.clone();
            let streams = self.streams.clone();
            nt.set(
                "stream",
                lua.create_function(
                    move |lua, (cmd, input, line, done): (String, String, Function, Function)| {
                        let id = next_id();
                        let shared = spawn_stream(cmd, input, tx.clone(), key.clone(), id)
                            .map_err(mlua::Error::external)?;
                        streams.borrow_mut().insert(id, shared);
                        store_stream(lua, id, line, done)?;
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

    pub fn register_timer(&mut self, id: u64, ms: u64, once: bool) {
        if let Some(previous) = self.timers.remove(&id) {
            previous.cancel();
        }

        let tx = self.tx.clone();
        let key = self.key.clone();
        let millis = ms.max(1);
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));

        let source = if once {
            let fired = fired.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(millis), move || {
                fired.set(true);
                let _ = tx.try_send(Msg::Timer { key, id });
            })
        } else {
            gtk::glib::timeout_add_local(Duration::from_millis(millis), move || {
                let _ = tx.try_send(Msg::Timer {
                    key: key.clone(),
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
            },
        );
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

        if let Some(func) = get_callback(&self.lua, id)
            && let Err(e) = func.call::<()>(())
        {
            eprintln!("nitroget: timer error: {e}");
        }

        once
    }

    pub fn exec_done(&mut self, id: u64, ok: bool, text: &str) {
        if let Some(func) = take_callback(&self.lua, id)
            && let Err(e) = func.call::<()>((ok, text.to_string()))
        {
            eprintln!("nitroget: exec callback error: {e}");
        }
    }

    pub fn fire_stream_line(&mut self, id: u64, line: &str) {
        if let Some(func) = get_stream_line(&self.lua, id)
            && let Err(e) = func.call::<()>(line.to_string())
        {
            eprintln!("nitroget: stream error: {e}");
        }
    }

    pub fn finish_stream(&mut self, id: u64, ok: bool, error: &str) {
        self.streams.borrow_mut().remove(&id);
        if let Some(func) = take_stream_done(&self.lua, id)
            && let Err(e) = func.call::<()>((ok, error.to_string()))
        {
            eprintln!("nitroget: stream callback error: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let key = (PathBuf::from("stream.nitroget"), None);
        let _child = spawn_stream(
            "printf 'one\\ntwo\\n'".to_string(),
            String::new(),
            tx,
            key,
            7,
        )
        .unwrap();

        let mut lines = Vec::new();
        loop {
            match rx.recv_blocking().unwrap() {
                Msg::StreamLine { id, line, .. } => {
                    assert_eq!(id, 7);
                    lines.push(line);
                }
                Msg::StreamDone { id, ok, error, .. } => {
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
        let key = (PathBuf::from("stream.nitroget"), None);
        let _child = spawn_stream("cat".to_string(), "one\ntwo\n".to_string(), tx, key, 8).unwrap();

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
}
