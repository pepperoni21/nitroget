//! Widget-folder and config watcher.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use async_channel::Sender;

use crate::discovery::{Fnv, collect_paths, source_mtime};
use crate::layout;
use crate::state::Msg;

const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Sends `Msg::Sync` whenever a widget file, the widget set, the config or the
/// monitor layout changes.
pub fn install(target: Option<PathBuf>, config_path: PathBuf, tx: Sender<Msg>) {
    let mut last: Option<u64> = None;

    gtk::glib::timeout_add_local(POLL_INTERVAL, move || {
        let current = fingerprint(target.as_deref(), &config_path);
        if last == Some(current) {
            return gtk::glib::ControlFlow::Continue;
        }
        last = Some(current);
        let _ = tx.try_send(Msg::Sync);
        gtk::glib::ControlFlow::Continue
    });
}

fn fingerprint(target: Option<&Path>, config_path: &Path) -> u64 {
    let mut hash = Fnv::new();

    hash.write(layout::monitor_signature().as_bytes());

    match collect_paths(target) {
        Ok(paths) => {
            for path in &paths {
                hash.write(path.to_string_lossy().as_bytes());
                hash.write_u128(
                    source_mtime(path)
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                );
            }
        }
        Err(_) => hash.write(b"<unreadable>"),
    }

    match std::fs::metadata(config_path).and_then(|m| m.modified()) {
        Ok(modified) => hash.write_u128(
            modified
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ),
        Err(_) => hash.write(b"<no-config>"),
    }

    let mut roots = Vec::new();
    match target {
        Some(path) if path.is_dir() => roots.push(path.to_path_buf()),
        Some(path) => {
            if let Some(parent) = path.parent() {
                roots.push(parent.to_path_buf());
            }
        }
        None => {}
    }
    if let Some(parent) = config_path.parent() {
        roots.push(parent.to_path_buf());
    }
    roots.sort();
    roots.dedup();

    for root in roots {
        hash.write(root.to_string_lossy().as_bytes());
        hash.write_u128(
            crate::discovery::css_mtime(&root)
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
    }

    hash.finish()
}
