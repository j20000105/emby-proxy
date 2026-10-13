use crate::config::{Config, normalize_host};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const HELP: &str = "emby-proxy [--config FILE] [--check-config]\n\
emby-proxy [--config FILE] reload [--socket PATH]\n\
emby-proxy [--config FILE] route list|get HOST|add HOST URL|update HOST URL|delete HOST\n\
emby-proxy [--config FILE] default [URL]\n\
Route/default edits only update the file. Run reload explicitly to apply them.";

pub struct Args {
    pub path: Option<PathBuf>,
    pub socket: Option<PathBuf>,
    pub command: Vec<String>,
}

pub fn parse() -> Result<Args> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args {
        path: None,
        socket: None,
        command: Vec::new(),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                parsed.path = Some(args.next().context("--config requires a path")?.into())
            }
            "--socket" => {
                parsed.socket = Some(args.next().context("--socket requires a path")?.into())
            }
            "--version" | "--help" | "--check-config" => parsed.command.push(arg),
            value if value.starts_with('-') => bail!("unknown option: {value}"),
            _ => parsed.command.push(arg),
        }
    }
    Ok(parsed)
}

pub fn config_path(path: Option<&Path>) -> PathBuf {
    path.unwrap_or(Path::new("/etc/emby-proxy/proxy.toml"))
        .to_owned()
}

// Serialize writers across processes, preserve comments/permissions/ownership,
// and replace the file only after validating the complete edited document.
fn edit(path: &Path, change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let lock_path = parent.join(format!(
        ".{}.lock",
        path.file_name()
            .context("invalid config path")?
            .to_string_lossy()
    ));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    lock.lock()?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file(),
        "edit a regular configuration file, not a symlink"
    );
    let original = Config::read(path)?;
    Config::parse(&original)?;
    let mut document: toml_edit::DocumentMut = original.parse()?;
    change(&mut document)?;
    let edited = document.to_string();
    Config::parse(&edited)?;
    let temporary = parent.join(format!(".emby-proxy-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        output.write_all(edited.as_bytes())?;
        if output.metadata()?.uid() != metadata.uid() || output.metadata()?.gid() != metadata.gid()
        {
            std::os::unix::fs::chown(&temporary, Some(metadata.uid()), Some(metadata.gid()))?;
        }
        output.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o777))?;
        output.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result?;
    println!("Configuration saved; run emby-proxy reload to apply.");
    Ok(())
}

pub fn file_command(path: &Path, args: &[&str]) -> Result<()> {
    match args {
        ["route", "list"] => {
            let config = Config::load(Some(path))?;
            let routes: BTreeMap<_, _> = config.routes.into_iter().collect();
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"default_upstream": config.default_upstream, "routes": routes})
                )?
            );
        }
        ["route", "get", host] => {
            let host = normalize_host(host).context("invalid host")?;
            let config = Config::load(Some(path))?;
            println!(
                "{}",
                config.routes.get(&host).context("route does not exist")?
            );
        }
        ["route", operation @ ("add" | "update"), host, upstream] => {
            let host = normalize_host(host).context("invalid host")?;
            crate::config::origin(upstream)?;
            edit(path, |doc| {
                if doc.get("routes").is_none() {
                    doc["routes"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let routes = doc["routes"]
                    .as_table_like_mut()
                    .context("routes must be a table")?;
                let existing = routes
                    .iter()
                    .find(|(k, _)| normalize_host(k).as_ref() == Some(&host))
                    .map(|(k, _)| k.to_owned());
                ensure!(
                    (*operation == "add") == existing.is_none(),
                    "add requires a new host; update requires an existing host"
                );
                let key = existing.as_deref().unwrap_or(&host);
                if let Some(item) = routes.get_mut(key) {
                    replace_string(item, upstream);
                } else {
                    routes.insert(key, toml_edit::value(*upstream));
                }
                Ok(())
            })?;
        }
        ["route", "delete", host] => {
            let host = normalize_host(host).context("invalid host")?;
            edit(path, |doc| {
                let routes = doc
                    .get_mut("routes")
                    .and_then(|v| v.as_table_like_mut())
                    .context("route does not exist")?;
                let key = routes
                    .iter()
                    .find(|(k, _)| normalize_host(k).as_ref() == Some(&host))
                    .map(|(k, _)| k.to_owned())
                    .context("route does not exist")?;
                routes.remove(&key);
                Ok(())
            })?;
        }
        ["default"] => println!("{}", Config::load(Some(path))?.default_upstream),
        ["default", upstream] => {
            crate::config::origin(upstream)?;
            edit(path, |doc| {
                replace_string(&mut doc["default_upstream"], upstream);
                Ok(())
            })?;
        }
        _ => bail!("{HELP}"),
    }
    Ok(())
}

fn replace_string(item: &mut toml_edit::Item, text: &str) {
    let mut value = toml_edit::Value::from(text);
    if let Some(previous) = item.as_value() {
        *value.decor_mut() = previous.decor().clone();
    }
    *item = toml_edit::Item::Value(value);
}
