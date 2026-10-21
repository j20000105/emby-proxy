use anyhow::Result;
use chrono::{NaiveDate, Utc};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tracing_appender::non_blocking::WorkerGuard;

const PREFIX: &str = "emby-proxy.log";

pub fn prune(directory: &Path, days: u16) -> Result<()> {
    let earliest = Utc::now().date_naive() - chrono::Duration::days(i64::from(days) - 1);
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(date) = name
            .to_str()
            .and_then(|s| s.strip_prefix("emby-proxy.log."))
        else {
            continue;
        };
        if let Ok(date) = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            && date < earliest
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub fn init(directory: &str, days: u16) -> Result<WorkerGuard> {
    std::fs::create_dir_all(directory)?;
    prune(Path::new(directory), days)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(PREFIX)
        .max_log_files(usize::from(days))
        .build(directory)?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        .finish(appender);
    tracing_subscriber::fmt()
        .json()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    let directory = PathBuf::from(directory);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let directory = directory.clone();
            if !matches!(
                tokio::task::spawn_blocking(move || prune(&directory, days)).await,
                Ok(Ok(()))
            ) {
                tracing::error!("log retention cleanup failed");
            }
        }
    });
    Ok(guard)
}

pub struct Access {
    pub id: String,
    pub method: String,
    pub route: &'static str,
    pub started: Instant,
    pub status: u16,
    pub redirects: usize,
    pub bytes: u64,
    pub complete: bool,
}

impl Drop for Access {
    fn drop(&mut self) {
        tracing::info!(request_id=%self.id, method=%self.method, route=self.route,
            status=self.status, redirects=self.redirects, bytes_streamed=self.bytes,
            elapsed_ms=self.started.elapsed().as_millis() as u64,
            complete=self.complete, "request finished");
    }
}
