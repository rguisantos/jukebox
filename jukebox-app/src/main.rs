//! JUKEBOX ARCADE OS — Ponto de entrada e integração de todos os módulos
//!
//! Mapa das threads (arquitetura de canais mpsc — a UI nunca bloqueia):
//!
//!   [UI Slint]  ←invoke_from_event_loop→  bridges de eventos
//!      │ callbacks (Z / Enter / retry / fechar USB / geometria de vídeo)
//!      ▼
//!   [Banco SQLite] — créditos: AddCredit (moedeiro+PIX) e RequestPlay
//!      │ (débito atômico → Enqueue no player)
//!      ▼
//!   [Player GStreamer] — playbin/xvimagesink/alsasink + fila (Módulo 3)
//!   [USB Sync]         — detecta /media/usb, copia, reescaneia (Módulo 4)
//!   [PIX Service]      — Tokio isolado: QR dinâmico + polling (Módulo 5)
//!   [Scanner]          — indexação inicial do catálogo (Módulo 2)

mod db;
mod finance;
mod media;
mod state;

use db::Database;
use finance::pix::{PixConfig, PixService, PixUiEvent};
use media::player::{self, PlayerCommand, PlayerEvent};
use media::usb_sync::{self, UsbSyncEvent};
use media::scanner;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use state::models::TrackInfo;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// Carrega as structs geradas a partir do arquivo ui/app_window.slint
slint::include_modules!();

/// Comandos despachados da UI/PIX para a thread do banco de dados.
/// Único dono da conexão SQLite principal → zero corrida de escrita.
#[derive(Debug)]
enum DbCommand {
    /// Moeda (tecla Z) ou PIX confirmado: credita
    AddCredit(u32),
    /// Enter numa faixa: débito atômico de 1 crédito + enfileiramento no player
    RequestPlay(TrackData),
}

/// Geração do toast atual (evita que um timer antigo apague um toast novo)
static TOAST_GENERATION: AtomicU64 = AtomicU64::new(0);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Iniciando Jukebox Arcade OS...");

    // =========================================================================
    // 1. Banco de dados e estado inicial
    // =========================================================================
    let mut db = match Database::open() {
        Ok(database) => database,
        Err(err) => {
            log::error!("Erro crítico ao inicializar o banco de dados: {}", err);
            return Err(Box::new(err));
        }
    };

    let initial_credits = db.get_credits().unwrap_or(0);
    log::info!("Créditos persistentes carregados do banco: {}", initial_credits);

    // =========================================================================
    // 2. Interface Slint
    // =========================================================================
    let main_window = MainWindow::new()?;
    main_window.set_credits(initial_credits as i32);
    main_window.set_scanning(true);

    // =========================================================================
    // 3. Canais de comunicação entre as threads
    // =========================================================================
    let (db_tx, db_rx) = mpsc::channel::<DbCommand>();
    let (player_cmd_tx, player_cmd_rx) = mpsc::channel::<PlayerCommand>();
    let (player_event_tx, player_event_rx) = mpsc::channel::<PlayerEvent>();
    let (usb_event_tx, usb_event_rx) = mpsc::channel::<UsbSyncEvent>();
    let (pix_event_tx, pix_event_rx) = mpsc::channel::<PixUiEvent>();

    // =========================================================================
    // 4. Thread do Banco de Dados (único escritor do SQLite principal)
    // =========================================================================
    {
        let player_tx = player_cmd_tx.clone();
        let ui_handle = main_window.as_weak();

        thread::spawn(move || {
            log::info!("Thread de persistência do SQLite iniciada.");
            while let Ok(cmd) = db_rx.recv() {
                match cmd {
                    DbCommand::AddCredit(amount) => {
                        log::info!("Processando crédito (+{}): moeda ou PIX.", amount);
                        apply_credit(&mut db, amount, &ui_handle);
                    }
                    DbCommand::RequestPlay(track_data) => {
                        let track = TrackInfo {
                            id: track_data.id as i64,
                            title: track_data.title.to_string(),
                            artist: track_data.artist.to_string(),
                            album: track_data.album.to_string(),
                            file_path: track_data.file_path.to_string(),
                            file_type: track_data.file_type.to_string(),
                        };

                        // Débito atômico: só enfileira se o saldo permitir
                        match db.spend_credits(1) {
                            Ok(Some(new_total)) => {
                                log::info!(
                                    "Crédito debitado (saldo: {}). Enfileirando '{}'.",
                                    new_total,
                                    track.title
                                );
                                update_credits_ui(&ui_handle, new_total);
                                if let Err(e) = player_tx.send(PlayerCommand::Enqueue(track)) {
                                    log::error!("Falha ao enviar faixa ao player: {}", e);
                                }
                            }
                            Ok(None) => {
                                log::warn!("Play negado: créditos insuficientes.");
                                show_toast(&ui_handle, "Créditos insuficientes", 2);
                            }
                            Err(e) => {
                                log::error!("Falha no débito de crédito: {}", e);
                                show_toast(&ui_handle, "Erro no banco de créditos", 2);
                            }
                        }
                    }
                }
            }
        });
    }

    // =========================================================================
    // 5. Thread do Scanner inicial (Módulo 2 — conexão SQLite própria em WAL)
    // =========================================================================
    {
        let ui_handle = main_window.as_weak();
        thread::spawn(move || {
            log::info!("Thread do scanner de mídia iniciada.");
            let mut scanner_db = match Database::open() {
                Ok(database) => database,
                Err(err) => {
                    log::error!("Scanner: falha ao abrir conexão: {}", err);
                    return;
                }
            };

            let tracks = scanner::scan_media_directory(&mut scanner_db);
            publish_catalog(&ui_handle, &tracks);
        });
    }

    // =========================================================================
    // 6. Player GStreamer (Módulo 3) + bridge de eventos → UI
    // =========================================================================
    player::spawn(player_cmd_rx, player_event_tx);

    {
        let ui_handle = main_window.as_weak();
        thread::spawn(move || {
            while let Ok(event) = player_event_rx.recv() {
                let ui_handle = ui_handle.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let ui = match ui_handle.upgrade() {
                        Some(ui) => ui,
                        None => return,
                    };
                    match event {
                        PlayerEvent::TrackStarted { id, title, artist, is_video } => {
                            ui.set_np_active(true);
                            ui.set_np_track_id(id as i32);
                            ui.set_np_title(title.into());
                            ui.set_np_artist(artist.into());
                            ui.set_np_video(is_video);
                        }
                        PlayerEvent::QueueUpdated { upcoming } => {
                            let model: Vec<TrackData> =
                                upcoming.iter().map(track_info_to_data).collect();
                            ui.set_queue_tracks(ModelRc::new(VecModel::from(model)));
                        }
                        PlayerEvent::QueueFinished => {
                            ui.set_np_active(false);
                            ui.set_np_track_id(-1);
                            ui.set_np_title("".into());
                            ui.set_np_artist("".into());
                            ui.set_np_video(false);
                        }
                        PlayerEvent::Error { context, detail } => {
                            log::error!("UI: erro do player em '{}': {}", context, detail);
                            show_toast(
                                &ui_handle,
                                &format!("Não foi possível tocar: {}", context),
                                2,
                            );
                        }
                    }
                });
            }
        });
    }

    // =========================================================================
    // 7. Sincronização USB (Módulo 4) + bridge de eventos → UI
    // =========================================================================
    usb_sync::spawn(usb_event_tx);

    {
        let ui_handle = main_window.as_weak();
        let player_tx = player_cmd_tx.clone();

        thread::spawn(move || {
            while let Ok(event) = usb_event_rx.recv() {
                match event {
                    UsbSyncEvent::Started { total } => {
                        // O vídeo XVideo pinta ACIMA do desenho do Slint:
                        // escondê-lo é pré-requisito para o popup ficar visível
                        let _ = player_tx.send(PlayerCommand::HideVideo);
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_usb_overlay_visible(true);
                                ui.set_usb_state(1);
                                ui.set_usb_total(total as i32);
                                ui.set_usb_copied(0);
                                ui.set_usb_new_tracks(0);
                            }
                        });
                    }
                    UsbSyncEvent::Progress { copied, total, current_file } => {
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_usb_state(1);
                                ui.set_usb_total(total as i32);
                                ui.set_usb_copied(copied as i32);
                                ui.set_usb_current_file(current_file.into());
                            }
                        });
                    }
                    UsbSyncEvent::Finished { copied, skipped, catalog } => {
                        log::info!(
                            "UI: sync USB concluído ({} copiados, {} pulados).",
                            copied,
                            skipped
                        );

                        // Catálogo atualizado no lugar (mesma thread do evento)
                        let track_count = catalog.len();
                        publish_catalog(&ui_handle, &catalog);

                        let ui_close = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_close.upgrade() {
                                ui.set_usb_state(2);
                                ui.set_usb_copied(copied as i32);
                                ui.set_usb_new_tracks(track_count as i32);
                                ui.set_selected_index(0);
                            }
                        });

                        // Auto-fechamento do overlay após 6s (o operador pode
                        // fechar antes pelo botão CONCLUIR)
                        let ui_auto = ui_handle.clone();
                        let player_auto = player_tx.clone();
                        thread::spawn(move || {
                            thread::sleep(Duration::from_secs(6));
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_auto.upgrade() {
                                    if ui.get_usb_overlay_visible()
                                        && ui.get_usb_state() == 2
                                    {
                                        ui.set_usb_overlay_visible(false);
                                        let _ = player_auto.send(PlayerCommand::RestoreVideo);
                                    }
                                }
                            });
                        });
                    }
                    UsbSyncEvent::Failed { reason } => {
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_usb_state(3);
                                ui.set_usb_error_text(reason.into());
                            }
                        });
                    }
                }
            }
        });
    }

    // =========================================================================
    // 8. Serviço PIX (Módulo 5) + bridge de eventos → UI
    // =========================================================================
    let pix_service = PixService::start(PixConfig::from_env(), pix_event_tx);

    {
        let ui_handle = main_window.as_weak();
        let db_tx = db_tx.clone();

        thread::spawn(move || {
            while let Ok(event) = pix_event_rx.recv() {
                match event {
                    PixUiEvent::Loading => {
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_pix_loading(true);
                                ui.set_pix_offline(false);
                            }
                        });
                    }
                    PixUiEvent::QrReady { rgb, width, height, copia_cola } => {
                        // O payload "Copia e Cola" vai para o log: útil para o
                        // operador colar manualmente num celular em último caso
                        log::info!("PIX Copia e Cola: {}", copia_cola);

                        // slint::Image NÃO é Send (Rc interna): o buffer RGB
                        // cru viaja até o event loop e vira textura lá dentro
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                let image = rgb_buffer_to_image(rgb, width, height);
                                ui.set_pix_qr_image(image);
                                ui.set_pix_loading(false);
                                ui.set_pix_offline(false);
                            }
                        });
                    }
                    PixUiEvent::Offline { reason } => {
                        log::warn!("PIX: backend inacessível: {}", reason);
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_pix_loading(false);
                                ui.set_pix_offline(true);
                                ui.set_pix_error_text(reason.into());
                            }
                        });
                    }
                    PixUiEvent::Paid { credits } => {
                        // 1) Credita no banco (thread do SQLite)
                        if let Err(e) = db_tx.send(DbCommand::AddCredit(credits)) {
                            log::error!("PIX: falha ao enviar crédito ao banco: {}", e);
                        }
                        // 2) Toast de confirmação
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            show_toast(
                                &ui_handle,
                                &format!(
                                    "PIX Recebido! +{} Crédito{}",
                                    credits,
                                    if credits == 1 { "" } else { "s" }
                                ),
                                1,
                            );
                        });
                        // 3) O próprio serviço PIX já busca o próximo QR
                    }
                }
            }
        });
    }

    // =========================================================================
    // 9. Callbacks da UI
    // =========================================================================

    // Tecla 'Z': pulso do moedeiro/noteiro
    {
        let tx_coin = db_tx.clone();
        main_window.on_coin_inserted(move || {
            log::debug!("Evento de moeda/tecla 'Z' detectado pela interface.");
            if let Err(e) = tx_coin.send(DbCommand::AddCredit(1)) {
                log::error!("Erro ao enviar comando de moeda para a fila: {}", e);
            }
        });
    }

    // Enter (ou clique/toque): debita e enfileira a faixa selecionada
    {
        let ui_handle = main_window.as_weak();
        let tx_play = db_tx.clone();
        main_window.on_track_activated(move |index| {
            let ui = match ui_handle.upgrade() {
                Some(ui) => ui,
                None => return,
            };
            let model = ui.get_tracks();
            if let Some(track) = model.iter().nth(index as usize) {
                if let Err(e) = tx_play.send(DbCommand::RequestPlay(track)) {
                    log::error!("Erro ao enviar faixa para débito: {}", e);
                }
            }
        });
    }

    // Botão "Tentar novamente" do painel PIX
    {
        let service = pix_service.clone();
        main_window.on_pix_retry(move || {
            log::info!("UI: retry manual do QR Code PIX.");
            service.request_refresh();
        });
    }

    // Fechamento do overlay USB (botão CONCLUIR/FECHAR)
    {
        let ui_handle = main_window.as_weak();
        let tx_player = player_cmd_tx.clone();
        main_window.on_usb_close(move || {
            if let Some(ui) = ui_handle.upgrade() {
                ui.set_usb_overlay_visible(false);
            }
            let _ = tx_player.send(PlayerCommand::RestoreVideo);
        });
    }

    // Geometria da área de vídeo (o Slint reporta px lógicos; o XVideo
    // trabalha em px físicos da janela — converte pelo fator de escala)
    {
        let ui_handle = main_window.as_weak();
        let tx_player = player_cmd_tx.clone();
        main_window.on_video_geometry_changed(move |x: f32, y: f32, width: f32, height: f32| {
            let scale = ui_handle
                .upgrade()
                .map(|ui| ui.window().scale_factor())
                .unwrap_or(1.0);
            let _ = tx_player.send(PlayerCommand::VideoGeometry {
                x: (x * scale).round() as i32,
                y: (y * scale).round() as i32,
                width: (width * scale).round() as i32,
                height: (height * scale).round() as i32,
            });
        });
    }

    // =========================================================================
    // 10. Handle da janela X11 → player (o vídeo é embutido via GstVideoOverlay)
    // =========================================================================
    if let Err(e) = main_window.show() {
        log::error!("Falha ao mapear a janela X11: {}", e);
    }
    {
        // Um Timer do próprio event loop consulta o xid periodicamente: o
        // acesso ao handle bruto precisa ocorrer na thread da UI, e a
        // janela pode levar alguns ciclos até ser mapeada pelo X11.
        // O player ignora xids repetidos — o timer eterno é barato (2Hz).
        let ui_handle = main_window.as_weak();
        let tx_player = player_cmd_tx.clone();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                if let Some(ui) = ui_handle.upgrade() {
                    if let Some(xid) = x11_window_id(ui.window()) {
                        let _ = tx_player.send(PlayerCommand::SetWindowHandle(xid));
                    }
                }
            },
        );
        // O timer precisa viver enquanto a aplicação viver — sem esta raiz
        // ele seria cancelado ao sair do escopo
        std::mem::forget(timer);
    }

    // =========================================================================
    // 11. Loop principal da interface (bloqueia até a janela fechar)
    // =========================================================================
    log::info!("Interface Slint pronta. Entrando no loop principal de eventos X11.");
    main_window.run()?;

    log::info!("Jukebox OS finalizado com sucesso.");
    Ok(())
}

// =============================================================================
// Utilidades de UI (executadas dentro do event loop do Slint)
// =============================================================================

/// Converte um TrackInfo (banco) em TrackData (modelo Slint)
fn track_info_to_data(t: &TrackInfo) -> TrackData {
    TrackData {
        id: t.id as i32,
        title: t.title.clone().into(),
        artist: t.artist.clone().into(),
        album: t.album.clone().into(),
        file_type: t.file_type.clone().into(),
        file_path: t.file_path.clone().into(),
    }
}

/// Publica o catálogo completo na UI e encerra o estado de escaneamento
fn publish_catalog(ui_handle: &slint::Weak<MainWindow>, tracks: &[TrackInfo]) {
    let model: Vec<TrackData> = tracks.iter().map(track_info_to_data).collect();
    let count = model.len();
    let ui_handle = ui_handle.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_handle.upgrade() {
            ui.set_tracks(ModelRc::new(VecModel::from(model)));
            ui.set_scanning(false);
            log::info!("Catálogo carregado na interface: {} faixas.", count);
        }
    });
}

/// Aplica um crédito no banco e reflete o novo saldo na UI
fn apply_credit(db: &mut Database, amount: u32, ui_handle: &slint::Weak<MainWindow>) {
    match db.increment_credits(amount) {
        Ok(new_total) => {
            log::info!("Créditos atualizados no SQLite: {}", new_total);
            update_credits_ui(ui_handle, new_total);
        }
        Err(e) => {
            log::error!("Falha ao gravar crédito no SQLite: {}", e);
        }
    }
}

/// Atualiza o badge de créditos (thread-safe: sempre dentro do event loop)
fn update_credits_ui(ui_handle: &slint::Weak<MainWindow>, new_total: u32) {
    let ui_handle = ui_handle.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_handle.upgrade() {
            ui.set_credits(new_total as i32);
        }
    });
}

/// Exibe um toast por 3,5s. kind: 0=info 1=sucesso 2=erro.
/// Um contador de geração impede que timers antigos apaguem toasts novos.
fn show_toast(ui_handle: &slint::Weak<MainWindow>, message: &str, kind: i32) {
    let generation = TOAST_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(ui) = ui_handle.upgrade() {
        ui.set_toast_message(message.into());
        ui.set_toast_kind(kind);
        ui.set_toast_visible(true);

        let ui_handle = ui_handle.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(3500));
            let ui_handle = ui_handle.clone();
            let _ = slint::invoke_from_event_loop(move || {
                // Somente esconde se nenhum toast mais novo foi exibido
                if TOAST_GENERATION.load(Ordering::SeqCst) == generation {
                    if let Some(ui) = ui_handle.upgrade() {
                        ui.set_toast_visible(false);
                    }
                }
            });
        });
    }
}

/// Converte o buffer RGB do QR Code (Módulo 5) em textura do Slint
fn rgb_buffer_to_image(rgb: Vec<u8>, width: u32, height: u32) -> slint::Image {
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
    let pixels = buffer.make_mut_slice();
    for (dst, src) in pixels.iter_mut().zip(rgb.chunks_exact(3)) {
        *dst = slint::Rgb8Pixel {
            r: src[0],
            g: src[1],
            b: src[2],
        };
    }
    slint::Image::from_rgb8(buffer)
}

/// Obtém o XID da janela X11 da UI (para o GstVideoOverlay do player).
/// Retorna None se a janela ainda não estiver mapeada ou não for X11.
/// Deve ser chamada na thread do event loop (o handle bruto não é Send).
fn x11_window_id(window: &slint::Window) -> Option<u64> {
    // Acesso ao backend bruto via traço raw-window-handle 0.6 — habilitado
    // pelo feature `raw-window-handle-06` do slint
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle();
    let raw = handle.window_handle().ok()?;
    match raw.as_raw() {
        // Xlib (winit/x11): window é c_ulong (u64 em x86_64)
        RawWindowHandle::Xlib(h) => Some(h.window as u64),
        // Xcb: window é NonZeroU32
        RawWindowHandle::Xcb(h) => Some(h.window.get() as u64),
        _ => None,
    }
}
