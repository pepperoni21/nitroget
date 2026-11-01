//! Lua runtime, timers and exec callbacks for one widget.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use mlua::{Function, Lua, Table, Value};

use crate::instance::{Instance, TimerEntry};
use crate::state::Msg;

const CALLBACKS: &str = "nitroget.callbacks";

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

impl Instance {
    pub(super) fn register_lua(&mut self) -> Result<()> {
        let lua = &self.lua;
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
}
