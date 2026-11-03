//! Command implementations: run, check, new, placement toggle and quit.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gtk::prelude::*;

use crate::cli::Cli;
use crate::component;
use crate::config;
use crate::discovery::collect_paths;
use crate::instance;
use crate::ipc;
use crate::state::{AppState, Msg, PlacementMsg};
use crate::watcher;

pub fn cmd_run(cli: &Cli) -> Result<()> {
    let target = cli.explicit_target();
    let config_path = cli.config_path();
    let socket_path = cli.socket_path();
    let folder = cli.canonical_search_dir();

    // There is one daemon. A second launch either activates the same folder or
    // explains which folder is already running; it never draws a duplicate set.
    if let Some(running) = running_folder(&socket_path) {
        if running == folder {
            if cli.start_placement {
                if ipc::send(&socket_path, "placement on").is_ok() {
                    println!("placement mode enabled");
                    return Ok(());
                }
            } else {
                println!("nitroget is already running for {}", folder.display());
                return Ok(());
            }
        } else {
            bail!(
                "nitroget is already running for {}; run `nitroget quit` first",
                running.display()
            );
        }
    }

    let paths = collect_paths(target.as_deref())?;

    if paths.is_empty() {
        eprintln!(
            "nitroget: no .nitroget widgets found in {}",
            cli.search_dir().display()
        );
        eprintln!("create a starter widget with: nitroget new clock");
        return Ok(());
    }

    // Bind the control socket before GTK starts.
    let (command_tx, command_rx) = async_channel::unbounded::<String>();
    let socket = ipc::serve(
        socket_path,
        folder.to_string_lossy().into_owned(),
        command_tx,
    )?;

    let app = gtk::Application::builder()
        .application_id(cli.app_id())
        .build();

    let windowed = cli.windowed;
    let start_placement = cli.start_placement;

    // A second `nitroget` on the same directory activates this instance; only
    // the first activation should build the widgets.
    let started = Cell::new(false);
    let socket = RefCell::new(Some(socket));
    let command_rx = RefCell::new(Some(command_rx));
    app.connect_activate(move |app| {
        if started.replace(true) {
            return;
        }

        // These are only available to the first activation.
        let (Some(socket), Some(command_rx)) =
            (socket.borrow_mut().take(), command_rx.borrow_mut().take())
        else {
            return;
        };

        install_base_css();

        let (tx, rx) = async_channel::unbounded::<Msg>();

        let mut state = AppState::new(
            target.clone(),
            config_path.clone(),
            windowed,
            app.clone(),
            tx.clone(),
            Some(socket),
        );

        state.sync();
        watcher::install(target.clone(), config_path.clone(), tx.clone());

        {
            let tx = tx.clone();
            gtk::glib::spawn_future_local(async move {
                while let Ok(command) = command_rx.recv().await {
                    let message = match command.as_str() {
                        // Toggle from `nitroget placement`; explicit start from
                        // `--placement` when the daemon already exists. Save
                        // and cancel are UI-only.
                        "placement" => Msg::Placement(PlacementMsg::Toggle),
                        "placement on" => Msg::Placement(PlacementMsg::Start),
                        "quit" => Msg::Quit,
                        other => {
                            eprintln!("nitroget: unknown command `{other}`");
                            continue;
                        }
                    };
                    let _ = tx.send(message).await;
                }
            });
        }

        if start_placement {
            // Let the widget surfaces map first: their monitor is only known
            // after the compositor has placed them.
            let tx = tx.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let _ = tx.try_send(Msg::Placement(PlacementMsg::Start));
            });
        }

        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = rx.recv().await {
                state.handle(message);
            }
        });
    });

    // Keep running as a daemon even while a widget is being reloaded (and the
    // last window temporarily disappears). `nitroget.quit()` still exits.
    let _hold = app.hold();
    app.run_with_args::<&str>(&[]);
    Ok(())
}

/// Folder reported by the running daemon, if there is one.
fn running_folder(socket: &Path) -> Option<PathBuf> {
    let reply = ipc::send(socket, "where").ok()?;
    let folder = PathBuf::from(reply);
    Some(std::fs::canonicalize(&folder).unwrap_or(folder))
}

pub fn cmd_check(cli: &Cli) -> Result<()> {
    let paths = collect_paths(cli.explicit_target().as_deref())?;
    if paths.is_empty() {
        bail!(
            "no .nitroget widgets found in {}",
            cli.search_dir().display()
        );
    }

    let config = config::load(&cli.config_path())?;

    let mut failures = 0;
    let mut enabled = Vec::new();
    let mut disabled = Vec::new();

    for path in &paths {
        match component::load(path) {
            Ok(mut comp) => {
                if config.apply(&mut comp) {
                    enabled.push(comp);
                } else {
                    disabled.push(path.clone());
                }
            }
            Err(e) => {
                eprintln!("fail  {}: {e:#}", path.display());
                failures += 1;
            }
        }
    }

    config::auto_place(&mut enabled, &config);

    for comp in &enabled {
        let output = match (
            comp.placement.output.as_deref(),
            comp.placement.output_index,
        ) {
            (Some("all"), _) => "all (one per monitor)".to_string(),
            (Some(output), _) => output.to_string(),
            (None, Some(index)) => format!("monitor #{index}"),
            (None, None) => "auto".to_string(),
        };

        if let component::ComponentKind::Panel(panel) = &comp.kind {
            let edge = match panel.edge {
                component::PanelEdge::Top => "top",
                component::PanelEdge::Bottom => "bottom",
                component::PanelEdge::Left => "left",
                component::PanelEdge::Right => "right",
            };
            println!(
                "ok    {}  [{}] panel edge={} thickness={} offset={} exclusive_zone={} output={} layer={}",
                comp.path.display(),
                comp.name,
                edge,
                panel.thickness,
                panel.offset,
                panel.exclusive_zone(),
                output,
                comp.placement.layer
            );
        } else {
            let (offset_x, offset_y) = comp.placement.offset();
            println!(
                "ok    {}  [{}] {}x{} anchor={} offset={},{} output={} layer={}",
                comp.path.display(),
                comp.name,
                comp.placement
                    .width
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "auto".into()),
                comp.placement
                    .height
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "auto".into()),
                comp.placement.anchor(),
                offset_x,
                offset_y,
                output,
                comp.placement.layer
            );
        }

        let lua = mlua::Lua::new();
        if let Err(e) = lua.load(&comp.script).set_name("script").into_function() {
            eprintln!("      script: {e}");
            failures += 1;
        }

        let mut notes = comp.notes.clone();
        notes.extend(lint(comp));
        for note in notes {
            println!("      note: {note}");
        }
    }

    for path in &disabled {
        println!(
            "skip  {}  (disabled in {})",
            path.display(),
            config::FILE_NAME
        );
    }

    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn lint(comp: &component::Component) -> Vec<String> {
    let mut out = Vec::new();
    if comp.css.trim().is_empty() {
        out.push("no <style> block".into());
    }
    if comp.script.trim().is_empty() {
        out.push("no <script> block".into());
    }
    if let Some(height) = comp.placement.height
        && height < 40
    {
        out.push("height looks too small".into());
    }
    out
}

pub fn cmd_new(cli: &Cli, name: &str) -> Result<()> {
    let dir = cli.new_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let sanitized = instance::sanitize(name);
    let path = dir.join(format!("{sanitized}.nitroget"));
    if path.exists() {
        bail!("{} already exists", path.display());
    }

    let template = builtin_template(name).unwrap_or(EXAMPLE_CLOCK);
    let content = rename_widget(template, &sanitized);

    std::fs::write(&path, content).with_context(|| format!("cannot write {}", path.display()))?;

    println!("{}", path.display());
    Ok(())
}

pub fn cmd_placement(cli: &Cli) -> Result<()> {
    let socket = cli.socket_path();
    let Some(folder) = running_folder(&socket) else {
        bail!("nitroget is not running");
    };

    // `nitroget placement` targets the running instance. If the user passed
    // `-w`, make sure that is actually the folder in use instead of silently
    // toggling a different set.
    if cli.explicit_target().is_some() {
        let want = cli.canonical_search_dir();
        if folder != want {
            bail!(
                "nitroget is running for {}, not {} (run `nitroget quit` first)",
                folder.display(),
                want.display()
            );
        }
    }

    ipc::send(&socket, "placement").context("nitroget stopped while sending the command")?;
    println!("placement mode toggled for {}", folder.display());
    Ok(())
}

pub fn cmd_quit(cli: &Cli) -> Result<()> {
    let socket = cli.socket_path();
    let Some(folder) = running_folder(&socket) else {
        bail!("nitroget is not running");
    };

    if cli.explicit_target().is_some() && folder != cli.canonical_search_dir() {
        bail!(
            "nitroget is running for {}, not {}",
            folder.display(),
            cli.canonical_search_dir().display()
        );
    }

    ipc::send(&socket, "quit").context("could not stop nitroget")?;
    println!("nitroget stopped ({})", folder.display());
    Ok(())
}

/// Rules that must survive even when a widget's own CSS provider is removed
/// (parked or retired widgets must stay invisible, not show the theme's window
/// background).
fn install_base_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };

    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        "window.nitroget { background-color: transparent; }\n\
         window.nitroget.nitroget-parked { background-color: rgba(0, 0, 0, 0.004); }\n\
         window.nitroget > box.nitroget-root { background-color: transparent; }",
    );

    #[allow(deprecated)]
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER,
    );
}

/// Built-in starter widgets, compiled into the binary.
fn builtin_template(name: &str) -> Option<&'static str> {
    match name {
        "clock" => Some(include_str!("../examples/clock.nitroget")),
        "media" => Some(include_str!("../examples/media.nitroget")),
        "sysmon" => Some(include_str!("../examples/sysmon.nitroget")),
        _ => None,
    }
}

/// Replaces `name="..."` on the root element so the scaffold matches the file.
fn rename_widget(source: &str, name: &str) -> String {
    let Some(start) = source.find("name=\"") else {
        return source.to_string();
    };
    let value_start = start + "name=\"".len();
    let Some(len) = source[value_start..].find('"') else {
        return source.to_string();
    };

    format!(
        "{}{}{}",
        &source[..value_start],
        name,
        &source[value_start + len..]
    )
}

const EXAMPLE_CLOCK: &str = include_str!("../examples/clock.nitroget");
