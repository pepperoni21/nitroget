//! Filesystem discovery: widget folders, config location and hashing.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::config;

/// `$XDG_CONFIG_HOME/nitroget` (usually `~/.config/nitroget`). This is the
/// default widget directory.
fn config_base() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));

    base.join("nitroget")
}

/// Placement/monitor config for a widget set. It lives inside the widget
/// folder, so different folders keep independent layouts. A single-file target
/// uses its parent directory.
pub fn config_path_for(target: &Path) -> PathBuf {
    let dir = if target.is_file() {
        target.parent().unwrap_or(target)
    } else {
        target
    };

    dir.join(config::FILE_NAME)
}

/// Widget directory: `$NITROGET_DIR`, else `~/.config/nitroget`.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("NITROGET_DIR")
        && !dir.is_empty()
    {
        return expand_tilde(&dir);
    }

    config_base()
}

pub fn home() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Expands a leading `~` for paths typed by hand.
pub fn expand_tilde(value: &str) -> PathBuf {
    expand_tilde_with(value, &home())
}

pub(crate) fn expand_tilde_with(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(value)
}

/// Scans a folder recursively for `.nitroget` files, or returns a single file.
pub fn collect_paths(target: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    match target {
        Some(target) => {
            if target.is_file() {
                paths.push(target.to_path_buf());
            } else if target.is_dir() {
                scan(target, &mut paths, 0)?;
            } else {
                bail!("{} does not exist", target.display());
            }
        }
        None => {
            let dir = default_dir();
            if dir.is_dir() {
                scan(&dir, &mut paths, 0)?;
            }
        }
    }

    paths.sort();
    Ok(paths)
}

fn scan(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth > 3 {
        return Ok(());
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if depth == 0 => {
            return Err(e).with_context(|| format!("cannot read {}", dir.display()));
        }
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            scan(&path, out, depth + 1)?;
        } else if file_type.is_file() && is_widget(&path) {
            out.push(path);
        }
    }

    Ok(())
}

fn is_widget(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "nitroget")
}

pub fn mtime_of(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(UNIX_EPOCH)
}

pub fn lua_mtime(dir: &Path) -> SystemTime {
    lua_mtime_at(dir, 0)
}

fn lua_mtime_at(dir: &Path, depth: usize) -> SystemTime {
    if depth > 8 {
        return UNIX_EPOCH;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return UNIX_EPOCH;
    };

    let mut newest = UNIX_EPOCH;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();

        if file_type.is_dir() {
            newest = newest.max(lua_mtime_at(&path, depth + 1));
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "lua") {
            newest = newest.max(mtime_of(&path));
        }
    }

    newest
}

pub fn source_mtime(path: &Path) -> SystemTime {
    let mut newest = mtime_of(path);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        newest = newest.max(lua_mtime(parent));
    }
    newest
}

pub fn short_hash(value: &str) -> String {
    let mut hash = Fnv::new();
    hash.write(value.as_bytes());
    format!("{:08x}", hash.finish() as u32)
}

pub(crate) struct Fnv(u64);

impl Fnv {
    pub fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub fn write_u128(&mut self, value: u128) {
        self.write(&value.to_le_bytes());
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_without_touching_other_paths() {
        let home = PathBuf::from("/home/test");
        assert_eq!(expand_tilde_with("~", &home), home);
        assert_eq!(
            expand_tilde_with("~/widgets", &home),
            PathBuf::from("/home/test/widgets")
        );
        assert_eq!(
            expand_tilde_with("/tmp/widgets", &home),
            PathBuf::from("/tmp/widgets")
        );
    }

    #[test]
    fn config_lives_in_the_widget_folder() {
        let dir = std::env::temp_dir().join(format!("nitroget-config-test-{}", std::process::id()));
        let file = dir.join("clock.nitroget");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "").unwrap();

        assert_eq!(config_path_for(&dir), dir.join("config.toml"));
        assert_eq!(config_path_for(&file), dir.join("config.toml"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn source_mtime_includes_nested_lua_files() {
        let dir = std::env::temp_dir().join(format!("nitroget-lua-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();

        let widget = dir.join("clock.nitroget");
        let lua = lib.join("helper.lua");
        std::fs::write(&widget, "").unwrap();
        std::fs::write(&lua, "").unwrap();

        assert_eq!(lua_mtime(&dir), mtime_of(&lua));
        assert_eq!(source_mtime(&widget), mtime_of(&widget).max(mtime_of(&lua)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks() {
        let dir = std::env::temp_dir().join(format!("nitroget-scan-test-{}", std::process::id()));
        let root = dir.join("root");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        std::fs::write(root.join("ok.nitroget"), "").unwrap();
        std::fs::write(outside.join("hidden.nitroget"), "").unwrap();
        std::fs::write(outside.join("shared.nitroget"), "").unwrap();

        std::os::unix::fs::symlink(&outside, root.join("linked-dir")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("shared.nitroget"),
            root.join("shared.nitroget"),
        )
        .unwrap();

        let paths = collect_paths(Some(&root)).unwrap();
        assert!(paths.iter().any(|path| path.ends_with("ok.nitroget")));
        // Neither symlinked files nor symlinked directories are followed.
        assert!(!paths.iter().any(|path| path.ends_with("shared.nitroget")));
        assert!(!paths.iter().any(|path| path.ends_with("hidden.nitroget")));

        std::fs::remove_dir_all(&dir).ok();
    }
}
