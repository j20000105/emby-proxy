use crate::{Shared, config::Config};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    command: String,
    config_path: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Reply {
    ok: bool,
    error: Option<String>,
}

pub struct Control {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
    inode: u64,
    _lock: fs::File,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.task.abort();
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub async fn start(path: &Path, shared: Arc<Shared>) -> Result<Control> {
    let parent = path.parent().context("control socket has no parent")?;
    fs::create_dir_all(parent)?;
    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .context("invalid socket filename")?
            .to_string_lossy()
    ));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.try_lock()
        .context("another proxy owns the control socket lock")?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            metadata.file_type().is_socket(),
            "control socket path is not a socket"
        );
        match UnixStream::connect(path).await {
            Ok(_) => anyhow::bail!("another proxy owns the control socket"),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => fs::remove_file(path)?,
            Err(e) => return Err(e.into()),
        }
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(path)?;
    let owner = metadata.uid();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut connection, _)) = listener.accept().await else {
                break;
            };
            let Ok(peer) = connection.peer_cred() else {
                continue;
            };
            if peer.uid() != 0 && peer.uid() != owner {
                continue;
            }
            let shared = shared.clone();
            tokio::spawn(async move {
                let operation = async {
                    let mut bytes = Vec::new();
                    (&mut connection).take(4097).read_to_end(&mut bytes).await?;
                    ensure!(bytes.len() <= 4096, "control request is too large");
                    let command: Command = serde_json::from_slice(&bytes)?;
                    ensure!(command.command == "reload", "unknown command");
                    ensure!(
                        shared.path.as_ref() == Some(&command.config_path),
                        "configuration path does not match the running service"
                    );
                    shared.reload().await
                };
                let result = match tokio::time::timeout(Duration::from_secs(10), operation).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!("control request timed out")),
                };
                let reply = match result {
                    Ok(()) => Reply {
                        ok: true,
                        error: None,
                    },
                    Err(error) => {
                        tracing::warn!(
                            "configuration reload rejected; previous configuration retained"
                        );
                        Reply {
                            ok: false,
                            error: Some(format!("{error:#}").chars().take(1024).collect()),
                        }
                    }
                };
                if let Ok(bytes) = serde_json::to_vec(&reply) {
                    let _ = connection.write_all(&bytes).await;
                }
            });
        }
    });
    Ok(Control {
        task,
        path: path.into(),
        inode: metadata.ino(),
        _lock: lock,
    })
}

pub async fn reload(path: &Path, socket: Option<&Path>) -> Result<()> {
    // Validate the complete file before contacting the running process. The
    // process validates again and builds a replacement before its atomic swap.
    let config =
        Config::load(Some(path)).context("configuration validation failed; reload not sent")?;
    let mut stream = UnixStream::connect(socket.unwrap_or(&config.control_socket))
        .await
        .context("cannot connect to the running proxy control socket")?;
    let request = Command {
        command: "reload".into(),
        config_path: fs::canonicalize(path)?,
    };
    stream.write_all(&serde_json::to_vec(&request)?).await?;
    stream.shutdown().await?;
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(15),
        (&mut stream).take(8193).read_to_end(&mut bytes),
    )
    .await??;
    ensure!(bytes.len() <= 8192, "control reply is too large");
    let reply: Reply = serde_json::from_slice(&bytes)?;
    ensure!(
        reply.ok,
        "{}",
        reply.error.unwrap_or_else(|| "reload failed".into())
    );
    println!("Configuration validated and reloaded successfully.");
    Ok(())
}
