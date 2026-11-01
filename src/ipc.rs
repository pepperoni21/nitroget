//! Minimal control socket for the running daemon.
//!
//! There is a single daemon, so the socket is global:
//! `$XDG_RUNTIME_DIR/nitroget.sock`. If `XDG_RUNTIME_DIR` is missing, a private
//! `$TMPDIR/nitroget-<uid>/` directory is used instead.
//!
//! Clients send one command line. `where` replies with the widget folder the
//! daemon is using; `quit` stops it; anything else is forwarded to the message
//! loop (for example `placement`).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn socket_path() -> PathBuf {
    runtime_dir().join("nitroget.sock")
}

fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|dir| !dir.is_empty())
    {
        let path = PathBuf::from(dir);
        if is_private_runtime_dir(&path) {
            return path;
        }
        eprintln!(
            "nitroget: ignoring insecure XDG_RUNTIME_DIR {}",
            path.display()
        );
    }

    fallback_dir()
}

fn fallback_dir() -> PathBuf {
    std::env::temp_dir().join(format!("nitroget-{}", uid()))
}

/// The directory must belong to this user and must not be writable by anyone
/// else. Otherwise another local user could plant a socket or swap it during
/// the brief bind/chmod window.
fn is_private_runtime_dir(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() && meta.uid() == uid() => {
            meta.permissions().mode() & 0o022 == 0
        }
        _ => false,
    }
}

fn uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(0)
}

pub fn send(path: &Path, command: &str) -> Result<String> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("cannot connect to {}", path.display()))?;

    // A wedged daemon must not block `nitroget` or `nitroget placement`
    // forever. `serve` still refuses to replace a socket that accepts
    // connections, so a timeout cannot silently start a second daemon.
    stream
        .set_write_timeout(Some(REPLY_TIMEOUT))
        .and_then(|_| stream.set_read_timeout(Some(REPLY_TIMEOUT)))
        .with_context(|| format!("cannot configure {}", path.display()))?;

    stream
        .write_all(command.as_bytes())
        .and_then(|_| stream.write_all(b"\n"))
        .and_then(|_| stream.flush())
        .with_context(|| format!("cannot write to {}", path.display()))?;

    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .with_context(|| format!("no reply from {}", path.display()))?;

    Ok(reply.trim().to_string())
}

/// Creates or secures the private fallback runtime directory.
fn prepare_private_dir(parent: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    match std::fs::symlink_metadata(parent) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("{} is a symlink; refusing to use it", parent.display());
        }
        Ok(meta) if !meta.file_type().is_dir() => {
            bail!("{} is not a directory", parent.display());
        }
        Ok(meta) if meta.uid() != uid() => {
            bail!("{} is not owned by you", parent.display());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        Err(e) => {
            return Err(e).with_context(|| format!("cannot inspect {}", parent.display()));
        }
    }

    // Re-check after creation and then tighten. `set_permissions` also fails if
    // a raced-in directory is not ours.
    let meta = std::fs::symlink_metadata(parent)
        .with_context(|| format!("cannot inspect {}", parent.display()))?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() || meta.uid() != uid() {
        bail!(
            "{} is not a private directory owned by you",
            parent.display()
        );
    }

    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot secure {}", parent.display()))?;
    Ok(())
}

/// Binds the control socket and serves commands until the daemon exits.
///
/// `folder` is the widget folder the daemon is running, reported to `where`.
/// Binding this socket is what enforces a single daemon instance.
pub fn serve(
    path: PathBuf,
    folder: String,
    tx: async_channel::Sender<String>,
) -> Result<SocketGuard> {
    let parent = path.parent().unwrap_or_else(|| Path::new("/tmp"));
    let fallback = parent == fallback_dir();

    if fallback {
        prepare_private_dir(parent)?;
    } else {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_socket() => {
            // Do not steal the socket of a live daemon.
            if UnixStream::connect(&path).is_ok() {
                bail!("nitroget is already running on {}", path.display());
            }
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove stale socket {}", path.display()))?;
        }
        Ok(_) => bail!("{} exists and is not a socket", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("cannot inspect {}", path.display()));
        }
    }

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        // Two processes can pass the stale-socket check at the same time.
        // Losing the bind race means the other one is the running daemon.
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            bail!("nitroget is already running on {}", path.display());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("cannot listen on {}", path.display()));
        }
    };

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("cannot secure {}", path.display()))?;

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            // A client that connects and never sends a line must not block the
            // accept loop forever.
            let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
            let _ = stream.set_write_timeout(Some(REPLY_TIMEOUT));
            let Ok(clone) = stream.try_clone() else {
                continue;
            };

            let mut reader = BufReader::new(clone);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }

            let command = line.trim().to_string();
            if command.is_empty() {
                continue;
            }

            if command == "where" {
                let _ = stream.write_all(folder.as_bytes());
                let _ = stream.write_all(b"\n");
                continue;
            }

            // Acknowledge `quit` before the main loop tears the daemon down,
            // otherwise the client can lose the race and see a broken pipe.
            if command == "quit" {
                let _ = stream.write_all(b"ok\n");
                let _ = tx.send_blocking(command);
                break;
            }

            if tx.send_blocking(command).is_err() {
                break;
            }
            let _ = stream.write_all(b"ok\n");
        }
    });

    Ok(SocketGuard { path })
}

/// Removes the control socket when the daemon shuts down cleanly. Crashes and
/// `SIGKILL` still leave the file behind; `serve` detects and replaces stale
/// sockets on the next start.
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nitroget-ipc-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn serves_where_and_forwards_commands() {
        let dir = test_dir("where");
        let path = dir.join("control.sock");
        let (tx, rx) = async_channel::unbounded();
        let guard = serve(path.clone(), "/tmp/widgets".into(), tx).unwrap();

        assert_eq!(send(&path, "where").unwrap(), "/tmp/widgets");

        send(&path, "placement").unwrap();
        assert_eq!(rx.recv_blocking().unwrap(), "placement");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        drop(guard);
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_replace_a_live_socket() {
        let dir = test_dir("busy");
        let path = dir.join("control.sock");
        let (tx, _rx) = async_channel::unbounded();
        let guard = serve(path.clone(), "/tmp/a".into(), tx).unwrap();

        let (tx, _rx) = async_channel::unbounded();
        let result = serve(path.clone(), "/tmp/b".into(), tx);
        assert!(result.is_err());
        let error = result.err().unwrap();
        assert!(format!("{error:#}").contains("already running"));

        drop(guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn private_runtime_dir_rejects_writable_directories() {
        let dir = test_dir("private-perms");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(is_private_runtime_dir(&dir));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!is_private_runtime_dir(&dir));

        prepare_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn private_runtime_dir_rejects_symlinks() {
        let dir = test_dir("private-link");
        let target = dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(!is_private_runtime_dir(&link));
        assert!(prepare_private_dir(&link).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
