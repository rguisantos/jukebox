//! JUKEBOX ARCADE OS — Ponto de entrada e integração de todos os módulos
//!
//! Mapa das threads (arquitetura de canais mpsc — a UI nunca bloqueia):
//!
//!   [UI Slint]  ←invoke_from_event_loop→  bridges de eventos
//!      │ arcade-key-pressed (E/R/I/W/Q/O/U/P/Z/X case-insensitive)
//!      ▼
//!   [AppState] máquina de estados de foco (Módulos 7/8, state/models.rs)
//!      │ Action::* → banco / player / USB
//!      ▼
//!   [Banco SQLite] — créditos: AddCredit (moedeiro+PIX), RequestPlay
//!      (débito do preço vigente), SetVolume (persistente), stats do
//!      operador (Módulo 7) e contadores antifraude/preço/gêneros (Módulo 8)
//!      │ (débito atômico → Enqueue no player)
//!      ▼
//!   [Player GStreamer] — playbin/xvimagesink/alsasink + fila (Módulo 3)
//!   [USB Sync]         — detecta /media/usb, copia, reescaneia (Módulo 4);
//!      também executa a "Sincronizar Pendrive" do menu do operador
//!   [Capas de Álbum]   — ID3 APIC → miniaturas RGB + cache (Módulo 7)
//!   [PIX Service]      — Tokio isolado: QR dinâmico + polling (Módulo 5)
//!   [Scanner]          — indexação inicial + agrupamento por álbum (Mód. 2/7)
//!
//! A navegação é 100% regida pela máquina de estados em `state/models.rs`:
//! este arquivo captura as teclas, pede a transição à máquina e espelha o
//! estado resultante nas propriedades do Slint (`mirror_nav` — o Slint é
//! renderizador puro, sem lógica de navegação própria).

mod db;
mod finance;
mod media;
mod state;

use db::Database;
use finance::pix::{PixConfig, PixService, PixUiEvent};
use media::covers::{self, CoverCommand, CoverEvent};
use media::player::{self, PlayerCommand, PlayerEvent};
use media::usb_sync::{self, UsbSyncCommand, UsbSyncEvent};
use media::scanner;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use state::models::{
    Action, AppState, AlbumInfo, CoverArt, FocusState, GenreInfo, TrackInfo, VOLUME_DEFAULT,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

// Carrega as structs geradas a partir do arquivo ui/app_window.slint
slint::include_modules!();

/// Chave da tabela chave-valor onde o volume persiste entre reinicializações
const VOLUME_CONFIG_KEY: &str = "volume";

/// Comandos despachados da UI/PIX para a thread do banco de dados.
/// Único dono da conexão SQLite principal → zero corrida de escrita.
#[derive(Debug)]
enum DbCommand {
    /// Moeda (tecla Z) ou PIX confirmado: credita e alimenta os
    /// contadores antifraude (caixa parcial + odômetro — Módulo 8)
    AddCredit(u32),
    /// Tecla O numa faixa: débito atômico do preço vigente (Módulo 8)
    /// + enfileiramento no player
    RequestPlay(TrackData),
    /// Fechamento do overlay de volume: persiste o valor (Módulo 7)
    SetVolume(u32),
    /// Abertura do menu do operador: caixa parcial, odômetro, preço e
    /// gêneros do acervo (Módulo 8 — substitui o QueryCollected do Módulo 7)
    QueryOperatorStats,
    /// Submenu de preço: grava o novo preço da música (Módulo 8)
    SetSongPrice(u32),
    /// Submenu de gêneros: alterna o bloqueio e recarrega o catálogo
    /// público com o filtro aplicado (Módulo 8)
    ToggleGenre(String),
    /// "Zerar Caixa Parcial": zera o contador de recolhimento (Módulo 8)
    ResetPartial,
    /// "Zerar Créditos Atuais": zera os créditos não gastos (Módulo 8)
    ResetCredits,
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

    // MÓDULO 8 — Preço da música (créditos por reprodução), persistido no
    // banco: o bar ajusta uma vez e sobrevive a todos os ciclos de energia
    let initial_price = db.get_song_price().unwrap_or(1);
    log::info!("Preço da música carregado do banco: {} crédito(s)", initial_price);

    // Volume persistido na tabela chave-valor (Módulo 7): o bar ajusta uma
    // vez e o valor sobrevive a todos os ciclos de energia da máquina
    let initial_volume = db
        .get_config_i64(VOLUME_CONFIG_KEY)
        .ok()
        .flatten()
        .map(|v| v.clamp(0, 100) as u32)
        .unwrap_or(VOLUME_DEFAULT);
    log::info!("Volume persistido carregado do banco: {}%", initial_volume);

    // =========================================================================
    // 2. Interface Slint + máquina de estados de foco (Módulo 7)
    // =========================================================================
    let main_window = MainWindow::new()?;
    main_window.set_credits(initial_credits as i32);
    main_window.set_volume_value(initial_volume as i32);
    main_window.set_op_song_price(initial_price as i32);
    main_window.set_scanning(true);

    // Estado de navegação compartilhado: callbacks da UI (event loop) e
    // bridges de publicação de catálogo — Arc<Mutex> atravessa as threads.
    let state_arc: Arc<Mutex<AppState>> =
        Arc::new(Mutex::new(AppState::new(initial_volume, initial_price)));

    // =========================================================================
    // 3. Canais de comunicação entre as threads
    // =========================================================================
    let (db_tx, db_rx) = mpsc::channel::<DbCommand>();
    let (player_cmd_tx, player_cmd_rx) = mpsc::channel::<PlayerCommand>();
    let (player_event_tx, player_event_rx) = mpsc::channel::<PlayerEvent>();
    let (usb_cmd_tx, usb_cmd_rx) = mpsc::channel::<UsbSyncCommand>();
    let (usb_event_tx, usb_event_rx) = mpsc::channel::<UsbSyncEvent>();
    let (pix_event_tx, pix_event_rx) = mpsc::channel::<PixUiEvent>();
    let (cover_cmd_tx, cover_cmd_rx) = mpsc::channel::<CoverCommand>();
    let (cover_event_tx, cover_event_rx) = mpsc::channel::<CoverEvent>();

    // =========================================================================
    // 4. Thread do Banco de Dados (único escritor do SQLite principal)
    // =========================================================================
    // MÓDULO 8: além do player (enfileiramento pós-débito), a thread também
    // recebe o estado de navegação e a fila de capas — necessários para
    // recarregar o catálogo público após alternar o bloqueio de um gênero.
    {
        let player_tx = player_cmd_tx.clone();
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let cover_tx = cover_cmd_tx.clone();

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
                            genre: String::new(), // não usado no caminho do play
                        };

                        // MÓDULO 8 — Débito atômico pelo preço VIGENTE (lido do
                        // banco na hora: fonte única de verdade, válida também
                        // se o operador acabou de mudar o preço no submenu)
                        let price = db.get_song_price().unwrap_or(1).max(1);

                        match db.spend_credits(price) {
                            Ok(Some(new_total)) => {
                                log::info!(
                                    "Créditos debitados: -{} (saldo: {}). Enfileirando '{}'.",
                                    price,
                                    new_total,
                                    track.title
                                );
                                update_credits_ui(&ui_handle, new_total);
                                if let Err(e) = player_tx.send(PlayerCommand::Enqueue(track)) {
                                    log::error!("Falha ao enviar faixa ao player: {}", e);
                                }
                            }
                            Ok(None) => {
                                log::warn!(
                                    "Play negado: créditos insuficientes (preço: {}).",
                                    price
                                );
                                show_toast(&ui_handle, "Créditos Insuficientes", 2);
                            }
                            Err(e) => {
                                log::error!("Falha no débito de crédito: {}", e);
                                show_toast(&ui_handle, "Erro no banco de créditos", 2);
                            }
                        }
                    }
                    DbCommand::SetVolume(volume) => {
                        if let Err(e) = db.set_config_i64(VOLUME_CONFIG_KEY, volume as i64) {
                            log::error!("Falha ao persistir o volume: {}", e);
                        }
                    }
                    DbCommand::QueryOperatorStats => {
                        // MÓDULO 8 — Painel do operador: odômetro, caixa parcial,
                        // preço vigente e a lista de gêneros para o submenu.
                        let partial = db.get_partial_coins().unwrap_or(0);
                        let absolute = db.get_absolute_coins().unwrap_or(0);
                        let price = db.get_song_price().unwrap_or(1);
                        let genres: Vec<GenreInfo> = db.get_genres().unwrap_or_default();
                        log::debug!(
                            "Menu do operador: odômetro={}, caixa parcial={}, preço={}, {} gênero(s).",
                            absolute,
                            partial,
                            price,
                            genres.len()
                        );

                        let ui_handle = ui_handle.clone();
                        let state_arc = state_arc.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_op_partial_coins(partial as i32);
                                ui.set_op_absolute_coins(absolute as i32);
                                ui.set_op_song_price(price as i32);

                                // Espelha no estado: o submenu de preço semeia
                                // sua edição com o valor vigente e o submenu de
                                // gêneros navega na lista recém-carregada
                                let mut st = lock_state(&state_arc);
                                st.song_price = price;
                                st.genres = genres;
                                mirror_nav(&ui, &st);
                            }
                        });
                    }
                    DbCommand::SetSongPrice(price) => {
                        match db.set_song_price(price) {
                            Ok(()) => {
                                log::info!("Preço da música atualizado: {} crédito(s).", price);
                                let ui_for_update = ui_handle.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_for_update.upgrade() {
                                        ui.set_op_song_price(price as i32);
                                    }
                                });
                                show_toast(
                                    &ui_handle,
                                    &format!(
                                        "Preço atualizado: {} crédito{}",
                                        price,
                                        if price == 1 { "" } else { "s" }
                                    ),
                                    1,
                                );
                            }
                            Err(e) => {
                                log::error!("Falha ao gravar o preço da música: {}", e);
                                show_toast(&ui_handle, "Erro ao salvar o preço", 2);
                            }
                        }
                    }
                    DbCommand::ToggleGenre(genre) => {
                        match db.toggle_genre_block(&genre) {
                            Ok(blocked) => {
                                log::info!(
                                    "Gênero '{}' {} pelo operador.",
                                    genre,
                                    if blocked { "BLOQUEADO" } else { "liberado" }
                                );
                                // Recarrega o catálogo público com o filtro
                                // aplicado — SEM expulsar o operador do submenu
                                // (a troca de linha do modelo é instantânea,
                                // sem animações — nota de performance Mód. 8)
                                let albums = db.get_albums().unwrap_or_else(|e| {
                                    log::error!("Falha ao recarregar catálogo: {}", e);
                                    Vec::new()
                                });
                                publish_albums(&ui_handle, &state_arc, &cover_tx, albums, true);
                            }
                            Err(e) => {
                                log::error!("Falha ao alternar bloqueio do gênero: {}", e);
                                show_toast(&ui_handle, "Erro ao bloquear gênero", 2);
                            }
                        }
                    }
                    DbCommand::ResetPartial => match db.reset_partial_coins() {
                        Ok(()) => {
                            log::info!("Caixa parcial zerado pelo operador (odômetro intacto).");
                            let ui_for_update = ui_handle.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_for_update.upgrade() {
                                    ui.set_op_partial_coins(0);
                                }
                            });
                            show_toast(&ui_handle, "Caixa parcial zerado", 1);
                        }
                        Err(e) => {
                            log::error!("Falha ao zerar o caixa parcial: {}", e);
                            show_toast(&ui_handle, "Erro ao zerar caixa", 2);
                        }
                    },
                    DbCommand::ResetCredits => match db.reset_current_credits() {
                        Ok(()) => {
                            log::info!("Créditos atuais zerados pelo operador.");
                            update_credits_ui(&ui_handle, 0);
                            show_toast(&ui_handle, "Créditos atuais zerados", 1);
                        }
                        Err(e) => {
                            log::error!("Falha ao zerar os créditos atuais: {}", e);
                            show_toast(&ui_handle, "Erro ao zerar créditos", 2);
                        }
                    },
                }
            }
        });
    }

    // =========================================================================
    // 5. Thread do Scanner inicial (Módulo 2 + agrupamento por álbum do 7)
    // =========================================================================
    {
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let cover_tx = cover_cmd_tx.clone();

        thread::spawn(move || {
            log::info!("Thread do scanner de mídia iniciada.");
            let mut scanner_db = match Database::open() {
                Ok(database) => database,
                Err(err) => {
                    log::error!("Scanner: falha ao abrir conexão: {}", err);
                    return;
                }
            };

            let _ = scanner::scan_media_directory(&mut scanner_db);

            // Catálogo já sai agrupado por (artista, álbum) — pronto para o
            // carrossel de capas do Módulo 7
            let albums = match scanner_db.get_albums() {
                Ok(albums) => albums,
                Err(err) => {
                    log::error!("Scanner: falha ao agrupar catálogo por álbum: {}", err);
                    Vec::new()
                }
            };

            publish_albums(&ui_handle, &state_arc, &cover_tx, albums, false);
        });
    }

    // =========================================================================
    // 6. Player GStreamer (Módulo 3) + bridge de eventos → UI
    // =========================================================================
    player::spawn(player_cmd_rx, player_event_tx);

    // Volume inicial aplicado assim que o player sobe (o playbin mantém o
    // volume entre faixas — basta definir uma vez)
    let _ = player_cmd_tx.send(PlayerCommand::SetVolume(volume_to_linear(initial_volume)));

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
    // 7. Sincronização USB (Módulo 4 + ForceSync do 7) + bridge → UI
    // =========================================================================
    usb_sync::spawn(usb_cmd_rx, usb_event_tx);

    {
        let ui_handle = main_window.as_weak();
        let player_tx = player_cmd_tx.clone();
        let state_arc = state_arc.clone();
        let cover_tx = cover_cmd_tx.clone();

        thread::spawn(move || {
            while let Ok(event) = usb_event_rx.recv() {
                match event {
                    UsbSyncEvent::Started { total } => {
                        // O vídeo XVideo pinta ACIMA do desenho do Slint:
                        // escondê-lo é pré-requisito para o popup ficar visível
                        let _ = player_tx.send(PlayerCommand::HideVideo);
                        let ui_handle = ui_handle.clone();
                        let state_arc = state_arc.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_usb_overlay_visible(true);
                                ui.set_usb_state(1);
                                ui.set_usb_total(total as i32);
                                ui.set_usb_copied(0);
                                ui.set_usb_new_tracks(0);

                                // A sincronização toma a tela: se o operador
                                // estava com volume/menu abertos, fecha (MÓDULO 8:
                                // vale para o menu principal E para os submenus)
                                let mut st = lock_state(&state_arc);
                                if st.focus == FocusState::VolumeControl
                                    || st.focus.is_operator()
                                {
                                    st.focus = FocusState::BrowsingAlbums;
                                }
                                mirror_nav(&ui, &st);
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
                    UsbSyncEvent::Finished { copied, skipped, albums } => {
                        log::info!(
                            "UI: sync USB concluído ({} copiados, {} pulados).",
                            copied,
                            skipped
                        );

                        let track_count = albums.iter().map(|a| a.tracks.len()).sum::<usize>();

                        // Catálogo agrupado atualizado + reset da navegação
                        publish_albums(&ui_handle, &state_arc, &cover_tx, albums, false);

                        let ui_close = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_close.upgrade() {
                                ui.set_usb_state(2);
                                ui.set_usb_copied(copied as i32);
                                ui.set_usb_new_tracks(track_count as i32);
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
                    UsbSyncEvent::NoMedia { reason } => {
                        // "Forçar Sincronização" sem pendrive: só um toast
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            show_toast(&ui_handle, &reason, 2);
                        });
                    }
                }
            }
        });
    }

    // =========================================================================
    // 8. Capas de álbum (Módulo 7) + PIX (Módulo 5) — bridges → UI
    // =========================================================================
    covers::spawn(cover_cmd_rx, cover_event_tx);

    {
        let ui_handle = main_window.as_weak();
        thread::spawn(move || {
            while let Ok(event) = cover_event_rx.recv() {
                let CoverEvent::Ready { key, art } = event;
                let ui_handle = ui_handle.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        apply_album_cover(&ui, &key, art);
                    }
                });
            }
        });
    }

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
    // 9. Callbacks da UI — orquestração da máquina de estados (Módulo 7)
    // =========================================================================

    // ---- Teclado arcade (E/R/I/W/Q/O/U/P/Z/X, case-insensitive) ----
    // Único ponto de entrada: a máquina de estados decide a transição, este
    // handler espelha o resultado na UI e despacha os efeitos colaterais.
    {
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let db_tx = db_tx.clone();
        let player_tx = player_cmd_tx.clone();
        let usb_tx = usb_cmd_tx.clone();

        main_window.on_arcade_key_pressed(move |key: slint::SharedString| -> bool {
            let Some(ui) = ui_handle.upgrade() else { return false };

            let action = {
                let mut st = lock_state(&state_arc);
                st.handle_key(key.as_str())
            };

            handle_ui_action(&ui, &state_arc, action, &db_tx, &player_tx, &usb_tx)
        });
    }

    // ---- Clique/toque numa capa do carrossel (equivale à tecla I) ----
    {
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let db_tx = db_tx.clone();
        let player_tx = player_cmd_tx.clone();
        let usb_tx = usb_cmd_tx.clone();

        main_window.on_album_activated(move |index: i32| {
            let Some(ui) = ui_handle.upgrade() else { return };

            let action = {
                let mut st = lock_state(&state_arc);
                if st.focus == FocusState::BrowsingAlbums && !st.albums.is_empty() {
                    st.album_index = (index.max(0) as usize).min(st.albums.len() - 1);
                    st.open_current_album();
                }
                Some(Action::Noop)
            };

            handle_ui_action(&ui, &state_arc, action, &db_tx, &player_tx, &usb_tx);
        });
    }

    // ---- Clique/toque numa faixa do álbum aberto (equivale à tecla O) ----
    {
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let db_tx = db_tx.clone();
        let player_tx = player_cmd_tx.clone();
        let usb_tx = usb_cmd_tx.clone();

        main_window.on_track_activated(move |index: i32| {
            let Some(ui) = ui_handle.upgrade() else { return };

            let action = {
                let mut st = lock_state(&state_arc);
                if st.focus == FocusState::BrowsingTracks && st.track_count() > 0 {
                    st.track_index = (index.max(0) as usize).min(st.track_count() - 1);
                    st.current_album()
                        .and_then(|album| album.tracks.get(st.track_index))
                        .cloned()
                        .map(Action::PlayTrack)
                } else {
                    None
                }
            };

            handle_ui_action(&ui, &state_arc, action, &db_tx, &player_tx, &usb_tx);
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
// MÓDULO 7 — Orquestração da máquina de estados (executada no event loop)
// =============================================================================

/// Bloqueia o estado compartilhado. À prova de envenenamento: se outra
/// thread entrar em pânico segurando o lock, recuperamos o conteúdo —
/// um jukebox de bar não pode travar por causa de um estado inconsistente.
fn lock_state(state_arc: &Arc<Mutex<AppState>>) -> std::sync::MutexGuard<'_, AppState> {
    state_arc.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Espelha TODO o estado de navegação nas propriedades do Slint.
/// Chamado após cada tecla/clique processado e a cada publicação de
/// catálogo — o Slint é renderizador puro dessa única fonte de verdade.
fn mirror_nav(ui: &MainWindow, st: &AppState) {
    ui.set_ui_focus(st.focus.as_i32());
    ui.set_selected_album(st.album_index as i32);
    ui.set_selected_track(st.track_index as i32);
    ui.set_volume_value(st.volume as i32);
    ui.set_op_menu_index(st.menu_index as i32);

    // MÓDULO 8 — Espelhos dos submenus do operador
    ui.set_op_price_edit(st.price_value as i32);
    ui.set_op_genre_index(st.genre_index as i32);
    if st.focus == FocusState::OperatorGenreMenu {
        let rows: Vec<GenreData> = st
            .genres
            .iter()
            .map(|g| GenreData {
                name: g.name.clone().into(),
                blocked: g.blocked,
                count: g.track_count as i32,
            })
            .collect();
        ui.set_op_genres(ModelRc::new(VecModel::from(rows)));
    }

    if let Some(album) = st.current_album() {
        ui.set_current_album_title(album.title.clone().into());
        ui.set_current_album_artist(album.artist.clone().into());
        ui.set_current_album_count(album.tracks.len() as i32);

        // Painel de faixas visível: publica as faixas do disco aberto
        if st.focus == FocusState::BrowsingTracks {
            let rows: Vec<TrackData> = album.tracks.iter().map(track_info_to_data).collect();
            ui.set_current_album_tracks(ModelRc::new(VecModel::from(rows)));
        }
    } else {
        ui.set_current_album_title("".into());
        ui.set_current_album_artist("".into());
        ui.set_current_album_count(0);
    }
}

/// Executa os efeitos de uma ação da máquina de estados e devolve se a
/// tecla foi consumida (o FocusScope do Slint usa isso para accept/reject).
fn handle_ui_action(
    ui: &MainWindow,
    state_arc: &Arc<Mutex<AppState>>,
    action: Option<Action>,
    db_tx: &mpsc::Sender<DbCommand>,
    player_tx: &mpsc::Sender<PlayerCommand>,
    usb_tx: &mpsc::Sender<UsbSyncCommand>,
) -> bool {
    let Some(action) = action else { return false };

    // Primeiro espelha o estado pós-tecla (uma única fonte de verdade)
    {
        let st = lock_state(state_arc);
        mirror_nav(ui, &st);
    }

    match action {
        Action::Noop => {}
        Action::AddCredit => {
            log::debug!("Evento de moeda/tecla 'Z' detectado pela interface.");
            if let Err(e) = db_tx.send(DbCommand::AddCredit(1)) {
                log::error!("Erro ao enviar comando de moeda para a fila: {}", e);
            }
        }
        Action::PlayTrack(track) => {
            if let Err(e) = db_tx.send(DbCommand::RequestPlay(track_info_to_data(&track))) {
                log::error!("Erro ao enviar faixa para débito: {}", e);
            }
        }
        Action::VolumeChanged(volume) => {
            let _ = player_tx.send(PlayerCommand::SetVolume(volume_to_linear(volume)));
        }
        Action::VolumeClosed(volume) => {
            // Aplica no player e persiste no banco (sobrevive ao reboot)
            let _ = player_tx.send(PlayerCommand::SetVolume(volume_to_linear(volume)));
            let _ = db_tx.send(DbCommand::SetVolume(volume));
        }
        Action::OpenOperatorMenu => {
            // IP calculado na hora (dhcp pode mudar entre aberturas do menu)
            ui.set_op_ip(query_local_ip().into());
            // MÓDULO 8 — Stats do operador (odômetro, caixa parcial, preço) e
            // lista de gêneros vêm do banco pela thread de persistência
            let _ = db_tx.send(DbCommand::QueryOperatorStats);
            // O diálogo cobre a área do vídeo XVideo: esconde o vídeo para
            // o menu ficar visível (mesma regra do overlay USB)
            let _ = player_tx.send(PlayerCommand::HideVideo);
        }
        Action::CloseOperatorMenu => {
            let _ = player_tx.send(PlayerCommand::RestoreVideo);
        }
        Action::ForceSync => {
            // Restaura o vídeo (o menu fechou); se houver pendrive, o overlay
            // de sincronização abre em seguida e esconde o vídeo de novo
            let _ = player_tx.send(PlayerCommand::RestoreVideo);
            if let Err(e) = usb_tx.send(UsbSyncCommand::ForceSync) {
                log::error!("Erro ao solicitar sincronização forçada: {}", e);
            }
        }

        // ---- MÓDULO 8 — Submenus e operações do operador ----
        Action::OpenPriceMenu => {
            // A máquina já semeou price_value com o preço vigente;
            // o mirror_nav publica o valor no submenu. Nenhum I/O.
        }
        Action::PriceSaved(price) => {
            // Persiste o novo preço (a thread do banco confirma com toast)
            if let Err(e) = db_tx.send(DbCommand::SetSongPrice(price)) {
                log::error!("Erro ao enviar novo preço ao banco: {}", e);
            }
        }
        Action::OpenGenreMenu => {
            // Gêneros pré-carregados na abertura do menu (QueryOperatorStats);
            // o mirror_nav publica a lista no submenu. Nenhum I/O aqui.
        }
        Action::CloseGenreMenu => {
            // Continua dentro das telas do operador: o vídeo permanece
            // oculto até o U final do menu principal
        }
        Action::ToggleGenreBlock(genre) => {
            // Grava o bloqueio e recarrega o catálogo público (a thread do
            // banco chama publish_albums com foco preservado)
            if let Err(e) = db_tx.send(DbCommand::ToggleGenre(genre)) {
                log::error!("Erro ao enviar bloqueio de gênero ao banco: {}", e);
            }
        }
        Action::ResetPartialCoins => {
            if let Err(e) = db_tx.send(DbCommand::ResetPartial) {
                log::error!("Erro ao enviar zeramento do caixa parcial: {}", e);
            }
        }
        Action::ResetCredits => {
            if let Err(e) = db_tx.send(DbCommand::ResetCredits) {
                log::error!("Erro ao enviar zeramento de créditos: {}", e);
            }
        }
        Action::PowerOff => {
            // A distro concede sudo sem senha ao usuário jukebox
            // (/etc/sudoers.d/jukebox) — o systemctl desliga a máquina de
            // forma limpa (unmount do overlay, sync do disco)
            log::info!("Operador solicitou o desligamento da máquina.");
            match std::process::Command::new("sudo")
                .arg("systemctl")
                .arg("poweroff")
                .spawn()
            {
                Ok(child) => {
                    log::info!("Comando de poweroff disparado (pid {}).", child.id());
                    show_toast(&ui.as_weak(), "Desligando a máquina...", 0);
                }
                Err(e) => {
                    log::error!("Falha ao disparar o poweroff: {}", e);
                    show_toast(&ui.as_weak(), "Não foi possível desligar", 2);
                }
            }
        }
    }

    true
}

// =============================================================================
// Publicação do catálogo e das capas
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

/// Converte um AlbumInfo (banco) em AlbumData (modelo Slint) — estrutura
/// aninhada: cada álbum carrega seu próprio VecModel de faixas
fn album_info_to_data(a: &AlbumInfo) -> AlbumData {
    let tracks: Vec<TrackData> = a.tracks.iter().map(track_info_to_data).collect();
    AlbumData {
        key: a.key.clone().into(),
        title: a.title.clone().into(),
        artist: a.artist.clone().into(),
        initial: a.initial.clone().into(),
        palette: a.palette as i32,
        has_cover: false,
        cover: slint::Image::default(),
        tracks: ModelRc::new(VecModel::from(tracks)),
    }
}

/// Publica o catálogo agrupado na UI (thread-safe: agenda no event loop),
/// reseta a navegação da máquina de estados e dispara a varredura de capas.
/// Chamada pelo scanner inicial, após cada sincronização USB (reset_nav =
/// false) e ao alternar o bloqueio de um gênero no menu do operador
/// (reset_nav = true → MÓDULO 8: preserva o foco para não expulsar o
/// operador do submenu de gêneros enquanto ele trabalha na lista).
fn publish_albums(
    ui_handle: &slint::Weak<MainWindow>,
    state_arc: &Arc<Mutex<AppState>>,
    cover_tx: &mpsc::Sender<CoverCommand>,
    albums: Vec<AlbumInfo>,
    keep_focus: bool,
) {
    let album_count = albums.len();
    // Cópia para o serviço de capas (o original vai para o estado)
    let scan_list = albums.clone();

    let ui_handle = ui_handle.clone();
    let state_arc = state_arc.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_handle.upgrade() else { return };

        let rows: Vec<AlbumData> = {
            let mut st = lock_state(&state_arc);
            if keep_focus {
                st.replace_catalog_keep_focus(albums);
            } else {
                st.replace_catalog(albums);
            }
            let rows = st.albums.iter().map(album_info_to_data).collect();
            // Limpa o painel de faixas do disco anterior
            ui.set_current_album_tracks(ModelRc::new(VecModel::from(Vec::<TrackData>::new())));
            mirror_nav(&ui, &st);
            rows
        };

        ui.set_albums(ModelRc::new(VecModel::from(rows)));
        ui.set_scanning(false);
        log::info!("Catálogo publicado na interface: {} álbuns.", album_count);
    });

    // A fila do event loop preserva a ordem: esta varredura roda DEPOIS da
    // publicação acima, então capas reemitidas encontram o modelo novo.
    // (Discos ainda sem capa na memória são extraídos e cacheados em disco.)
    if let Err(e) = cover_tx.send(CoverCommand::Scan(scan_list)) {
        log::error!("Falha ao solicitar varredura de capas: {}", e);
    }
}

/// Aplica uma capa pronta (RGB cru) na linha do álbum correspondente do
/// modelo atual da UI. Executada sempre dentro do event loop — a textura
/// do Slint não pode nascer em outra thread.
fn apply_album_cover(ui: &MainWindow, key: &str, art: CoverArt) {
    let model = ui.get_albums();
    let row_count = model.iter().count();

    for row in 0..row_count {
        if let Some(mut data) = model.row_data(row) {
            if data.key.as_str() == key {
                data.cover = rgb_buffer_to_image(art.rgb, art.width, art.height);
                data.has_cover = true;
                if let Some(vec_model) = model.as_any().downcast_ref::<VecModel<AlbumData>>() {
                    vec_model.set_row_data(row, data);
                }
                return;
            }
        }
    }
    // Álbuns do scan antigo que já saíram do catálogo: ignora silenciosamente
}

// =============================================================================
// Utilidades de UI (executadas dentro do event loop do Slint)
// =============================================================================

/// Converte 0..=100 (barra da UI) para o volume linear do playbin com curva
/// CÚBICA: a percepção humana de loudness é logarítmica — sem a curva, os
/// 30% iniciais da barra seriam quase inaudíveis e o resto explodiria.
fn volume_to_linear(volume: u32) -> f64 {
    let fraction = (volume.min(100) as f64) / 100.0;
    fraction * fraction * fraction
}

/// IP atual da máquina via `hostname -I` (primeiro endereço listado).
/// Roda no keypress da abertura do menu — alguns milissegundos apenas.
fn query_local_ip() -> String {
    match std::process::Command::new("hostname").arg("-I").output() {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            match text.split_whitespace().next() {
                Some(ip) if !ip.is_empty() => ip.to_string(),
                _ => "IP indisponível".to_string(),
            }
        }
        _ => "IP indisponível".to_string(),
    }
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
/// MÓDULO 8: a pintura das propriedades agora ocorre DENTRO do event loop —
/// chamadas vindas de outras threads (ex.: débito negado na thread do banco)
/// antes eram silenciosamente descartadas pelo upgrade() fora da thread da UI.
fn show_toast(ui_handle: &slint::Weak<MainWindow>, message: &str, kind: i32) {
    let generation = TOAST_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let message = message.to_string();

    let ui_for_show = ui_handle.clone();
    let queued = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_for_show.upgrade() else { return };
        ui.set_toast_message(message.into());
        ui.set_toast_kind(kind);
        ui.set_toast_visible(true);
    });

    if queued.is_err() {
        return; // event loop já encerrou — nada a fazer
    }

    let ui_for_hide = ui_handle.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(3500));
        let ui_for_hide = ui_for_hide.clone();
        let _ = slint::invoke_from_event_loop(move || {
            // Somente esconde se nenhum toast mais novo foi exibido
            if TOAST_GENERATION.load(Ordering::SeqCst) == generation {
                if let Some(ui) = ui_for_hide.upgrade() {
                    ui.set_toast_visible(false);
                }
            }
        });
    });
}

/// Converte um buffer RGB (QR do PIX, capas de álbum) em textura do Slint
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

