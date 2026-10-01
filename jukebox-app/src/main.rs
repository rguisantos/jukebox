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
//!   [Banco SQLite] — créditos: CashPulse/AcceptPix, RequestPlay
//!      (débito do preço vigente), SetVolume (persistente), stats do
//!      operador (Módulo 7) e contadores antifraude/preço/gêneros (Módulo 8)
//!      │ (débito atômico → Enqueue no player)
//!      ▼
//!   [Player GStreamer] — playbin/appsink/alsasink + fila (Módulo 3)
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

mod catalog_ui;
mod db;
mod finance;
mod legacy_keys;
mod media;
mod operator;
mod settings;
mod storage;
mod state;

use db::Database;
use finance::pix::{PixConfig, PixService, PixUiEvent};
use finance::pixlogic;
use media::covers::{self, CoverCommand, CoverEvent};
use catalog_ui::{publish_albums, track_info_to_data};
use media::player::{self, PlayerCommand, PlayerEvent};
use media::scanner;
use media::usb_sync::{self, UsbSyncCommand, UsbSyncEvent};
use storage::service::{self, DbEvent, DbHandle};
use slint::{ComponentHandle, ModelRc, VecModel};
use state::models::{
    Action, AppState, FocusState, VOLUME_DEFAULT,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(feature = "legacy-pg")]
use db::legacy_pg::client::LegacyDb;
#[cfg(feature = "legacy-pg")]
use db::legacy_pg::PgConfig;
#[cfg(feature = "legacy-pg")]
use storage::legacy_service;

// Carrega as structs geradas a partir do arquivo ui/app_window.slint
slint::include_modules!();

/// Geração do toast atual (evita que um timer antigo apague um toast novo)
static WIFI_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ONLINE_TX: std::sync::OnceLock<mpsc::Sender<()>> = std::sync::OnceLock::new();
static TOAST_GENERATION: AtomicU64 = AtomicU64::new(0);
const VOLUME_CONFIG_KEY: &str = "volume";

/// Mapa de teclas físicas da máquina (colunas `codtecla*` lidas do
/// `sistema` no boot do perfil legacy). Vazio nos demais perfis — o
/// callback de teclado aplica identidade.
static LEGACY_KEYMAP: std::sync::OnceLock<legacy_keys::RuntimeKeyMap> =
    std::sync::OnceLock::new();

/// Perfil legacy (fase 1 — Jukebox TV na base antiga): o app assume o
/// banco PostgreSQL `jukeboxtvdb` em vez do SQLite. Exige o binário
/// construído com a feature `legacy-pg` e `JUKEBOX_PROFILE=legacy` no
/// ambiente (launcher da distro antiga).
fn legacy_profile_requested() -> bool {
    #[cfg(feature = "legacy-pg")]
    {
        std::env::var("JUKEBOX_PROFILE").as_deref() == Ok("legacy")
    }
    #[cfg(not(feature = "legacy-pg"))]
    {
        false
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Iniciando Jukebox Arcade OS...");

    let legacy = legacy_profile_requested();
    if legacy {
        log::info!(
            "Perfil LEGACY ativo (fase 1): PostgreSQL jukeboxtvdb + árvore de mídia original"
        );
    }

    // =========================================================================
    // 1. Banco de dados e estado inicial
    // =========================================================================
    // Perfil modern: SQLite em /dados. Perfil legacy: PostgreSQL
    // `jukeboxtvdb` (conecta antes da janela — sem banco não há app, como
    // no Java original) + serviço de capas do banco.
    let mut sqlite: Option<Database> = None;
    #[cfg(feature = "legacy-pg")]
    let mut legacy_db: Option<LegacyDb> = None;
    #[cfg(feature = "legacy-pg")]
    if legacy {
        let config = PgConfig::from_env();
        legacy_db = Some(LegacyDb::connect(&config).map_err(|e| {
            log::error!("Perfil legacy: {e}");
            e
        })?);
        if let Err(e) = storage::legacy_covers::spawn(&config) {
            // Não derruba o app: o pipeline cai para capa de pasta/APIC.
            log::warn!("Capas do banco indisponíveis: {e}");
        }
    }

    let (initial_settings, initial_credits, initial_price, initial_volume, initial_recent_days) =
        if legacy {
            #[cfg(feature = "legacy-pg")]
            {
                let boot = legacy_service::boot_state(legacy_db.as_mut().expect("conectado"))?;
                LEGACY_KEYMAP.set(boot.keys.runtime_keymap()).ok();
                log::info!(
                    "Perfil legacy: saldo {} (preço {} crédito(s), volume {}%, \
                     lançamentos {}d, teclas da máquina carregadas)",
                    boot.credits,
                    boot.price,
                    boot.volume,
                    boot.recent_days
                );
                (boot.settings, boot.credits, boot.price, boot.volume, boot.recent_days)
            }
            #[cfg(not(feature = "legacy-pg"))]
            {
                unreachable!("perfil legacy exige a feature legacy-pg")
            }
        } else {
            let db = match Database::open() {
                Ok(database) => database,
                Err(err) => {
                    log::error!("Erro crítico ao inicializar o banco de dados: {}", err);
                    return Err(Box::new(err));
                }
            };

            let initial_settings = db.settings()?;
            let initial_credits = db.get_credits().unwrap_or(0);
            log::info!(
                "Créditos persistentes carregados do banco: {}",
                initial_credits
            );

            // MÓDULO 8 — Preço da música (créditos por reprodução), persistido no
            // banco: o bar ajusta uma vez e sobrevive a todos os ciclos de energia
            let initial_price = db.get_song_price().unwrap_or(1);
            log::info!(
                "Preço da música carregado do banco: {} crédito(s)",
                initial_price
            );

            // Volume persistido na tabela chave-valor (Módulo 7): o bar ajusta uma
            // vez e o valor sobrevive a todos os ciclos de energia da máquina
            let initial_volume = db
                .get_config_i64(VOLUME_CONFIG_KEY)
                .ok()
                .flatten()
                .map(|v| v.clamp(0, 100) as u32)
                .unwrap_or(VOLUME_DEFAULT);
            log::info!("Volume persistido carregado do banco: {}%", initial_volume);

            let initial_recent_days = db.get_recent_days().unwrap_or(30);
            log::info!(
                "Dias recém-adicionados carregados do banco: {}",
                initial_recent_days
            );

            sqlite = Some(db);
            (initial_settings, initial_credits, initial_price, initial_volume, initial_recent_days)
        };

    // =========================================================================
    // 2. Interface Slint + máquina de estados de foco (Módulo 7)
    // =========================================================================
    let main_window = MainWindow::new()?;
    main_window.set_credits(initial_credits as i32);
    main_window.set_free_play(initial_settings.free_play);
    main_window.set_config_data(operator::form(&initial_settings));
    main_window.set_volume_value(initial_volume as i32);
    main_window.set_op_song_price(initial_price as i32);
    main_window.set_op_recent_days(initial_recent_days as i32);
    main_window.set_scanning(true);
    // Perfil legacy: o Enter chega cru para o remapeamento das teclas da
    // máquina (evita colidir com as teclas I/O físicas).
    if legacy {
        main_window.set_pass_through_enter(true);
    }

    // Estado de navegação compartilhado: callbacks da UI (event loop) e
    // bridges de publicação de catálogo — Arc<Mutex> atravessa as threads.
    let state_arc: Arc<Mutex<AppState>> =
        Arc::new(Mutex::new(AppState::new(initial_volume, initial_price)));
    {
        let mut st = lock_state(&state_arc);
        st.recent_days = initial_recent_days;
        st.recent_days_value = initial_recent_days;
    }

    // =========================================================================
    // 3. Canais de comunicação entre as threads
    // =========================================================================
    let (player_cmd_tx, player_cmd_rx) = mpsc::channel::<PlayerCommand>();
    let (player_event_tx, player_event_rx) = mpsc::channel::<PlayerEvent>();
    let (usb_cmd_tx, usb_cmd_rx) = mpsc::channel::<UsbSyncCommand>();
    let (usb_event_tx, usb_event_rx) = mpsc::channel::<UsbSyncEvent>();
    let (pix_event_tx, pix_event_rx) = mpsc::channel::<PixUiEvent>();
    let (cover_cmd_tx, cover_cmd_rx) = mpsc::channel::<CoverCommand>();
    let (cover_event_tx, cover_event_rx) = mpsc::channel::<CoverEvent>();

    // =========================================================================
    // 4. Serviço de persistência (SQLite no perfil modern, jukeboxtvdb no
    //    legacy) e sua ponte de eventos → UI/player
    // =========================================================================
    #[cfg(feature = "legacy-pg")]
    let (db_tx, db_events) = if let Some(db) = legacy_db {
        legacy_service::spawn(db)
    } else {
        service::spawn(sqlite.expect("SQLite aberto na inicialização"))
    };
    #[cfg(not(feature = "legacy-pg"))]
    let (db_tx, db_events) = service::spawn(sqlite.expect("SQLite aberto na inicialização"));
    {
        let weak = main_window.as_weak();
        let state = state_arc.clone();
        let covers = cover_cmd_tx.clone();
        let player = player_cmd_tx.clone();
        thread::spawn(move || {
            while let Ok(event) = db_events.recv() {
                if let DbEvent::Enqueue(track) = event {
                    if let Err(e) = player.send(PlayerCommand::Enqueue(track)) {
                        log::error!("Player indisponível: {e}");
                        show_toast(&weak, "Player indisponível; nenhum crédito debitado", 2);
                    }
                    continue;
                }
                let weak = weak.clone();
                let state = state.clone();
                let covers = covers.clone();
                let player = player.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    match event {
                        DbEvent::Balance(balance) => ui.set_credits(balance as i32),
                        DbEvent::CreditAccepted { balance, added } => {
                            ui.set_credits(balance as i32);
                            credit_feedback(&weak, &player, added);
                        }
                        DbEvent::Toast { message, kind } => show_toast(&weak, &message, kind),
                        DbEvent::SettingsLoaded(settings) => {
                            ui.set_config_data(operator::form(&settings));
                        }
                        DbEvent::SettingsSaved(settings) => {
                            let _ = player.send(PlayerCommand::ReloadSettings);
                            ui.set_free_play(settings.free_play);
                            ui.set_config_data(operator::form(&settings));
                        }
                        DbEvent::GenreOptions(genres) => {
                            let mut st = lock_state(&state);
                            st.available_genres = genres;
                            st.selected_genre = st.available_genres.iter()
                                .position(|g| g == &st.active_genre).unwrap_or(0);
                            mirror_nav(&ui, &st);
                        }
                        DbEvent::Catalog(albums) => publish_albums(&weak, &state, &covers, albums, true),
                        DbEvent::OperatorStats { partial, absolute, revenue, price, recent_days, genres } => {
                            ui.set_op_partial_coins(partial as i32);
                            ui.set_op_absolute_coins(absolute as i32);
                            ui.set_op_total_revenue(revenue_label(revenue).into());
                            ui.set_op_song_price(price as i32);
                            ui.set_op_recent_days(recent_days as i32);
                            let mut st = lock_state(&state);
                            st.song_price = price;
                            st.recent_days = recent_days;
                            st.genres = genres;
                            mirror_nav(&ui, &st);
                        }
                        DbEvent::SongPrice(price) => ui.set_op_song_price(price as i32),
                        DbEvent::PartialReset => ui.set_op_partial_coins(0),
                        DbEvent::Enqueue(_) => unreachable!(),
                    }
                });
            }
        });
    }

    // =========================================================================
    // 5. Thread do Scanner inicial (Módulo 2 + agrupamento por álbum do 7)
    //    — perfil modern apenas: no legacy o catálogo vem do PostgreSQL
    //    (publicado pelo serviço na inicialização).
    // =========================================================================
    if !legacy {
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
    // 6. Player (Módulo 3) + bridge de eventos → UI
    //    — perfil modern: GStreamer; perfil legacy: player de validação
    //      (débito + fila no jukeboxtvdb; mídia chega com o libVLC).
    // =========================================================================
    #[cfg(feature = "legacy-pg")]
    if legacy {
        media::legacy_player::spawn(player_cmd_rx, player_event_tx, PgConfig::from_env());
    } else {
        player::spawn(player_cmd_rx, player_event_tx);
    }
    #[cfg(not(feature = "legacy-pg"))]
    player::spawn(player_cmd_rx, player_event_tx);

    // Volume inicial aplicado assim que o player sobe (o playbin mantém o
    // volume entre faixas — basta definir uma vez)
    let _ = player_cmd_tx.send(PlayerCommand::SetVolume(volume_to_linear(initial_volume)));

    {
        let ui_handle = main_window.as_weak();
        let state_arc = state_arc.clone();
        let balance_tx = db_tx.clone();
        let storage_covers = cover_cmd_tx.clone();
        thread::spawn(move || {
            while let Ok(event) = player_event_rx.recv() {
                if matches!(event, PlayerEvent::CreditsChanged) {
                    let _ = balance_tx.refresh_credits();
                    continue;
                }
                let ui_handle = ui_handle.clone();
                let state_arc = state_arc.clone();
                let storage_covers = storage_covers.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let ui = match ui_handle.upgrade() {
                        Some(ui) => ui,
                        None => return,
                    };
                    match event {
                        PlayerEvent::Storage { rows, catalog, message, restore } => {
                            {
                                let mut st = lock_state(&state_arc);
                                st.storage_albums = rows;
                                let storage_options: Vec<slint::SharedString> = st.storage_albums.iter().map(|album| {
                                    let status = if album.removed { "REMOVIDO · restaurar" } else if album.online { "ONLINE · remover" } else { "USB · remover" };
                                    format!("{} / {} · {} · {:.1} MiB · {}", album.artist, album.title, album.genre,
                                        album.size as f64 / 1_048_576.0, status).into()
                                }).chain(["Atualizar lista".into(), "Voltar".into()]).collect();
                                ui.set_storage_options(ModelRc::new(VecModel::from(storage_options)));
                                st.storage_index = st.storage_index.min(st.storage_albums.len() + 1);
                                st.storage_busy = false;
                                st.storage_message = message;
                                mirror_nav(&ui, &st);
                            }
                            if let Some(albums) = catalog {
                                publish_albums(&ui_handle, &state_arc, &storage_covers, albums, true);
                            }
                            if restore {
                                if let Some(tx) = ONLINE_TX.get() { let _ = tx.send(()); }
                            }
                        }
                        PlayerEvent::CreditsChanged => {}
                        PlayerEvent::Notice(message) => show_toast(&ui_handle, &message, 2),
                        PlayerEvent::Previous { title, artist } => {
                            ui.set_previous_title(title.into());
                            ui.set_previous_artist(artist.into());
                        }
                        PlayerEvent::Visual(active) => {
                            ui.set_video_available(active);
                            if !active {
                                ui.set_video_frame(slint::Image::default());
                            }
                        }
                        PlayerEvent::TrackStarted {
                            id,
                            title,
                            artist,
                            is_video,
                        } => {
                            lock_state(&state_arc).is_playing = true;
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
                            lock_state(&state_arc).is_playing = false;
                            ui.set_np_active(false);
                            ui.set_np_track_id(-1);
                            ui.set_np_title("".into());
                            ui.set_np_artist("".into());
                            ui.set_np_video(false);
                        }
                        PlayerEvent::Error { context, detail } => {
                            log::error!("UI: erro do player em '{}': {}", context, detail);
                            show_toast(&ui_handle, &format!("{}: {}", context, detail), 2);
                        }
                    }
                });
            }
        });
    }

    // =========================================================================
    // 7. Sincronização USB (Módulo 4 + ForceSync do 7) + bridge → UI
    //    — perfil modern apenas: no legacy o acervo vive na árvore
    //    original, gerenciado pelo importador do operador.
    // =========================================================================
    if !legacy {
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
                        // O overlay USB é composto sobre o vídeo pelo Slint.
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
                                if st.focus == FocusState::VolumeControl {
                                    st.focus = FocusState::BrowsingAlbums;
                                }
                                mirror_nav(&ui, &st);
                            }
                        });
                    }
                    UsbSyncEvent::Progress {
                        copied,
                        total,
                        current_file,
                    } => {
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
                    UsbSyncEvent::Finished {
                        copied,
                        skipped,
                        albums,
                    } => {
                        log::info!(
                            "UI: sync USB concluído ({} copiados, {} pulados).",
                            copied,
                            skipped
                        );

                        let track_count = albums.iter().map(|a| a.tracks.len()).sum::<usize>();
                        // Clipes importados para /dados/fundos entram em cena sem
                        // reiniciar a música nem esperar o próximo boot.
                        let _ = player_tx.send(PlayerCommand::RefreshBackgrounds);

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
                                    if ui.get_usb_overlay_visible() && ui.get_usb_state() == 2 {
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
    }

    // =========================================================================
    // 8. Capas de álbum (Módulo 7) + PIX (Módulo 5) — bridges → UI
    // =========================================================================
    covers::spawn(cover_cmd_rx, cover_event_tx);

    let _cover_timer = catalog_ui::install_cover_handlers(
        &main_window, &state_arc, cover_event_rx, cover_cmd_tx.clone()
    );

    // Perfil modern apenas: sincronização online do acervo. No legacy o
    // acervo é do importador original — ONLINE_TX fica desligado e o item
    // de menu apenas avisa.
    if !legacy {
        let (tx, rx) = mpsc::channel();
        let _ = ONLINE_TX.set(media::online_sync::spawn(tx));
        let ui_handle = main_window.as_weak();
        let state = state_arc.clone();
        let covers = cover_cmd_tx.clone();
        thread::spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    media::online_sync::OnlineEvent::Status(message) => {
                        let weak = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_online_status(message.into());
                            }
                        });
                    }
                    media::online_sync::OnlineEvent::Catalog(albums) => {
                        publish_albums(&ui_handle, &state, &covers, albums, true);
                    }
                }
            }
        });
    }

    let _legacy_pix = match pixlogic::Config::from_env() {
        _ if std::env::var("JUKEBOX_DATA_PERSISTENT").as_deref() == Ok("0") => {
            log::error!("Pix desativado: /dados não está montado como partição persistente");
            main_window.set_pixlogic_mode(true);
            main_window.set_pix_loading(false);
            main_window.set_pix_offline(true);
            main_window.set_pix_error_text("Armazenamento temporário: pagamentos desativados".into());
            main_window.set_pix_qr_error("Monte /dados para ativar pagamentos".into());
            None
        }
        Some(Ok(config)) => {
            main_window.set_pixlogic_mode(true);
            main_window.set_pix_loading(false);
            watch_pixlogic_qr(main_window.as_weak());
            pixlogic::spawn(config, db_tx.clone(), pix_event_tx);
            None
        }
        Some(Err(reason)) => {
            main_window.set_pixlogic_mode(true);
            main_window.set_pix_loading(false);
            main_window.set_pix_offline(true);
            main_window.set_pix_qr_error(reason.clone().into());
            main_window.set_pix_error_text(reason.into());
            None
        }
        None => Some(PixService::start(PixConfig::from_env(), pix_event_tx, db_tx.clone())),
    };

    {
        let ui_handle = main_window.as_weak();
        let db_tx = db_tx.clone();

        thread::spawn(move || {
            while let Ok(event) = pix_event_rx.recv() {
                match event {
                    PixUiEvent::PixLogicStatus { connected, message } => {
                        let weak = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_pix_offline(!connected);
                                ui.set_pix_error_text(message.into());
                            }
                        });
                    }
                    PixUiEvent::Loading => {
                        let ui_handle = ui_handle.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_handle.upgrade() {
                                ui.set_pix_loading(true);
                                ui.set_pix_offline(false);
                            }
                        });
                    }
                    PixUiEvent::QrReady {
                        rgb,
                        width,
                        height,
                        copia_cola,
                    } => {
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
                    PixUiEvent::Paid { machine_id, txid, credits } => {
                        // 1) Credita no banco (thread do SQLite)
                        if let Err(e) = db_tx.accept_pix(machine_id, txid, credits) {
                            log::error!("PIX: falha ao enviar crédito ao banco: {}", e);
                        }
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
            let Some(ui) = ui_handle.upgrade() else {
                return false;
            };

            // Perfil legacy: as teclas físicas desta máquina (codtecla*)
            // viram as canônicas do app. Mapa vazio = identidade (os
            // demais perfis não passam por aqui em nada).
            let key = match LEGACY_KEYMAP.get() {
                Some(keymap) => slint::SharedString::from(keymap.remap(key.as_str())),
                None => key,
            };

            let _ = player_tx.send(PlayerCommand::Activity);
            {
                let mut st = lock_state(&state_arc);
                if st.focus == FocusState::VolumeControl && !key.is_empty() {
                    st.last_overlay_interaction = std::time::Instant::now();
                }
                if st.focus != FocusState::VolumeControl
                    && matches!(
                        key.to_ascii_lowercase().as_str(),
                        "q" | "w" | "e" | "r" | "i" | "o" | "p"
                    )
                {
                    st.last_navigation = std::time::Instant::now();
                    st.fullscreen = false;
                    ui.set_video_fullscreen(false);
                }
            }
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
            let _ = player_tx.send(PlayerCommand::Activity);
            {
                let mut st = lock_state(&state_arc);
                st.last_navigation = std::time::Instant::now();
                st.fullscreen = false;
            }
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };

            let action = {
                let mut st = lock_state(&state_arc);
                if st.focus == FocusState::BrowsingAlbums && !st.albums.is_empty() {
                    st.album_index = (index.max(0) as usize).min(st.albums.len() - 1);
                    st.open_current_album();
                    st.last_overlay_interaction = std::time::Instant::now();
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
            let _ = player_tx.send(PlayerCommand::Activity);
            {
                let mut st = lock_state(&state_arc);
                st.last_navigation = std::time::Instant::now();
                st.fullscreen = false;
            }
            let Some(ui) = ui_handle.upgrade() else {
                return;
            };

            let action = {
                let mut st = lock_state(&state_arc);
                if st.focus == FocusState::BrowsingTracks && st.track_count() > 0 {
                    st.track_index = (index.max(0) as usize).min(st.track_count() - 1);
                    st.last_overlay_interaction = std::time::Instant::now();
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

    let media_timer = slint::Timer::default();
    {
        let weak = main_window.as_weak();
        let state = state_arc.clone();
        let db = db_tx.clone();
        media_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                let Some(ui) = weak.upgrade() else { return };
                if let Some(frame) = player::take_frame() {
                    ui.set_video_frame(rgb_buffer_to_image(frame.rgb, frame.width, frame.height));
                }
                let mut st = lock_state(&state);
                if let Some(action) = st.expire_idle_overlay(std::time::Instant::now()) {
                    mirror_nav(&ui, &st);
                    if let Action::VolumeClosed(volume) = action {
                        let _ = db.set_volume(volume);
                    }
                }
                if !st.focus.is_operator()
                    && st.focus != FocusState::VolumeControl
                    && st.focus != FocusState::GenrePicker
                    && !ui.get_usb_overlay_visible()
                {
                    if st.last_navigation.elapsed() >= Duration::from_secs(10)
                        && ui.get_video_available()
                    {
                        st.fullscreen = true;
                    }
                }
                ui.set_video_fullscreen(
                    st.fullscreen
                        && ui.get_video_available()
                        && !st.focus.is_operator()
                        && st.focus != FocusState::GenrePicker
                        && !ui.get_usb_overlay_visible(),
                );
            },
        );
    }
    // Aviso de disco cheio (/dados) — perfil modern apenas: a base antiga
    // não tem /dados e o acervo vive na árvore do importador.
    if !legacy {
        let weak = main_window.as_weak();
        thread::spawn(move || loop {
            let directory = scanner::resolve_media_dir();
            let directory = directory.parent().unwrap();
            let threshold = Database::open()
                .and_then(|db| db.settings())
                .map(|s| s.low_disk_mib as u64 * 1024 * 1024)
                .unwrap_or(1024 * 1024 * 1024);
            let result = settings::available_bytes(directory);
            let weak = weak.clone();
            if slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(bytes) => {
                            ui.set_disk_low(bytes < threshold);
                            ui.set_disk_status(
                                format!("Disco: {:.1} GiB livres", bytes as f64 / 1073741824.)
                                    .into(),
                            );
                        }
                        Err(_) => {
                            ui.set_disk_low(true);
                            ui.set_disk_status("Não foi possível consultar o disco".into());
                        }
                    }
                }
            })
            .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_secs(30));
        });
    }
    main_window.show()?;
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
pub(crate) fn lock_state(state_arc: &Arc<Mutex<AppState>>) -> std::sync::MutexGuard<'_, AppState> {
    state_arc
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Espelha TODO o estado de navegação nas propriedades do Slint.
/// Chamado após cada tecla/clique processado e a cada publicação de
/// catálogo — o Slint é renderizador puro dessa única fonte de verdade.
pub(crate) fn mirror_nav(ui: &MainWindow, st: &AppState) {
    ui.set_storage_index(st.storage_index as i32);
    ui.set_storage_busy(st.storage_busy);
    ui.set_storage_message(st.storage_message.clone().into());
    ui.set_storage_confirm_index(st.storage_confirm_action as i32);
    if let Some(album) = st.storage_albums.get(st.storage_index) {
        ui.set_storage_confirm_title(format!("{} / {}\n{} · {:.1} MiB", album.artist, album.title,
            album.genre, album.size as f64 / 1_048_576.0).into());
        ui.set_storage_confirm_action(if album.removed { "Restaurar" } else { "Remover desta máquina" }.into());
    }
    ui.set_ui_focus(st.focus.as_i32());
    ui.set_selected_album(st.album_index as i32);
    ui.set_selected_track(st.track_index as i32);
    ui.set_active_genre(if st.active_genre.is_empty() {
        "Todos os gêneros".into()
    } else {
        st.active_genre.clone().into()
    });
    ui.set_op_settings_index(st.settings_index as i32);
    ui.set_op_submenu_action(st.submenu_action as i32);
    ui.set_volume_value(st.volume as i32);
    ui.set_op_menu_index(st.menu_index as i32);

    // MÓDULO 8 — Espelhos dos submenus do operador
    ui.set_op_price_edit(st.price_value as i32);
    ui.set_op_recent_days_edit(st.recent_days_value as i32);
    ui.set_selected_letter_index(st.letter_index as i32);
    ui.set_selected_genre_index(st.selected_genre as i32);
    ui.set_genre_options(ModelRc::new(VecModel::from(st.available_genres.iter()
        .map(|g| if g.is_empty() { "Todos os gêneros".into() } else { g.clone().into() })
        .chain(std::iter::once("Voltar".into())).collect::<Vec<slint::SharedString>>() )));
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
        ui.set_current_album_genre(album.genre.clone().into());
        ui.set_current_album_count(album.tracks.len() as i32);

        // Painel de faixas visível: publica as faixas do disco aberto
        if st.focus == FocusState::BrowsingTracks {
            let rows: Vec<TrackData> = album.tracks.iter().map(track_info_to_data).collect();
            ui.set_current_album_tracks(ModelRc::new(VecModel::from(rows)));
        }
    } else {
        ui.set_current_album_title("".into());
        ui.set_current_album_artist("".into());
        ui.set_current_album_genre("".into());
        ui.set_current_album_count(0);
    }
}

/// Executa os efeitos de uma ação da máquina de estados e devolve se a
/// tecla foi consumida (o FocusScope do Slint usa isso para accept/reject).
fn handle_ui_action(
    ui: &MainWindow,
    state_arc: &Arc<Mutex<AppState>>,
    action: Option<Action>,
    db_tx: &DbHandle,
    player_tx: &mpsc::Sender<PlayerCommand>,
    usb_tx: &mpsc::Sender<UsbSyncCommand>,
) -> bool {
    let Some(action) = action else { return false };
    if let Action::ManageStorage(operation) = action {
        mirror_nav(ui, &lock_state(state_arc));
        if let Err(error) = player_tx.send(PlayerCommand::Storage(operation)) {
            let mut st = lock_state(state_arc);
            st.storage_busy = false;
            st.storage_message = format!("Player indisponível: {error}");
            mirror_nav(ui, &st);
        }
        return true;
    }

    // Primeiro espelha o estado pós-tecla (uma única fonte de verdade)
    {
        let st = lock_state(state_arc);
        mirror_nav(ui, &st);
    }

    match action {
        Action::ManageStorage(_) => unreachable!("storage action already handled"),
        Action::OpenWifi => {
            if !WIFI_OPEN.swap(true, Ordering::Relaxed) {
                let weak = ui.as_weak();
                thread::spawn(move || {
                    match std::process::Command::new("/usr/local/bin/jukebox-wifi").status() {
                        Ok(status) if status.success() => {}
                        result => {
                            log::warn!("Configuração Wi-Fi: {:?}", result);
                            show_toast(
                                &weak,
                                "Wi-Fi indisponível; verifique a instalação da distro",
                                3,
                            );
                        }
                    }
                    WIFI_OPEN.store(false, Ordering::Relaxed);
                });
            }
        }
        Action::SyncOnline => {
            if let Some(tx) = ONLINE_TX.get() {
                let _ = tx.send(());
            }
            show_toast(&ui.as_weak(), "Verificação do acervo solicitada", 2);
        }

        Action::OpenGenrePicker => { let _ = db_tx.load_genres(); }
        Action::SelectGenre => { let _ = db_tx.refresh_catalog(); }
        Action::OpenSettings => {
            let _ = db_tx.load_settings();
        }
        Action::SaveSettings => {
            let _ = db_tx.save_settings(operator::SettingsInput {
                base_cents: ui.get_cfg_base_cents().to_string(),
                base_credits: ui.get_cfg_base_credits().to_string(),
                pack_cents: ui.get_cfg_pack_cents().to_string(),
                pack_credits: ui.get_cfg_pack_credits().to_string(),
                large_cents: ui.get_cfg_large_cents().to_string(),
                large_credits: ui.get_cfg_large_credits().to_string(),
                coin_cents: ui.get_cfg_coin_cents().to_string(),
                attract_minutes: ui.get_cfg_attract_minutes().to_string(),
                low_disk_mib: ui.get_cfg_low_disk_mib().to_string(),
                free_play: ui.get_cfg_free_play(),
            });
        }
        Action::SettingsAdjusted { index, direction } => {
            adjust_settings(ui, index, direction);
        }
        Action::Noop => {}
        Action::AddCredit => {
            log::debug!("Evento de moeda/tecla 'Z' detectado pela interface.");
            if let Err(e) = db_tx.cash_pulse() {
                log::error!("Erro ao enviar comando de moeda para a fila: {}", e);
            }
        }
        Action::PlayTrack(track) => {
            if let Err(e) = db_tx.request_play(track) {
                log::error!("Erro ao enviar faixa para débito: {}", e);
            }
        }
        Action::VolumeChanged(volume) => {
            let _ = player_tx.send(PlayerCommand::SetVolume(volume_to_linear(volume)));
        }
        Action::VolumeClosed(volume) => {
            // Aplica no player e persiste no banco (sobrevive ao reboot)
            let _ = player_tx.send(PlayerCommand::SetVolume(volume_to_linear(volume)));
            let _ = db_tx.set_volume(volume);
        }
        Action::SkipTrack => {
            let _ = player_tx.send(PlayerCommand::SkipTrack);
        }
        Action::QuitApp => {
            log::info!("Tecla L detectada: encerrando o programa.");
            std::process::exit(0);
        }
        Action::OpenOperatorMenu => {
            let _ = db_tx.unlock_operator();
            let _ = player_tx.send(PlayerCommand::Operator(true));
            let _ = db_tx.load_settings();
            // IP calculado na hora (dhcp pode mudar entre aberturas do menu)
            ui.set_op_ip(query_local_ip().into());
            // MÓDULO 8 — Stats do operador (odômetro, caixa parcial, preço) e
            // lista de gêneros vêm do banco pela thread de persistência
            let _ = db_tx.operator_stats();
            // A camada administrativa permanece sobre o vídeo.
            let _ = player_tx.send(PlayerCommand::HideVideo);
        }
        Action::CloseOperatorMenu => {
            let _ = db_tx.lock_operator();
            let _ = player_tx.send(PlayerCommand::Operator(false));
            let _ = player_tx.send(PlayerCommand::RestoreVideo);
        }
        Action::ForceSync => {
            if legacy_profile_requested() {
                show_toast(
                    &ui.as_weak(),
                    "Perfil legacy: acervo gerenciado pelo importador original",
                    2,
                );
                return true;
            }
            // Sincroniza sem encerrar a sessão administrativa.
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
            if let Err(e) = db_tx.set_song_price(price) {
                log::error!("Erro ao enviar novo preço ao banco: {}", e);
            }
        }
        Action::OpenGenreMenu => {
            // Gêneros pré-carregados na abertura do menu (QueryOperatorStats);
            // o mirror_nav publica a lista no submenu. Nenhum I/O aqui.
        }
        Action::CloseGenreMenu => {
            // Continua dentro das telas do operador: o vídeo permanece
            // oculto até selecionar Voltar no menu principal
        }
        Action::ToggleGenreBlock(genre) => {
            // Grava o bloqueio e recarrega o catálogo público (a thread do
            // banco chama publish_albums com foco preservado)
            if let Err(e) = db_tx.toggle_genre(genre) {
                log::error!("Erro ao enviar bloqueio de gênero ao banco: {}", e);
            }
        }
        Action::ResetPartialCoins => {
            if let Err(e) = db_tx.reset_partial() {
                log::error!("Erro ao enviar zeramento do caixa parcial: {}", e);
            }
        }
        Action::ResetCredits => {
            if let Err(e) = db_tx.reset_credits() {
                log::error!("Erro ao enviar zeramento de créditos: {}", e);
            }
        }
        Action::OpenAlphabetPicker => {
            schedule_alphabet_auto_confirm(ui, state_arc);
        }
        Action::LetterMoved(_idx) => {
            schedule_alphabet_auto_confirm(ui, state_arc);
        }
        Action::ConfirmLetter(_idx) => {}
        Action::OpenRecentDaysMenu => {}
        Action::RecentDaysSaved(days) => {
            if let Err(e) = db_tx.set_recent_days(days) {
                log::error!(
                    "Erro ao enviar novos dias recém-adicionados ao banco: {}",
                    e
                );
            }
        }
        Action::PowerOff => {
            // Perfil modern: a distro concede sudo sem senha ao usuário
            // jukebox (/etc/sudoers.d/jukebox) — o systemctl desliga a
            // máquina de forma limpa (unmount do overlay, sync do disco).
            // Perfil legacy: base antiga sem systemd/sudo — o app roda como
            // root na instalação original; poweroff direto.
            log::info!("Operador solicitou o desligamento da máquina.");
            let mut command = if legacy_profile_requested() {
                std::process::Command::new("poweroff")
            } else {
                let mut command = std::process::Command::new("sudo");
                command.arg("systemctl").arg("poweroff");
                command
            };
            match command.spawn() {
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

fn adjust_settings(ui: &MainWindow, index: usize, direction: i32) {
    if index == 9 {
        ui.set_cfg_free_play(!ui.get_cfg_free_play());
        return;
    }
    let (current, step, lower, upper) = match index {
        0 => (ui.get_cfg_base_cents(), 100, 1, 100_000),
        1 => (ui.get_cfg_base_credits(), 1, 1, 10_000),
        2 => (ui.get_cfg_pack_cents(), 100, 1, 100_000),
        3 => (ui.get_cfg_pack_credits(), 1, 1, 10_000),
        4 => (ui.get_cfg_large_cents(), 100, 1, 100_000),
        5 => (ui.get_cfg_large_credits(), 1, 1, 10_000),
        6 => (ui.get_cfg_coin_cents(), 100, 1, 100_000),
        7 => (ui.get_cfg_attract_minutes(), 1, 0, 1440),
        8 => (ui.get_cfg_low_disk_mib(), 100, 1, 100_000),
        _ => return,
    };
    let value = current.as_str().parse::<i32>().unwrap_or(lower);
    let value = (value + step * direction).clamp(lower, upper).to_string().into();
    match index {
        0 => ui.set_cfg_base_cents(value),
        1 => ui.set_cfg_base_credits(value),
        2 => ui.set_cfg_pack_cents(value),
        3 => ui.set_cfg_pack_credits(value),
        4 => ui.set_cfg_large_cents(value),
        5 => ui.set_cfg_large_credits(value),
        6 => ui.set_cfg_coin_cents(value),
        7 => ui.set_cfg_attract_minutes(value),
        8 => ui.set_cfg_low_disk_mib(value),
        _ => {}
    }
}

// =============================================================================
// Publicação do catálogo e das capas
// =============================================================================

/// Timer de auto-confirmação da seleção alfabética (1.5s após a última navegação)
static ALPHABET_TIMER_GEN: AtomicU64 = AtomicU64::new(0);

fn schedule_alphabet_auto_confirm(ui: &MainWindow, state_arc: &Arc<Mutex<AppState>>) {
    let gen = ALPHABET_TIMER_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ui_weak = ui.as_weak();
    let state_arc = state_arc.clone();

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(1500));
        let ui_weak = ui_weak.clone();
        let state_arc = state_arc.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if ALPHABET_TIMER_GEN.load(Ordering::SeqCst) == gen {
                if let Some(ui) = ui_weak.upgrade() {
                    let mut st = lock_state(&state_arc);
                    if st.focus == FocusState::AlphabetPicker {
                        let idx = st.letter_index;
                        if let Some(&letter) = state::models::ALPHABET_ITEMS.get(idx) {
                            st.jump_to_letter(letter);
                        } else {
                            st.focus = FocusState::BrowsingAlbums;
                        }
                        mirror_nav(&ui, &st);
                    }
                }
            }
        });
    });
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

/// A failed financial query must never be displayed as a genuine zero.
fn revenue_label(result: Result<i64, String>) -> String {
    match result {
        Ok(cents) => format_brl(cents),
        Err(error) => {
            log::error!("Falha ao consultar recebimentos do moedeiro: {error}");
            "Indisponível".into()
        }
    }
}

/// Formata centavos como moeda brasileira (`R$ 1.234,56` — ponto como
/// separador de milhar, vírgula como decimal). Usado pelo odômetro de
/// receita do menu do operador; nunca usa ponto-flutuante para dinheiro.
fn format_brl(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let reais = abs / 100;
    let centavos = abs % 100;
    let digits = reais.to_string();
    let mut grouped: String = digits
        .chars()
        .rev()
        .enumerate()
        .flat_map(|(i, ch)| {
            // Insere um ponto a cada três dígitos (exceto antes do primeiro).
            let dot = if i > 0 && i % 3 == 0 { Some('.') } else { None };
            dot.into_iter().chain(std::iter::once(ch))
        })
        .collect();
    // `flat_map` montou a sequência já invertida — só reverter de volta.
    grouped = grouped.chars().rev().collect();
    format!("{sign}R$ {grouped},{centavos:02}")
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

/// Feedback is only scheduled after durable credit acceptance.
fn credit_feedback(
    weak: &slint::Weak<MainWindow>,
    player: &mpsc::Sender<PlayerCommand>,
    added: u32,
) {
    let _ = player.send(PlayerCommand::Activity);
    let _ = player.send(PlayerCommand::CreditFx);
    show_toast(
        weak,
        &if added > 0 {
            format!("Saldo recebido! +{added} crédito(s)")
        } else {
            "Saldo recebido; complete o valor do próximo crédito".into()
        },
        1,
    );
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
        let Some(ui) = ui_for_show.upgrade() else {
            return;
        };
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

fn pixlogic_qr_path() -> std::path::PathBuf {
    Database::resolve_db_path().parent().unwrap().join("pix/qr.png")
}

/// Observa o QR público sem bloquear o event loop. Um arquivo criado ou
/// substituído durante a execução aparece automaticamente na tela.
fn watch_pixlogic_qr(ui: slint::Weak<MainWindow>) {
    thread::Builder::new().name("pixlogic-qr".into()).spawn(move || {
        let path = pixlogic_qr_path();
        log::info!("QR PixLogic: lendo {}", path.display());
        let mut observed = None;
        let mut retry = true;
        let mut last_error = String::new();
        loop {
            let signature = std::fs::metadata(&path).ok()
                .map(|meta| (meta.len(), meta.modified().ok()));
            if retry || observed.as_ref() != Some(&signature) {
                observed = Some(signature);
                let result = load_pixlogic_qr(&path);
                retry = result.is_err();
                match result {
                    Ok((rgb, width, height)) => {
                        last_error.clear();
                        log::info!("QR PixLogic local carregado: {}x{}", width, height);
                        let weak = ui.clone();
                        if slint::invoke_from_event_loop(move || {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_pix_qr_image(rgb_buffer_to_image(rgb, width, height));
                                ui.set_pixlogic_has_qr(true);
                                ui.set_pix_qr_error("".into());
                            }
                        }).is_err() { return; }
                    }
                    Err(reason) => {
                        if reason != last_error {
                            log::warn!("QR PixLogic local: {reason}");
                            last_error = reason.clone();
                            let weak = ui.clone();
                            if slint::invoke_from_event_loop(move || {
                                if let Some(ui) = weak.upgrade() {
                                    ui.set_pixlogic_has_qr(false);
                                    ui.set_pix_qr_image(slint::Image::default());
                                    ui.set_pix_qr_error(reason.into());
                                }
                            }).is_err() { return; }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(5));
        }
    }).expect("thread do QR PixLogic");
}

/// Valida e redimensiona a imagem antes de enviá-la para a UI. O caminho é
/// público e não contém credenciais PixLogic.
fn load_pixlogic_qr(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() < 24 || bytes.len() > 8 * 1024 * 1024
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err("Esperado PNG válido de até 8 MiB (PDF precisa ser convertido)".into());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if !(128..=4096).contains(&width) || !(128..=4096).contains(&height) {
        return Err("QR PNG deve ter entre 128 e 4096 pixels por dimensão".into());
    }
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| format!("PNG inválido: {e}"))?;
    let rgba = decoded.to_rgba8();
    let (display_w, display_h, pixels) = if width.max(height) > 768 {
        let scale = 768f64 / f64::from(width.max(height));
        let w = (f64::from(width) * scale).round().max(1.0) as u32;
        let h = (f64::from(height) * scale).round().max(1.0) as u32;
        (w, h, image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Nearest))
    } else {
        (width, height, rgba)
    };
    let mut rgb = Vec::with_capacity((display_w as usize) * (display_h as usize) * 3);
    for pixel in pixels.pixels() {
        let alpha = u16::from(pixel[3]);
        for color in &pixel.0[..3] {
            rgb.push(((u16::from(*color) * alpha + 255 * (255 - alpha)) / 255) as u8);
        }
    }
    Ok((rgb, display_w, display_h))
}

/// Converte um buffer RGB (QR do PIX, capas de álbum) em textura do Slint
pub(crate) fn rgb_buffer_to_image(rgb: Vec<u8>, width: u32, height: u32) -> slint::Image {
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

#[cfg(test)]
mod brl_tests {
    use super::{format_brl, load_pixlogic_qr, pixlogic_qr_path, revenue_label};

    #[test]
    fn pixlogic_qr_uses_the_same_data_directory_as_the_database() {
        let db = crate::db::Database::resolve_db_path();
        let qr = pixlogic_qr_path();
        assert_eq!(qr, db.parent().unwrap().join("pix/qr.png"));
        assert!(qr.ends_with("pix/qr.png"));
        if db.starts_with("./dados") {
            assert_eq!(qr, std::path::PathBuf::from("./dados/pix/qr.png"));
        }
    }

    #[test]
    fn local_pix_qr_requires_a_readable_png() {
        let path = std::env::temp_dir().join(format!("jukebox-qr-{}-{}.png",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let pixels = image::RgbaImage::from_pixel(128,128,image::Rgba([0,0,0,255]));
        image::DynamicImage::ImageRgba8(pixels).save(&path).unwrap();
        let (_, width, height) = load_pixlogic_qr(&path).unwrap();
        assert_eq!((width, height), (128, 128));
        image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(1024,1024,image::Rgba([255,255,255,255]))
        ).save(&path).unwrap();
        let (rgb, width, height) = load_pixlogic_qr(&path).unwrap();
        assert_eq!((width, height), (768, 768));
        assert_eq!(rgb.len(), 768 * 768 * 3);
        std::fs::write(&path,b"PDF, not PNG").unwrap();
        assert!(load_pixlogic_qr(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_receipts_query_is_not_a_zero_balance() {
        assert_eq!(revenue_label(Ok(0)), "R$ 0,00");
        assert_eq!(revenue_label(Err("database unavailable".into())), "Indisponível");
    }

    #[test]
    fn formats_zero_and_small_amounts() {
        assert_eq!(format_brl(0), "R$ 0,00");
        assert_eq!(format_brl(1), "R$ 0,01");
        assert_eq!(format_brl(150), "R$ 1,50");
        assert_eq!(format_brl(16_050), "R$ 160,50");
    }

    #[test]
    fn groups_thousands_with_dots() {
        assert_eq!(format_brl(123_456), "R$ 1.234,56");
        assert_eq!(format_brl(1_000), "R$ 10,00");
        assert_eq!(format_brl(1_000_000), "R$ 10.000,00");
        assert_eq!(format_brl(9_999_999_999), "R$ 99.999.999,99");
    }

    #[test]
    fn negative_cents_keep_the_sign_before_the_currency() {
        assert_eq!(format_brl(-50), "-R$ 0,50");
    }
}
