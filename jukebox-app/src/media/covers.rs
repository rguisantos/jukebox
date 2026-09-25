//! MÓDULO 7 — Capas de Álbum (arquivo na pasta do CD + ID3 APIC + cache)
//!
//! A navegação por capas precisa da arte do disco. Este serviço roda numa
//! thread própria (nice alto = baixa prioridade) e NUNCA bloqueia a UI:
//!
//!   1. arquivo na pasta do álbum (`cover.jpg`, `folder.jpg`, `front.jpg`,
//!      jpeg/png/webp) — o jeito mais comum nos CDs ripados;
//!   2. frame ID3 `APIC` no primeiro MP3 com imagem;
//!   3. cache em `/dados/capas` (formato cru `.jbc`) para não redecodificar.
//!
//! Decodificar JPEG num Sempron 145 compete com o alsasink: por isso esta
//! thread cede CPU (`nice 19` + pausa entre discos) e as capas aparecem
//! aos poucos enquanto o player continua preenchendo o buffer de áudio.

use crate::state::models::{AlbumInfo, CoverArt};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;

/// Lado máximo da miniatura (px) — 256×256 RGB = 192 KB por disco,
/// textura pequena o bastante para a GPU integrada GMA 3150
const COVER_SIZE: u32 = 256;

/// Assinatura do arquivo de cache (versão do formato)
const CACHE_MAGIC: [u8; 4] = *b"JBC1";

/// Tamanho do cabeçalho do cache: magic(4) + width(4) + height(4)
const CACHE_HEADER: usize = 12;

/// Pausa entre discos que ainda precisam de decode (deixa o áudio respirar)
const DECODE_YIELD: Duration = Duration::from_millis(40);

const FOLDER_COVER_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "cover.webp",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "folder.webp",
    "front.jpg",
    "front.jpeg",
    "front.png",
    "front.webp",
    "album.jpg",
    "album.jpeg",
    "album.png",
    "album.webp",
    "albumart.jpg",
    "AlbumArt.jpg",
    "AlbumArtSmall.jpg",
];

/// Comandos recebidos pelo serviço de capas
pub enum CoverCommand {
    /// Varre a lista de álbuns extraindo capas ainda não produzidas.
    /// Reemite capas já conhecidas: o modelo da UI pode ter sido trocado
    /// por uma sincronização USB e precisa receber a textura de novo.
    Scan(Vec<AlbumInfo>),
}

/// Eventos emitidos pelo serviço de capas para a UI
pub enum CoverEvent {
    /// Capa pronta (ou reemitida para um modelo novo) — indexada pela
    /// chave do álbum ("artista|álbum")
    Ready { key: String, art: CoverArt },
}

/// Sobe a thread dedicada ao serviço de capas. Retorna imediatamente.
pub fn spawn(cmd_rx: Receiver<CoverCommand>, event_tx: Sender<CoverEvent>) {
    thread::Builder::new()
        .name("album-covers".to_string())
        .spawn(move || {
            log::info!("Capas: thread de extração iniciada.");

            #[cfg(target_os = "linux")]
            {
                let tid = unsafe { libc::syscall(libc::SYS_gettid) } as libc::id_t;
                let _ = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, 19) };
            }

            let cache_dir = resolve_cache_dir();
            if let Err(e) = fs::create_dir_all(&cache_dir) {
                log::warn!("Capas: impossível criar cache {:?}: {}", cache_dir, e);
            }

            // Memória do serviço: chave → Some(capa) produzida, ou None para
            // discos sem arte (cache negativo evita reabrir MP3s)
            let mut produced: HashMap<String, Option<CoverArt>> = HashMap::new();

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    CoverCommand::Scan(albums) => {
                        for album in &albums {
                            match produced.get(&album.key) {
                                Some(Some(art)) => {
                                    let _ = event_tx.send(CoverEvent::Ready {
                                        key: album.key.clone(),
                                        art: art.clone(),
                                    });
                                }
                                Some(None) => {}
                                None => {
                                    let art = load_from_cache(&cache_dir, &album.key).or_else(|| {
                                        let art = extract_from_album(album);
                                        if let Some(art) = &art {
                                            save_to_cache(&cache_dir, &album.key, art);
                                        }
                                        // Cede o núcleo para o decode/ALSA no Sempron
                                        thread::sleep(DECODE_YIELD);
                                        art
                                    });
                                    produced.insert(album.key.clone(), art.clone());
                                    if let Some(art) = art {
                                        let _ = event_tx.send(CoverEvent::Ready {
                                            key: album.key.clone(),
                                            art,
                                        });
                                    }
                                }
                            }
                        }
                        log::info!(
                            "Capas: varredura concluída ({} discos no inventário).",
                            albums.len()
                        );
                    }
                }
            }
        })
        .expect("Falha crítica ao criar a thread de capas");
}

/// Diretório do cache: `/dados/capas` na máquina de produção,
/// `./dados/capas` no ambiente de desenvolvimento (mesma regra do SQLite).
fn resolve_cache_dir() -> PathBuf {
    if Path::new("/dados").is_dir() {
        PathBuf::from("/dados/capas")
    } else {
        PathBuf::from("./dados/capas")
    }
}

/// Caminho do arquivo de cache para uma chave de álbum
/// (hash FNV-1a 64-bit — estável entre execuções)
fn cache_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{:016x}.jbc", crate::state::models::fnv64(key)))
}

/// Carrega uma capa do cache em disco. Formato: `JBC1` + width + height
/// (u32 little-endian) + pixels RGB intercalados. Arquivo corrupto ou
/// truncado (queda de energia no meio da escrita) é descartado sem pânico.
fn load_from_cache(dir: &Path, key: &str) -> Option<CoverArt> {
    let bytes = fs::read(cache_path(dir, key)).ok()?;
    if bytes.len() < CACHE_HEADER || bytes[..4] != CACHE_MAGIC {
        return None;
    }

    let width = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    if width == 0 || height == 0 || width > COVER_SIZE || height > COVER_SIZE {
        return None;
    }
    if bytes.len() != CACHE_HEADER + (width as usize) * (height as usize) * 3 {
        return None;
    }

    Some(CoverArt {
        rgb: bytes[CACHE_HEADER..].to_vec(),
        width,
        height,
    })
}

/// Grava uma capa no cache (best-effort: falha de disco é só um log)
fn save_to_cache(dir: &Path, key: &str, art: &CoverArt) {
    let mut buffer = Vec::with_capacity(CACHE_HEADER + art.rgb.len());
    buffer.extend_from_slice(&CACHE_MAGIC);
    buffer.extend_from_slice(&art.width.to_le_bytes());
    buffer.extend_from_slice(&art.height.to_le_bytes());
    buffer.extend_from_slice(&art.rgb);

    if let Err(e) = fs::write(cache_path(dir, key), buffer) {
        log::warn!("Capas: falha ao gravar cache de {:?}: {}", key, e);
    }
}

/// Extrai a capa: primeiro a pasta do CD, depois APIC nos MP3s.
fn extract_from_album(album: &AlbumInfo) -> Option<CoverArt> {
    if let Some(art) = extract_from_folder(album) {
        return Some(art);
    }
    extract_from_id3(album)
}

fn album_dir(album: &AlbumInfo) -> Option<PathBuf> {
    album
        .tracks
        .first()
        .and_then(|t| Path::new(&t.file_path).parent().map(|p| p.to_path_buf()))
}

fn extract_from_folder(album: &AlbumInfo) -> Option<CoverArt> {
    let dir = album_dir(album)?;

    for name in FOLDER_COVER_NAMES {
        let path = dir.join(name);
        if path.is_file() {
            if let Some(art) = read_cover_file(&path) {
                log::info!("Capas: arquivo {:?} em '{}'.", path.file_name(), album.title);
                return Some(art);
            }
        }
    }

    let entries = fs::read_dir(&dir).ok()?;
    let mut fallback: Option<PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
        {
            Some(ext) => ext,
            None => continue,
        };
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if matches!(
            stem.as_str(),
            "cover" | "folder" | "front" | "album" | "albumart" | "albumartsmall"
        ) {
            if let Some(art) = read_cover_file(&path) {
                log::info!("Capas: arquivo {:?} em '{}'.", path.file_name(), album.title);
                return Some(art);
            }
        } else if fallback.is_none() {
            fallback = Some(path);
        }
    }

    if let Some(path) = fallback {
        if let Some(art) = read_cover_file(&path) {
            log::info!(
                "Capas: imagem {:?} usada como capa de '{}'.",
                path.file_name(),
                album.title
            );
            return Some(art);
        }
    }

    None
}

fn read_cover_file(path: &Path) -> Option<CoverArt> {
    let bytes = fs::read(path).ok()?;
    decode_to_cover(&bytes)
}

fn extract_from_id3(album: &AlbumInfo) -> Option<CoverArt> {
    for track in &album.tracks {
        if track.file_type != "mp3" {
            continue;
        }

        let tag = match id3::Tag::read_from_path(&track.file_path) {
            Ok(tag) => tag,
            Err(_) => continue,
        };

        let pictures: Vec<&id3::frame::Picture> = tag.pictures().collect();
        if pictures.is_empty() {
            continue;
        }

        let picture = pictures
            .iter()
            .find(|p| p.picture_type == id3::frame::PictureType::CoverFront)
            .copied()
            .unwrap_or(pictures[0]);

        if let Some(art) = decode_to_cover(&picture.data) {
            log::info!(
                "Capas: arte extraída de '{}' ({}).",
                track.title,
                picture.mime_type
            );
            return Some(art);
        }
    }

    log::debug!("Capas: '{}' não tem arte na pasta nem no ID3.", album.title);
    None
}

/// Decodifica bytes JPEG/PNG/WebP e gera a miniatura RGB (até 256×256,
/// proporção preservada — o Slint recorta com `ImageFit.cover`)
fn decode_to_cover(data: &[u8]) -> Option<CoverArt> {
    let image = image::load_from_memory(data).ok()?;
    let thumbnail = image.thumbnail(COVER_SIZE, COVER_SIZE);
    let rgb = thumbnail.to_rgb8();
    let (width, height) = rgb.dimensions();
    Some(CoverArt {
        rgb: rgb.into_raw(),
        width,
        height,
    })
}
