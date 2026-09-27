//! Catalog presentation, cover viewport and Slint model projection.
//! All model mutations happen on the Slint event loop.
use crate::{lock_state, mirror_nav, rgb_buffer_to_image, AlbumData, MainWindow, TrackData};
use crate::media::covers::{self, CoverCommand, CoverEvent};
use crate::state::models::{AlbumInfo, AppState, CoverArt, TrackInfo};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::{thread, time::Duration};

static CATALOG_GENERATION: AtomicU64 = AtomicU64::new(0);
static ALBUM_ROW_INDEX: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();

pub(crate) fn install_cover_handlers(
    main_window: &MainWindow,
    state_arc: &Arc<Mutex<AppState>>,
    cover_event_rx: Receiver<CoverEvent>,
    cover_cmd_tx: Sender<CoverCommand>,
) -> slint::Timer {
    {
        let ui_handle = main_window.as_weak();
        thread::spawn(move || {
            while let Ok(event) = cover_event_rx.recv() {
                let CoverEvent::Ready {
                    generation,
                    key,
                    art,
                } = event;
                let ui_handle = ui_handle.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_handle.upgrade() {
                        if covers::GENERATION.load(Ordering::Relaxed) == generation {
                            apply_album_cover(&ui, &key, art);
                        }
                    }
                });
            }
        });
    }

    // 49 covers max in the UI (~9.2 MiB RGB), independent of catalog size.
    let cover_timer = slint::Timer::default();
    {
        let ui_handle = main_window.as_weak();
        let state = state_arc.clone();
        let tx = cover_cmd_tx.clone();
        let mut previous = None;
        let mut previous_window: Option<(usize, usize, u64)> = None;
        cover_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(150),
            move || {
                let Some(ui) = ui_handle.upgrade() else {
                    return;
                };
                let st = lock_state(&state);
                let signature = (st.album_index, CATALOG_GENERATION.load(Ordering::Relaxed));
                if previous == Some(signature) {
                    return;
                }
                previous = Some(signature);
                let start = st.album_index.saturating_sub(24);
                let end = (st.album_index + 25).min(st.albums.len());
                let model = ui.get_albums();
                if let Some((old_start, old_end, old_generation)) = previous_window {
                    if old_generation == signature.1 {
                        if let Some(rows) = model.as_any().downcast_ref::<VecModel<AlbumData>>() {
                            for i in old_start..old_end {
                                if (i < start || i >= end) && i < rows.row_count() {
                                    if let Some(mut row) = rows.row_data(i) {
                                        if row.has_cover {
                                            row.cover = slint::Image::default();
                                            row.has_cover = false;
                                            rows.set_row_data(i, row);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                previous_window = Some((start, end, signature.1));
                let generation = covers::GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
                let mut albums: Vec<_> = st.albums[start.min(end)..end]
                    .iter()
                    .map(|a| AlbumInfo {
                        key: a.key.clone(),
                        title: a.title.clone(),
                        artist: a.artist.clone(),
                        genre: a.genre.clone(),
                        initial: a.initial.clone(),
                        palette: a.palette,
                        is_recent: a.is_recent,
                        tracks: a.tracks.clone(),
                    })
                    .collect();
                // Center first, then nearby albums. Only the nearby slice is cloned.
                albums.sort_by_key(|a| {
                    if st.current_album().map(|v| &v.key) == Some(&a.key) {
                        0
                    } else {
                        1
                    }
                });
                let _ = tx.send(CoverCommand::Scan { generation, albums });
            },
        );
    }

    cover_timer
}

/// Converte um TrackInfo (banco) em TrackData (modelo Slint)
pub(crate) fn track_info_to_data(t: &TrackInfo) -> TrackData {
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
    // Tracks are materialized only for the currently open album.
    let tracks: Vec<TrackData> = Vec::new();
    AlbumData {
        key: a.key.clone().into(),
        title: a.title.clone().into(),
        artist: a.artist.clone().into(),
        genre: a.genre.clone().into(),
        initial: a.initial.clone().into(),
        palette: a.palette as i32,
        is_recent: a.is_recent,
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
pub(crate) fn publish_albums(
    ui_handle: &slint::Weak<MainWindow>,
    state_arc: &Arc<Mutex<AppState>>,
    cover_tx: &Sender<CoverCommand>,
    albums: Vec<AlbumInfo>,
    keep_focus: bool,
) {
    let album_count = albums.len();
    // Cópia para o serviço de capas (o original vai para o estado)
    let _ = cover_tx;

    let ui_handle = ui_handle.clone();
    let state_arc = state_arc.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(ui) = ui_handle.upgrade() else {
            return;
        };

        let rows: Vec<AlbumData> = {
            let mut st = lock_state(&state_arc);
            let mut albums = albums;
            if !st.active_genre.is_empty() {
                albums.retain(|a| a.genre == st.active_genre);
            }
            if keep_focus || st.focus.is_operator() {
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

        let mut index = std::collections::HashMap::with_capacity(rows.len());
        for (row_number, row) in rows.iter().enumerate() {
            index.entry(row.key.to_string()).or_insert(row_number);
        }
        *ALBUM_ROW_INDEX.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = index;
        ui.set_albums(ModelRc::new(VecModel::from(rows)));
        covers::GENERATION.fetch_add(1, Ordering::Relaxed);
        CATALOG_GENERATION.fetch_add(1, Ordering::Relaxed);
        ui.set_scanning(false);
        log::info!("Catálogo publicado na interface: {} álbuns.", album_count);
    });
}

/// Aplica uma capa pronta (RGB cru) na linha do álbum correspondente do
/// modelo atual da UI. Executada sempre dentro do event loop — a textura
/// do Slint não pode nascer em outra thread.
fn apply_album_cover(ui: &MainWindow, key: &str, art: CoverArt) {
    let model = ui.get_albums();
    let index = ALBUM_ROW_INDEX.get().and_then(|map| map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(key).copied());
    if let Some(index) = index {
        if let Some(mut data) = model.row_data(index) {
            if data.key.as_str() == key {
                data.cover = rgb_buffer_to_image(art.rgb, art.width, art.height);
                data.has_cover = true;
                if let Some(vec_model) = model.as_any().downcast_ref::<VecModel<AlbumData>>() {
                    vec_model.set_row_data(index, data);
                }
                return;
            }
        }
    }
    // Álbuns do scan antigo que já saíram do catálogo: ignora silenciosamente
}
