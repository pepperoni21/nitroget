//! nitroget — a Wayland desktop widget engine.
//!
//! Widgets are single `.nitroget` files made of a template, a stylesheet and a
//! Lua script. They are drawn on the layer-shell `bottom` layer: above the
//! wallpaper, below normal windows.
//!
//! Placement and monitor selection live in `config.toml` inside the widget
//! folder. Without it, widgets are laid out automatically.

mod app;
mod cli;
mod clip;
mod component;
mod config;
mod css;
mod discovery;
mod instance;
mod ipc;
mod layout;
mod markdown;
mod placement;
mod reconcile;
mod render;
mod state;
mod template;
mod watcher;

use anyhow::Result;

use crate::cli::{Cli, Command};

fn main() {
    if let Err(e) = run_cli(std::env::args().skip(1).collect()) {
        eprintln!("nitroget: {e:#}");
        std::process::exit(1);
    }
}

fn run_cli(args: Vec<String>) -> Result<()> {
    let cli = Cli::parse(args)?;

    match &cli.command {
        Command::Run => app::cmd_run(&cli),
        Command::Check => app::cmd_check(&cli),
        Command::New(name) => app::cmd_new(&cli, name),
        Command::Placement => app::cmd_placement(&cli),
        Command::Quit => app::cmd_quit(&cli),
        Command::Help => {
            crate::cli::print_help();
            Ok(())
        }
        Command::Version => {
            println!("nitroget {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}
