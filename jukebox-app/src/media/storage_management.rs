//! Machine-local album operations. Player serialization protects purchased tracks.
use serde::Deserialize;
use std::{io::Write, process::{Command, Stdio}};

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct StorageAlbum {
    pub id: String,
    pub path: String,
    pub artist: String,
    pub title: String,
    pub genre: String,
    pub size: u64,
    pub online: bool,
    pub removed: bool,
}

#[derive(Clone, Debug)]
pub enum StorageOperation {
    List,
    Remove(StorageAlbum),
    Restore(StorageAlbum),
}

#[derive(Deserialize)]
pub struct StorageResult {
    pub rows: Vec<StorageAlbum>,
    pub changed: bool,
    pub restore: bool,
    pub message: String,
}

pub fn run(operation: &StorageOperation, protected: &[String]) -> Result<StorageResult, String> {
    let installed = std::path::Path::new("/opt/jukebox/storage_manage.py");
    let development = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../distro/config/includes.chroot/opt/jukebox/storage_manage.py");
    let script = if installed.exists() { installed } else { &development };
    let (action, album) = match operation {
        StorageOperation::List => ("list", None),
        StorageOperation::Remove(album) => ("remove", Some(album)),
        StorageOperation::Restore(album) => ("restore", Some(album)),
    };
    let media = super::scanner::resolve_media_dir();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let request = serde_json::json!({"action": action, "album": album,
        "protected_paths": protected, "working_dir": cwd});
    let mut child = Command::new("python3").arg(script)
        .env("JUKEBOX_DATA_DIR", media.parent().ok_or("Diretório de mídia inválido")?)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("Gerenciador indisponível: {e}"))?;
    let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    child.stdin.take().ok_or("Entrada do gerenciador indisponível")?
        .write_all(&input).map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()
            .and_then(|value| value["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_string());
        return Err(format!("Armazenamento: {error}"));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("Resposta de armazenamento inválida: {e}"))
}
