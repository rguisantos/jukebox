//! Low-priority catalog worker. Operational credits are never imported.
use crate::{db::Database, state::models::AlbumInfo};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub enum OnlineEvent {
    Status(String),
    Catalog(Vec<AlbumInfo>),
}

pub fn spawn(events: mpsc::Sender<OnlineEvent>) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let interval = std::env::var("JUKEBOX_CATALOG_INTERVAL")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1800)
            .max(60);
        loop {
            let installed = std::path::Path::new("/opt/jukebox/catalog_sync.py");
            let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../distro/config/includes.chroot/opt/jukebox/catalog_sync.py");
            let script = if installed.exists() { installed } else { &dev };
            let mut changed = false;
            match Command::new("python3")
                .arg(script)
                .env(
                    "JUKEBOX_DATA_DIR",
                    super::scanner::resolve_media_dir().parent().unwrap(),
                )
                .stdout(Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(stdout) = child.stdout.take() {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                                changed |= value["changed"].as_bool().unwrap_or(false);
                                if let Some(message) = value["message"].as_str() {
                                    let _ = events.send(OnlineEvent::Status(message.to_string()));
                                }
                            }
                        }
                    }
                    if let Err(e) = child.wait() {
                        log::warn!("Atualização: {}", e);
                    }
                }
                Err(e) => {
                    let _ = events.send(OnlineEvent::Status(format!(
                        "Atualizador indisponível: {}",
                        e
                    )));
                }
            }
            if changed {
                match Database::open() {
                    Ok(mut db) => {
                        super::scanner::scan_media_directory(&mut db);
                        match db.get_albums() {
                            Ok(albums) => {
                                let _ = events.send(OnlineEvent::Catalog(albums));
                            }
                            Err(e) => log::error!("Catálogo: {}", e),
                        }
                    }
                    Err(e) => log::error!("Catálogo: {}", e),
                }
            }
            // Collapse repeated manual clicks during a download into one check.
            while rx.try_recv().is_ok() {}
            match rx.recv_timeout(Duration::from_secs(interval)) {
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
        }
    });
    tx
}
