//! MÓDULO 4 — Sincronização de Mídia via Pendrive USB
//!
//! O operador do bar insere um pendrive; a regra udev da distro
//! (`/etc/udev/rules.d/99-jukebox-usb.rules`) monta a primeira partição
//! automaticamente em `/media/usb` em modo SOMENTE LEITURA (segurança:
//! o pendrive nunca é gravado e a montagem é instantânea).
//!
//! Esta thread apenas monitora (polling leve a cada 2s) o surgimento do
//! ponto de montagem. Ao detectar mídia nova:
//!   1. Emite `UsbSyncEvent::Started` — a UI exibe o overlay de sincronização
//!      e a thread principal oculta o retângulo de vídeo do GStreamer
//!      (o popup é desenhado pelo Slint, mas o vídeo XVideo é composto
//!      DIRETAMENTE na janela X11, acima de qualquer desenho do Slint).
//!   2. Copia recursivamente os arquivos `.mp3/.mp4/.wav/.wmv/.mpeg`
//!      para `/dados/musicas/` (pulando arquivos já existentes — sincronização
//!      idempotente: reinserir o mesmo pendrive não duplica nada).
//!   3. Roda o `scanner::scan_media_directory()` para indexar as novidades.
//!   4. Emite `UsbSyncEvent::Finished` com o catálogo completo atualizado.
//!   5. Aguarda a REMOÇÃO do pendrive antes de voltar ao estado ocioso,
//!      para que o mesmo pendrive não dispare uma segunda sincronização.
//!
//! Nenhuma operação bloqueia a thread de interface: todo o progresso é
//! comunicado via canal mpsc e aplicado com `slint::invoke_from_event_loop`.

use crate::db::Database;
use crate::media::scanner;
use crate::state::models::TrackInfo;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

/// Ponto de montagem criado pela regra udev da distro (montagem read-only)
const USB_MOUNT_POINT: &str = "/media/usb";

/// Extensões aceitas na sincronização (espelha o scanner do Módulo 2)
const SYNC_EXTENSIONS: &[&str] = &["mp3", "mp4", "wav", "wmv", "mpeg"];

/// Intervalo de verificação do ponto de montagem (leve: apenas um stat)
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Eventos enviados do worker de sincronização para a thread principal,
/// que os converte em atualizações da interface via event loop do Slint.
pub enum UsbSyncEvent {
    /// Pendrive detectado — abrir overlay de sincronização
    Started { total: usize },
    /// Um arquivo foi copiado — atualizar barra de progresso
    Progress {
        copied: usize,
        total: usize,
        current_file: String,
    },
    /// Sincronização concluída — catálogo completo atualizado no fim
    Finished {
        copied: usize,
        skipped: usize,
        catalog: Vec<TrackInfo>,
    },
    /// Falha de I/O durante a cópia (HD cheio, pendrive removido no meio...)
    Failed { reason: String },
}

/// Inicia a thread dedicada de monitoramento/sincronização USB.
/// Retorna imediatamente; toda a comunicação acontece via `event_tx`.
pub fn spawn(event_tx: Sender<UsbSyncEvent>) {
    thread::Builder::new()
        .name("usb-sync".to_string())
        .spawn(move || {
            log::info!("USB Sync: thread de monitoramento de /media/usb iniciada.");
            run_loop(&event_tx);
        })
        .expect("Falha crítica ao criar a thread de sincronização USB");
}

/// Máquina de estados principal: Ocioso → Copiando → Concluído → Aguarda remoção → Ocioso
fn run_loop(event_tx: &Sender<UsbSyncEvent>) {
    loop {
        // ------------------------------------------------------------------
        // ESTADO 1: OCIOSO — aguarda um pendrive com mídia aparecer
        // ------------------------------------------------------------------
        let files = loop {
            thread::sleep(POLL_INTERVAL);

            if !mount_has_media() {
                continue;
            }

            // Coleta a lista completa de arquivos de mídia do pendrive
            let files = collect_media_files(Path::new(USB_MOUNT_POINT));
            if files.is_empty() {
                // Pendrive sem mídia suportada: ignora silenciosamente
                continue;
            }

            break files;
        };

        log::info!(
            "USB Sync: pendrive detectado com {} arquivo(s) de mídia.",
            files.len()
        );

        // ------------------------------------------------------------------
        // ESTADO 2: COPIANDO — overlay aberto, vídeo ocultado pela UI
        // ------------------------------------------------------------------
        let total = files.len();
        let _ = event_tx.send(UsbSyncEvent::Started { total });

        let dest_dir = scanner::resolve_media_dir();
        let mut copied: usize = 0;
        let mut skipped: usize = 0;
        let mut failure: Option<String> = None;

        for src in &files {
            let file_name = src
                .file_name()
                .map(OsStr::to_string_lossy)
                .unwrap_or_default()
                .to_string();
            let dest = dest_dir.join(&file_name);

            // Sincronização idempotente: arquivo com mesmo nome já catalogado
            // no HD é pulado (evita duplicar mídia e poupa o HD mecânico lento)
            if dest.exists() {
                skipped += 1;
                log::debug!("USB Sync: pulando arquivo já existente: {}", file_name);
            } else if let Err(e) = fs::copy(src, &dest) {
                // Erro de cópia: pendrive removido no meio, HD cheio, etc.
                log::error!("USB Sync: falha ao copiar {:?}: {}", src, e);
                failure = Some(format!("{}: {}", file_name, e));
                break;
            }

            copied += 1;
            let _ = event_tx.send(UsbSyncEvent::Progress {
                copied,
                total,
                current_file: file_name,
            });
        }

        // ------------------------------------------------------------------
        // ESTADO 3: CONCLUÍDO / FALHA — reindexa catálogo e notifica a UI
        // ------------------------------------------------------------------
        match failure {
            Some(reason) => {
                let _ = event_tx.send(UsbSyncEvent::Failed { reason });
            }
            None => {
                // Reescaneia o diretório com conexão SQLite própria (modo WAL
                // permite concorrência com as outras threads do sistema)
                let catalog = match Database::open() {
                    Ok(mut db) => scanner::scan_media_directory(&mut db),
                    Err(e) => {
                        log::error!("USB Sync: falha ao abrir banco pós-cópia: {}", e);
                        Vec::new()
                    }
                };

                log::info!(
                    "USB Sync: concluído — {} copiados, {} pulados, catálogo com {} faixas.",
                    copied,
                    skipped,
                    catalog.len()
                );

                let _ = event_tx.send(UsbSyncEvent::Finished {
                    copied,
                    skipped,
                    catalog,
                });
            }
        }

        // ------------------------------------------------------------------
        // ESTADO 4: AGUARDA REMOÇÃO — evita redisparar o mesmo pendrive
        // ------------------------------------------------------------------
        while mount_has_media() {
            thread::sleep(POLL_INTERVAL);
        }
        log::info!("USB Sync: pendrive removido. Monitoramento ocioso novamente.");
    }
}

/// Verifica se o ponto de montagem existe, é um diretório e é legível.
/// (Quando o udev desmonta, /media/usb some do sistema de arquivos.)
fn mount_has_media() -> bool {
    let mount = Path::new(USB_MOUNT_POINT);
    mount.is_dir() && fs::read_dir(mount).is_ok()
}

/// Percorre recursivamente o pendrive coletando arquivos de mídia suportados
fn collect_media_files(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("USB Sync: não foi possível ler {:?}: {}", dir, e);
            return results;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            results.extend(collect_media_files(&path));
        } else if is_syncable_media(&path) {
            results.push(path);
        }
    }

    results
}

/// Verifica se o arquivo do pendrive tem extensão de mídia suportada
fn is_syncable_media(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| SYNC_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}
