//! Command-line parsing and help.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::discovery::{config_path_for, default_dir, expand_tilde, short_hash};
use crate::ipc;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    Check,
    New(String),
    Placement,
    Quit,
    Help,
    Version,
}

#[derive(Debug, Clone)]
pub struct Cli {
    pub command: Command,
    /// Path passed on the command line (file or directory).
    pub target: Option<PathBuf>,
    /// `-w` / `--widgets` (`-d` / `--dir` is an alias).
    pub dir: Option<PathBuf>,
    pub windowed: bool,
    /// Start and immediately enter placement mode.
    pub start_placement: bool,
}

impl Cli {
    pub fn parse(args: Vec<String>) -> Result<Self> {
        let mut positional = Vec::new();
        let mut dir = None;
        let mut windowed = false;
        let mut start_placement = false;
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--windowed" => windowed = true,
                "--placement" => start_placement = true,
                "--" => {
                    positional.extend(iter.by_ref());
                    break;
                }
                "-w" | "--widgets" | "-d" | "--dir" => {
                    let value = iter.next().context("--widgets needs a directory")?;
                    dir = Some(expand_tilde(&value));
                }
                _ if arg.starts_with("--widgets=") => {
                    dir = Some(expand_tilde(&arg["--widgets=".len()..]));
                }
                _ if arg.starts_with("--dir=") => {
                    dir = Some(expand_tilde(&arg["--dir=".len()..]));
                }
                _ if arg.starts_with("-w=") => {
                    dir = Some(expand_tilde(&arg["-w=".len()..]));
                }
                _ if arg.starts_with("-d=") => {
                    dir = Some(expand_tilde(&arg["-d=".len()..]));
                }
                "-h" | "--help" | "-V" | "--version" => positional.push(arg),
                _ if arg.starts_with('-') && arg.len() > 1 => {
                    bail!("unknown option `{arg}` (try `nitroget --help`)");
                }
                _ => positional.push(arg),
            }
        }

        let first = positional.first().map(String::as_str);

        let (command, rest) = match first {
            None => (Command::Run, &positional[..0]),
            Some("check") => (Command::Check, &positional[1..]),
            Some("new") => {
                let name = positional
                    .get(1)
                    .cloned()
                    .context("usage: nitroget new <name>")?;
                if positional.len() > 2 {
                    bail!("unexpected argument: {}", positional[2]);
                }
                (Command::New(name), &positional[..0])
            }
            Some("help" | "-h" | "--help") => (Command::Help, &positional[..0]),
            Some("version" | "-V" | "--version") => (Command::Version, &positional[..0]),
            Some("placement") => {
                if positional.len() > 1 {
                    bail!(
                        "`placement` takes no arguments; use Cancel / Done (or Esc / Enter) in the UI"
                    );
                }
                (Command::Placement, &positional[..0])
            }
            Some("quit" | "stop") => {
                if positional.len() > 1 {
                    bail!("`quit` takes no arguments");
                }
                (Command::Quit, &positional[..0])
            }
            Some("run") => bail!("`run` is no longer a command; use `nitroget [PATH]`"),
            // `nitroget ./some/dir` is the normal way to pick a widget folder.
            Some(path) => {
                if positional.len() > 1 {
                    bail!("unexpected argument: {}", positional[1]);
                }
                return Ok(Cli {
                    command: Command::Run,
                    target: Some(expand_tilde(path)),
                    dir,
                    windowed,
                    start_placement,
                });
            }
        };

        let target = match rest {
            [] => None,
            [path] => Some(expand_tilde(path)),
            [_, extra, ..] => bail!("unexpected argument: {extra}"),
        };

        Ok(Cli {
            command,
            target,
            dir,
            windowed,
            start_placement,
        })
    }

    /// Where to look for widgets: an explicit path wins over `--widgets`.
    pub fn explicit_target(&self) -> Option<PathBuf> {
        self.target.clone().or_else(|| self.dir.clone())
    }

    pub fn search_dir(&self) -> PathBuf {
        self.explicit_target().unwrap_or_else(default_dir)
    }

    pub fn new_dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(default_dir)
    }

    /// Canonical form of `search_dir`, used to compare this process with the
    /// folder reported by a running daemon.
    pub fn canonical_search_dir(&self) -> PathBuf {
        let dir = self.search_dir();
        std::fs::canonicalize(&dir).unwrap_or(dir)
    }

    /// One GTK application id per widget directory. The control socket is
    /// global, so this only affects GTK's own activation handling.
    pub fn dir_hash(&self) -> String {
        short_hash(&self.canonical_search_dir().to_string_lossy())
    }

    pub fn app_id(&self) -> String {
        format!("dev.nitroget.Nitroget.n{}", self.dir_hash())
    }

    pub fn socket_path(&self) -> PathBuf {
        ipc::socket_path()
    }

    /// Placement/monitor config for the selected widget set.
    pub fn config_path(&self) -> PathBuf {
        config_path_for(&self.search_dir())
    }
}

pub fn print_help() {
    println!(
        "nitroget {}

Usage:
  nitroget                     run every widget in the default directory
  nitroget [PATH]              run widgets from a file or directory
  nitroget check [PATH]        parse widgets, Lua and config, then exit
  nitroget placement           toggle mouse placement mode
  nitroget quit                stop the running instance
  nitroget new <name>          scaffold a widget (clock, media, sysmon, counter)
  nitroget help
  nitroget --version

Options:
  -w, --widgets <PATH>         widget directory to load (-d/--dir is an alias)
      --windowed               use normal windows instead of layer-shell
      --placement               start in placement mode

Default widget directory: {}
Placement/monitor config: {} (inside the widget folder)
Override the widget directory with $NITROGET_DIR.",
        env!("CARGO_PKG_VERSION"),
        default_dir().display(),
        config_path_for(&default_dir()).display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli> {
        Cli::parse(args.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn no_arguments_runs_the_default_directory() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.command, Command::Run);
        assert!(cli.target.is_none());
        assert!(cli.dir.is_none());
        assert!(!cli.windowed);
    }

    #[test]
    fn bare_path_is_an_implicit_run() {
        let cli = parse(&["/tmp/widgets"]).unwrap();
        assert_eq!(cli.command, Command::Run);
        assert_eq!(cli.target, Some(PathBuf::from("/tmp/widgets")));
    }

    #[test]
    fn check_takes_a_path() {
        let cli = parse(&["check", "widgets"]).unwrap();
        assert_eq!(cli.command, Command::Check);
        assert_eq!(cli.target, Some(PathBuf::from("widgets")));
    }

    #[test]
    fn run_subcommand_is_gone() {
        assert!(parse(&["run", "widgets"]).is_err());
    }

    #[test]
    fn dir_flag_and_equals_form() {
        let cli = parse(&["--widgets", "/tmp/a"]).unwrap();
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/a")));

        let cli = parse(&["--widgets=/tmp/b"]).unwrap();
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/b")));

        let cli = parse(&["-w", "/tmp/c"]).unwrap();
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/c")));

        // `--dir` and `-d` stay as aliases.
        let cli = parse(&["--dir", "/tmp/d"]).unwrap();
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/d")));

        let cli = parse(&["-d=/tmp/e"]).unwrap();
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/e")));
    }

    #[test]
    fn widgets_flag_works_with_check() {
        let cli = parse(&["check", "--widgets", "/tmp/a"]).unwrap();
        assert_eq!(cli.command, Command::Check);
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/a")));

        let cli = parse(&["--widgets=/tmp/a", "check"]).unwrap();
        assert_eq!(cli.command, Command::Check);
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/a")));
    }

    #[test]
    fn unknown_options_are_reported() {
        let error = parse(&["--nope"]).unwrap_err().to_string();
        assert!(error.contains("unknown option"));
    }

    #[test]
    fn placement_command_takes_no_arguments() {
        assert_eq!(parse(&["placement"]).unwrap().command, Command::Placement);
        assert_eq!(
            parse(&["--widgets", "/tmp/a", "placement"])
                .unwrap()
                .command,
            Command::Placement
        );
        assert!(parse(&["placement", "on"]).is_err());
        assert!(parse(&["placement", "done"]).is_err());
    }

    #[test]
    fn quit_command_takes_no_arguments() {
        assert_eq!(parse(&["quit"]).unwrap().command, Command::Quit);
        assert_eq!(parse(&["stop"]).unwrap().command, Command::Quit);
        assert!(parse(&["quit", "now"]).is_err());

        let cli = parse(&["-w", "/tmp/a", "quit"]).unwrap();
        assert_eq!(cli.command, Command::Quit);
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/a")));
    }

    #[test]
    fn path_beats_dir_flag() {
        let cli = parse(&["--dir", "/tmp/a", "/tmp/b"]).unwrap();
        assert_eq!(cli.search_dir(), PathBuf::from("/tmp/b"));
    }

    #[test]
    fn config_lives_in_the_selected_widget_folder() {
        let cli = parse(&["/tmp/some-widgets"]).unwrap();
        assert_eq!(
            cli.config_path(),
            PathBuf::from("/tmp/some-widgets/config.toml")
        );

        let cli = parse(&["--widgets", "/tmp/other-widgets"]).unwrap();
        assert_eq!(
            cli.config_path(),
            PathBuf::from("/tmp/other-widgets/config.toml")
        );
    }

    #[test]
    fn new_takes_a_name() {
        let cli = parse(&["new", "media"]).unwrap();
        assert_eq!(cli.command, Command::New("media".into()));
        assert!(parse(&["new"]).is_err());
    }

    #[test]
    fn windowed_flag_is_global() {
        let cli = parse(&["--windowed", "x"]).unwrap();
        assert!(cli.windowed);
        assert_eq!(cli.command, Command::Run);
        assert_eq!(cli.target, Some(PathBuf::from("x")));
    }

    #[test]
    fn placement_flag_is_parsed() {
        let cli = parse(&["--placement", "--widgets", "/tmp/a"]).unwrap();
        assert!(cli.start_placement);
        assert_eq!(cli.explicit_target(), Some(PathBuf::from("/tmp/a")));
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(&["--help"]).unwrap().command, Command::Help);
        assert_eq!(parse(&["-V"]).unwrap().command, Command::Version);
    }
}
