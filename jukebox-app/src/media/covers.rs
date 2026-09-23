//! MÓDULO 7 — Capas de Álbum (extração ID3 APIC + cache em disco)
//!
//! A navegação por capas precisa da arte embutida nos MP3 (frame ID3
//! `APIC`). Este serviço roda numa thread própria e NUNCA bloqueia a UI:
//!
//!   * `main.rs` envia `CoverCommand::Scan` com o catálogo agrupado (após o
//!     scanner inicial e após cada sincronização USB);
//!   * para cada disco, a thread procura o PRIMEIRO MP3 com imagem embutida
//!     (priorizando `PictureType::CoverFront`), decodifica com a crate
//!     `image` e gera uma miniatura de até 256×256 px;
//!   * o resultado vira `CoverEvent::Ready` com pixels RGB crus — a textura
//!     do Slint só é criada dentro do event loop (`slint::Image` não é Send);
//!   * o cache em `/dados/capas` (formato cru `.jbc`) faz a extração custar
//!     UMA vez por disco na vida da máquina — o HD mecânico do Sempron 145
//!     agradece nos boots seguintes.
//!
//! Decodificar JPEG num Sempron 145 leva algumas centenas de milissegundos
//! por capa: por isso as capas aparecem progressivamente (o carrossel mostra
//! placeholders coloridos com a inicial do disco enquanto isso).

use crate::state::models::{AlbumInfo, CoverArt};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/// Lado máximo da miniatura (px) — 256×256 RGB = 192 KB por disco,
/// textura pequena o bastante para a GPU integrada GMA 3150
const COVER_SIZE: u32 = 256;

/// Assinatura do arquivo de cache (versão do formato)
const CACHE_MAGIC: [u8; 4] = *b"JBC1";

/// Tamanho do cabeçalho do cache: magic(4) + width(4) + height(4)
const CACHE_HEADER: usize = 12;

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

            let cache_dir = resolve_cache_dir();
            if let Err(e) = fs::create_dir_all(&cache_dir) {
                log::warn!("Capas: impossível criar cache {:?}: {}", cache_dir, e);
            }

            // Memória do serviço: chave → Some(capa) produzida, ou None para
            // discos sem arte embutida (cache negativo evita reabrir MP3s)
            let mut produced: HashMap<String, Option<CoverArt>> = HashMap::new();

            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    CoverCommand::Scan(albums) => {
                        for album in &albums {
                            match produced.get(&album.key) {
                                // Já produzida: reemite para o modelo atual
                                Some(Some(art)) => {
                                    let _ = event_tx.send(CoverEvent::Ready {
                                        key: album.key.clone(),
                                        art: art.clone(),
                                    });
                                }
                                // Cache negativo: este disco não tem capa
                                Some(None) => {}
                                // Primeira vez: disco em cache → extrai
                                None => {
                                    let art = load_from_cache(&cache_dir, &album.key)
                                        .or_else(|| {
                                            let art = extract_from_album(album);
                                            if let Some(art) = &art {
                                                save_to_cache(&cache_dir, &album.key, art);
                                            }
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

/// Extrai a capa de um álbum: percorre os MP3s do disco até achar o
/// primeiro com imagem embutida (frame ID3 APIC), priorizando a capa
/// frontal. Vídeos (mp4/wmv/mpeg) não têm tag ID3 — ficam com o
/// placeholder colorido.
fn extract_from_album(album: &AlbumInfo) -> Option<CoverArt> {
    for track in &album.tracks {
        if track.file_type != "mp3" {
            continue;
        }

        let tag = match id3::Tag::read_from_path(&track.file_path) {
            Ok(tag) => tag,
            Err(_) => continue, // sem tag ou arquivo ilegível: tenta o próximo
        };

        let pictures: Vec<&id3::frame::Picture> = tag.pictures().collect();
        if pictures.is_empty() {
            continue;
        }

        // Ordem de preferência: capa frontal → primeira imagem qualquer
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

    log::debug!("Capas: '{}' não tem arte embutida.", album.title);
    None
}

/// Decodifica bytes JPEG/PNG e gera a miniatura RGB (até 256×256,
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
