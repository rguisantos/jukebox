//! Bounded video composition into Slint; independent foreground, background and FX pipelines.
use crate::{db::Database, state::models::TrackInfo};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app::{AppSink, AppSinkCallbacks};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc::{Receiver, Sender},
        Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Frame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
static FRAME: OnceLock<Mutex<Option<Frame>>> = OnceLock::new();
static SOURCE: AtomicU8 = AtomicU8::new(0);
pub fn take_frame() -> Option<Frame> {
    FRAME.get_or_init(|| Mutex::new(None)).lock().ok()?.take()
}
fn clear_frame() {
    if let Ok(mut frame) = FRAME.get_or_init(|| Mutex::new(None)).lock() {
        *frame = None;
    }
}

pub enum PlayerCommand {
    Storage(super::storage_management::StorageOperation),
    Enqueue(TrackInfo),
    SetVolume(f64),
    SkipTrack,
    CreditFx,
    Activity,
    Operator(bool),
    ReloadSettings,
    RefreshBackgrounds,
    // Older dialogs still emit these; Slint now handles visibility without stopping video.
    HideVideo,
    RestoreVideo,
}

pub enum PlayerEvent {
    Storage {
        rows: Vec<super::storage_management::StorageAlbum>,
        catalog: Option<Vec<crate::state::models::AlbumInfo>>,
        message: String,
        restore: bool,
    },
    CreditsChanged,
    TrackStarted {
        id: i64,
        title: String,
        artist: String,
        is_video: bool,
    },
    Previous {
        title: String,
        artist: String,
    },
    Visual(bool),
    QueueUpdated {
        upcoming: Vec<TrackInfo>,
    },
    QueueFinished,
    Error {
        context: String,
        detail: String,
    },
    Notice(String),
}

fn element(name: &str) -> Result<gst::Element, String> {
    gst::ElementFactory::make(name)
        .build()
        .map_err(|e| format!("{name}: {e}"))
}
fn video_sink(source: u8) -> Result<gst::Bin, String> {
    // One pending RGB frame; dropped frames never create an unbounded UI backlog.
    let sink = gst::parse::bin_from_description(
        "videoscale add-borders=true ! video/x-raw,width=640,height=360 ! videorate ! video/x-raw,framerate=20/1 ! videoconvert ! video/x-raw,format=RGB,pixel-aspect-ratio=1/1 ! appsink name=frames max-buffers=1 drop=true sync=true", true).map_err(|e|e.to_string())?;
    let app = sink
        .by_name("frames")
        .ok_or("appsink ausente")?
        .downcast::<AppSink>()
        .map_err(|_| "appsink inválido")?;
    app.set_callbacks(
        AppSinkCallbacks::builder()
            .new_sample(move |app| {
                let sample = app.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                if SOURCE.load(Ordering::Relaxed) != source {
                    return Ok(gst::FlowSuccess::Ok);
                }
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info = gstreamer_video::VideoInfo::from_caps(caps)
                    .map_err(|_| gst::FlowError::Error)?;
                let buffer = sample
                    .buffer()
                    .ok_or(gst::FlowError::Error)?
                    .map_readable()
                    .map_err(|_| gst::FlowError::Error)?;
                let width = info.width();
                let height = info.height();
                let stride = info.stride()[0] as usize;
                if stride < width as usize * 3 || buffer.len() < stride * height as usize {
                    return Err(gst::FlowError::Error);
                }
                let mut rgb = Vec::with_capacity((width * height * 3) as usize);
                for row in buffer.chunks(stride).take(height as usize) {
                    rgb.extend_from_slice(&row[..width as usize * 3]);
                }
                if let Ok(mut frame) = FRAME.get_or_init(|| Mutex::new(None)).lock() {
                    *frame = Some(Frame { rgb, width, height });
                }
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
    Ok(sink)
}
fn audio_sink() -> Result<gst::Element, String> {
    // dmix in the appliance allows music + effect without grabbing the device exclusively.
    if std::env::var("JUKEBOX_AUDIO_SINK").as_deref() == Ok("fakesink") {
        let sink = element("fakesink")?;
        sink.set_property("sync", true);
        return Ok(sink);
    }
    gst::ElementFactory::make("alsasink")
        .property("device", "default")
        .property("buffer-time", 400_000i64)
        .build()
        .map_err(|e| e.to_string())
}
fn uri(path: &Path) -> Result<String, String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    gst::glib::filename_to_uri(path, None)
        .map(|s| s.to_string())
        .map_err(|e| e.to_string())
}

/// Notas do aviso sonoro de crédito (arpejo ascendente estilo fliperama):
/// C5 → C6 (523Hz → 1046Hz). `num-buffers=3 × 1024` amostras ≈ 64–70ms por
/// nota conforme a taxa negociada; o par completa em ~140–150ms.
const FX_NOTE_LO_HZ: u32 = 523;
const FX_NOTE_HI_HZ: u32 = 1046;
/// Intervalo entre as duas notas do arpejo (~75ms por nota).
const FX_NOTE_GAP: Duration = Duration::from_millis(75);

/// Uma nota do arpejo de crédito: tom curto via `audiotestsrc`, misturado no
/// dmix junto com a música. Em teste (`JUKEBOX_AUDIO_SINK=fakesink`) usa a
/// mesma topologia com saída descartada para manter o comportamento medível.
fn credit_effect(freq: u32, fake: bool) -> Result<gst::Element, String> {
    let sink = if fake { "fakesink sync=true" } else { "alsasink device=default" };
    gst::parse::launch(&format!(
        "audiotestsrc wave=sine freq={freq} num-buffers=3 samplesperbuffer=1024 volume=0.12 \
         ! audioconvert ! audioresample ! {sink}"
    )).map_err(|e| e.to_string())
}

/// Shared by CreditFx and tests. Rapid receipts restart the complete pair:
/// an old high note is stopped and its deadline is replaced, never queued.
struct CreditArpeggio {
    lo: gst::Element,
    hi: gst::Element,
    hi_at: Option<Instant>,
}
impl CreditArpeggio {
    fn new(fake: bool) -> Result<Self, String> {
        Ok(Self { lo: credit_effect(FX_NOTE_LO_HZ, fake)?,
            hi: credit_effect(FX_NOTE_HI_HZ, fake)?, hi_at: None })
    }
    fn trigger(&mut self, now: Instant) {
        self.stop();
        if self.lo.set_state(gst::State::Playing).is_ok() {
            self.hi_at = Some(now + FX_NOTE_GAP);
        }
    }
    fn tick(&mut self, now: Instant) -> bool {
        let due = self.hi_at.map(|deadline| now >= deadline).unwrap_or(false);
        if due {
            self.hi_at = None;
            let _ = self.hi.set_state(gst::State::Playing);
        }
        due
    }
    fn drain(&self) {
        for pipe in [&self.lo, &self.hi] {
            if let Some(bus) = pipe.bus() {
                if bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error]).is_some() {
                    let _ = pipe.set_state(gst::State::Null);
                }
            }
        }
    }
    fn stop(&mut self) {
        self.hi_at = None;
        for pipe in [&self.lo, &self.hi] { let _ = pipe.set_state(gst::State::Null); }
    }
}
impl Drop for CreditArpeggio {
    fn drop(&mut self) { self.stop(); }
}

/// Lista clipes completos em /dados/fundos, inclusive subpastas, sem seguir
/// links simbólicos do pendrive. A ordem é estável; a escolha ocorre no player.
fn background_files(dir: &Path) -> Vec<PathBuf> {
    fn visit(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() { continue; }
            let path = entry.path();
            if kind.is_dir() {
                visit(&path, files);
            } else if kind.is_file()
                && path.extension().and_then(|ext| ext.to_str())
                    .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "mp4" | "mpeg" | "wmv"))
                    .unwrap_or(false)
            {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    visit(dir, &mut files);
    files.sort();
    files
}

pub fn spawn(rx: Receiver<PlayerCommand>, tx: Sender<PlayerEvent>) {
    thread::Builder::new()
        .name("jukebox-player".into())
        .spawn(move || {
            if let Err(e) = gst::init()
                .map_err(|e| e.to_string())
                .and_then(|_| Player::new(tx.clone()).map(|mut p| p.run(rx)))
            {
                let _ = tx.send(PlayerEvent::Error {
                    context: "Inicialização do player".into(),
                    detail: e,
                });
            }
        })
        .expect("player thread");
}
struct Player {
    music: gst::Element,
    background: gst::Element,
    effect: CreditArpeggio,
    db: Database,
    queue: VecDeque<(i64, TrackInfo)>,
    current: Option<TrackInfo>,
    current_id: Option<i64>,
    pending_finish: Option<(bool, Instant)>,
    settings: crate::settings::Settings,
    tx: Sender<PlayerEvent>,
    last_activity: Instant,
    last_auto: Instant,
    auto_retry: Instant,
    operator: bool,
    rng: u64,
    last_path: Option<String>,
    backgrounds: Vec<PathBuf>,
    failed_backgrounds: Vec<PathBuf>,
    current_background: Option<PathBuf>,
    storage_tx: Sender<PlayerEvent>,
    storage_rx: Receiver<PlayerEvent>,
    storage_pending: bool,
    removal_path: Option<PathBuf>,
}
impl Player {
    fn new(tx: Sender<PlayerEvent>) -> Result<Self, String> {
        Self::with_database(Database::open().map_err(|e| e.to_string())?, tx,
            std::env::var("JUKEBOX_AUDIO_SINK").as_deref() == Ok("fakesink"))
    }
    fn with_database(db: Database, tx: Sender<PlayerEvent>, fake: bool) -> Result<Self, String> {
        let music = element("playbin")?;
        music.set_property("video-sink", video_sink(1)?);
        music.set_property("audio-sink", if fake { element("fakesink")? } else { audio_sink()? });
        let background = element("playbin")?;
        background.set_property("video-sink", video_sink(2)?);
        background.set_property("audio-sink", element("fakesink")?);
        background.set_property("mute", true);
        // Built-in two-note arcade arpeggio, no external file or license dependency.
        let effect = CreditArpeggio::new(fake)?;
        let settings = db.settings().map_err(|e| e.to_string())?;
        let queue = db.pending_tracks().map_err(|e| e.to_string())?.into();
        let (storage_tx, storage_rx) = std::sync::mpsc::channel();
        let mut p = Self {
            music,
            background,
            effect,
            db,
            queue,
            current: None,
            current_id: None,
            pending_finish: None,
            settings,
            tx,
            last_activity: Instant::now(),
            last_auto: Instant::now(),
            auto_retry: Instant::now(),
            operator: false,
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64
                | 1,
            last_path: None,
            backgrounds: vec![],
            failed_backgrounds: vec![],
            current_background: None,
            storage_tx,
            storage_rx,
            storage_pending: false,
            removal_path: None,
        };
        p.refresh_backgrounds();
        Ok(p)
    }
    fn random(&mut self, len: usize) -> usize {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng as usize) % len.max(1)
    }
    fn refresh_backgrounds(&mut self) {
        let dir = super::scanner::resolve_media_dir()
            .parent()
            .unwrap()
            .join("fundos");
        self.backgrounds = background_files(&dir);
        // Compatibilidade: versões anteriores do importador USB colocavam
        // fundos dentro de musicas/fundos. Novos clipes vão para /dados/fundos.
        let legacy_dir = super::scanner::resolve_media_dir().join("fundos");
        self.backgrounds.extend(background_files(&legacy_dir).into_iter().filter(|path| {
            path.strip_prefix(&legacy_dir).ok()
                .map(|relative| !dir.join(relative).is_file())
                .unwrap_or(false)
        }));
        self.failed_backgrounds.clear();
        log::info!("Vídeos de fundo disponíveis: {}", self.backgrounds.len());
    }
    fn background_next(&mut self) {
        let _ = self.background.set_state(gst::State::Null);
        let mut files: Vec<_> = self
            .backgrounds
            .iter()
            .filter(|p| !self.failed_backgrounds.contains(p))
            .cloned()
            .collect();
        if files.len() > 1 {
            files.retain(|p| Some(p) != self.current_background.as_ref());
        }
        if files.is_empty() {
            SOURCE.store(0, Ordering::Relaxed);
            clear_frame();
            let _ = self.tx.send(PlayerEvent::Visual(false));
            return;
        }
        let index = self.random(files.len());
        let file = files[index].clone();
        if let Ok(uri) = uri(&file) {
            self.background.set_property("uri", uri);
            self.current_background = Some(file.clone());
            SOURCE.store(2, Ordering::Relaxed);
            if self.background.set_state(gst::State::Playing).is_ok() {
                let _ = self.tx.send(PlayerEvent::Visual(true));
            } else {
                self.failed_backgrounds.push(file);
                let _ = self.tx.send(PlayerEvent::Visual(false));
            }
        }
    }
    fn run(&mut self, rx: Receiver<PlayerCommand>) {
        self.emit_queue();
        loop {
            while let Ok(event) = self.storage_rx.try_recv() {
                self.storage_pending = false;
                self.removal_path = None;
                let _ = self.tx.send(event);
            }
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(cmd) => self.command(cmd),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
            // Nominal 75 ms; synchronous database/media work may delay this tick.
            self.effect.tick(Instant::now());
            // Service EOS/errors even under continuous key input.
            if let Some(bus) = self.music.bus() {
                while let Some(msg) =
                    bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])
                {
                    match msg.view() {
                        gst::MessageView::Eos(_) => {
                            self.finish(false);
                        }
                        gst::MessageView::Error(e) => {
                            let _ = self.tx.send(PlayerEvent::Error {
                                context: "Reprodução".into(),
                                detail: e.error().to_string(),
                            });
                            self.finish(true);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(bus) = self.background.bus() {
                while let Some(msg) =
                    bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])
                {
                    if matches!(msg.view(), gst::MessageView::Error(_)) {
                        if let Some(p) = self.current_background.take() {
                            self.failed_backgrounds.push(p);
                        }
                    }
                    if self
                        .current
                        .as_ref()
                        .map(|t| !t.is_video())
                        .unwrap_or(false)
                    {
                        self.background_next();
                    } else {
                        let _ = self.background.set_state(gst::State::Null);
                    }
                }
            }
            self.effect.drain();
            self.retry_finish();
            if self.current.is_none() && self.current_id.is_none() {
                if let Some((id, t)) = self.queue.pop_front() {
                    self.current_id = Some(id);
                    self.start(t);
                } else if Instant::now() >= self.auto_retry
                    && crate::settings::autoplay_due(
                        &self.settings,
                        self.operator,
                        self.last_activity.elapsed(),
                        self.last_auto.elapsed(),
                    )
                {
                    if self.last_auto.elapsed() >= Duration::from_millis(250) {
                        self.last_auto = Instant::now();
                        match self
                            .db
                            .random_track(self.last_path.as_deref())
                            .and_then(|track| {
                                if track.is_some() {
                                    Ok(track)
                                } else {
                                    self.db.random_track(None)
                                }
                            }) {
                            Ok(Some(t)) => self.start(t),
                            Ok(None) => {}
                            Err(e) => log::warn!("Attract: {e}"),
                        }
                    }
                }
            }
        }
        self.effect.stop();
        for pipe in [&self.music, &self.background] {
            let _ = pipe.set_state(gst::State::Null);
        }
    }
    fn command(&mut self, cmd: PlayerCommand) {
        match cmd {
            PlayerCommand::Storage(operation) => self.manage_storage(operation),
            PlayerCommand::Enqueue(t) => {
                self.last_activity = Instant::now();
                if let Some(folder) = &self.removal_path {
                    let path = std::env::current_dir().unwrap_or_default().join(&t.file_path);
                    if path.starts_with(folder) {
                        let _ = self.tx.send(PlayerEvent::Notice("Álbum em remoção; nenhum crédito debitado".into()));
                        return;
                    }
                }
                if !Path::new(&t.file_path).is_file() {
                    let _ = self.tx.send(PlayerEvent::Notice(
                        "Arquivo indisponível; nenhum crédito debitado".into(),
                    ));
                    return;
                }
                let tail = self
                    .queue
                    .back()
                    .map(|(_, t)| t.file_path.as_str())
                    .or_else(|| self.current.as_ref().map(|t| t.file_path.as_str()));
                if tail == Some(t.file_path.as_str()) {
                    let _ = self.tx.send(PlayerEvent::Notice(
                        "Esta música já é a última da fila".into(),
                    ));
                    return;
                }
                match self.db.reserve_track(&t) {
                    Ok(Some((id, _))) => {
                        self.queue.push_back((id, t));
                        let _ = self.tx.send(PlayerEvent::CreditsChanged);
                        if self.current.is_some() && self.current_id.is_none() {
                            self.finish(false);
                        }
                        self.emit_queue();
                    }
                    Ok(None) => {
                        let message = if !self.settings.free_play
                            && self.db.get_credits().unwrap_or(0)
                                < self.db.get_song_price().unwrap_or(1)
                        {
                            "Créditos insuficientes. Insira saldo para escolher uma música."
                        } else {
                            "Fila cheia ou música repetida; nenhum crédito debitado"
                        };
                        let _ = self.tx.send(PlayerEvent::Notice(message.into()));
                    }
                    Err(e) => {
                        let _ = self.tx.send(PlayerEvent::Error {
                            context: "Compra".into(),
                            detail: e.to_string(),
                        });
                    }
                }
            }
            PlayerCommand::SetVolume(v) => {
                self.music.set_property("volume", v.clamp(0., 1.));
            }
            PlayerCommand::SkipTrack => {
                self.last_activity = Instant::now();
                self.finish(false);
            }
            PlayerCommand::CreditFx => self.effect.trigger(Instant::now()),
            PlayerCommand::Activity => {
                self.last_activity = Instant::now();
            }
            PlayerCommand::Operator(open) => {
                self.operator = open;
                self.last_activity = Instant::now();
            }
            PlayerCommand::ReloadSettings => {
                if let Ok(s) = self.db.settings() {
                    self.settings = s;
                }
                self.refresh_backgrounds();
                self.last_auto = Instant::now();
            }
            PlayerCommand::RefreshBackgrounds => {
                self.refresh_backgrounds();
                if self.current.as_ref().map(|track| !track.is_video()).unwrap_or(false)
                    && SOURCE.load(Ordering::Relaxed) != 2
                {
                    self.background_next();
                }
            }
            PlayerCommand::HideVideo | PlayerCommand::RestoreVideo => {}
        }
    }
    fn manage_storage(&mut self, operation: super::storage_management::StorageOperation) {
        use super::storage_management::{self, StorageOperation};
        // Install a reservation gate on the player thread, then do filesystem
        // work off-thread so playback, EOS and credit effects keep progressing.
        if self.storage_pending { return; }
        if !self.operator {
            let _ = self.tx.send(PlayerEvent::Storage {
                rows: vec![], catalog: None, message: "Abra o menu do operador".into(), restore: false,
            });
            return;
        }
        let pending = match self.db.pending_tracks() {
            Ok(tracks) => tracks,
            Err(error) => {
                let _ = self.tx.send(PlayerEvent::Storage { rows: vec![], catalog: None,
                    message: format!("Não foi possível conferir a fila: {error}"), restore: false });
                return;
            }
        };
        let mut protected: Vec<String> = pending.into_iter().map(|(_, t)| t.file_path).collect();
        protected.extend(self.current.iter().map(|t| t.file_path.clone()));
        protected.extend(self.queue.iter().map(|(_, t)| t.file_path.clone()));
        if let StorageOperation::Remove(album) = &operation {
            if let Ok(cwd) = std::env::current_dir() {
                let media = super::scanner::resolve_media_dir();
                self.removal_path = Some(cwd.join(media.parent().unwrap()).join(&album.path));
            } else {
                let _ = self.tx.send(PlayerEvent::Storage { rows: vec![], catalog: None,
                    message: "Não foi possível conferir o caminho do álbum".into(), restore: false });
                return;
            }
        }
        self.storage_pending = true;
        let tx = self.storage_tx.clone();
        let media = super::scanner::resolve_media_dir();
        thread::spawn(move || {
            let result = storage_management::run(&operation, &protected);
            let (rows, message, restore, changed) = match result {
                Ok(result) => (result.rows, result.message, result.restore, result.changed),
                Err(error) => {
                    let rows = storage_management::run(&StorageOperation::List, &protected)
                        .map(|result| result.rows).unwrap_or_default();
                    (rows, error, false, matches!(operation, StorageOperation::Remove(_)))
                }
            };
            let catalog = if changed {
                match super::catalog_lock::CatalogLock::acquire(media.parent().unwrap()) {
                    Ok(_lock) => match Database::open() {
                        Ok(mut db) => {
                            super::scanner::scan_media_directory(&mut db);
                            db.get_albums().ok()
                        },
                        Err(error) => { log::error!("Armazenamento: erro do catálogo: {error}"); None },
                    }
                    Err(error) => { log::error!("Armazenamento: não foi possível reler catálogo: {error}"); None }
                }
            } else { None };
            let _ = tx.send(PlayerEvent::Storage { rows, catalog, message, restore });
        });
    }
    fn start(&mut self, t: TrackInfo) {
        let _ = self.music.set_state(gst::State::Null);
        let _ = self.background.set_state(gst::State::Null);
        clear_frame();
        let is_video = t.is_video();
        self.current = Some(t.clone());
        match uri(Path::new(&t.file_path)) {
            Ok(uri) => {
                self.music.set_property("uri", uri);
            }
            Err(e) => {
                let _ = self.tx.send(PlayerEvent::Error {
                    context: t.title.clone(),
                    detail: e,
                });
                self.finish(true);
                return;
            }
        }
        SOURCE.store(if is_video { 1 } else { 0 }, Ordering::Relaxed);
        if self.music.set_state(gst::State::Playing).is_err() {
            self.finish(true);
            return;
        }
        let _ = self.tx.send(PlayerEvent::TrackStarted {
            id: t.id,
            title: t.title,
            artist: t.artist,
            is_video,
        });
        if is_video {
            let _ = self.tx.send(PlayerEvent::Visual(true));
        } else {
            self.background_next();
        }
        self.emit_queue();
    }
    fn retry_finish(&mut self) {
        if let Some((refund, when)) = self.pending_finish {
            if Instant::now() >= when { self.finish(refund); }
        }
    }
    fn finish(&mut self, refund: bool) {
        // Preserve the first outcome: a later skip must not cancel an owed refund.
        let refund = if let Some((original, when)) = self.pending_finish {
            if Instant::now() < when { return; }
            original
        } else { refund };
        if let Some(id) = self.current_id {
            if let Err(e) = self.db.finish_track(id, refund) {
                let first_failure = self.pending_finish.is_none();
                self.pending_finish = Some((refund, Instant::now() + Duration::from_secs(2)));
                log::warn!("Finalização pendente da faixa {id}; nova tentativa em 2s: {e}");
                let _ = self.music.set_state(gst::State::Null);
                let _ = self.background.set_state(gst::State::Null);
                SOURCE.store(0, Ordering::Relaxed);
                clear_frame();
                let _ = self.tx.send(PlayerEvent::Visual(false));
                if first_failure {
                    let _ = self.tx.send(PlayerEvent::Error {
                        context: "Persistência da fila".into(),
                        detail: format!("{e}; tentando recuperar automaticamente"),
                    });
                }
                return;
            }
            let _ = self.tx.send(PlayerEvent::CreditsChanged);
        }
        self.pending_finish = None;
        if let Some(t) = self.current.take() {
            self.last_path = Some(t.file_path);
            if !refund {
                let _ = self.tx.send(PlayerEvent::Previous {
                    title: t.title,
                    artist: t.artist,
                });
            }
        }
        self.current_id = None;
        self.last_auto = Instant::now();
        if refund {
            self.auto_retry = Instant::now() + Duration::from_secs(5);
        }
        let _ = self.music.set_state(gst::State::Null);
        let _ = self.background.set_state(gst::State::Null);
        SOURCE.store(0, Ordering::Relaxed);
        clear_frame();
        let _ = self.tx.send(PlayerEvent::Visual(false));
        let _ = self.tx.send(PlayerEvent::QueueFinished);
        self.emit_queue();
    }
    fn emit_queue(&self) {
        let _ = self.tx.send(PlayerEvent::QueueUpdated {
            upcoming: self.queue.iter().map(|(_, t)| t.clone()).collect(),
        });
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;
    // Both tests touch the bounded global frame mailbox.
    static MEDIA_TEST: Mutex<()> = Mutex::new(());
    #[test]
    fn removal_gate_prevents_new_purchase_but_allows_other_albums() {
        let _guard = MEDIA_TEST.lock().unwrap();
        gst::init().unwrap();
        let folder = std::env::temp_dir().join(format!("jukebox-removal-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let album = folder.join("album");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("song.mp3"), b"audio").unwrap();
        std::fs::write(folder.join("other.mp3"), b"audio").unwrap();
        let mut db = Database::in_memory(); db.increment_credits(2).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut player = Player::with_database(db, tx, true).unwrap();
        player.removal_path = Some(album.clone());
        let mut track = TrackInfo { id: 1, title: "Test".into(), artist: "Ana".into(), album: "CD".into(),
            file_path: album.join("song.mp3").to_string_lossy().into(), file_type: "mp3".into(), genre: "Rock".into() };
        player.command(PlayerCommand::Enqueue(track.clone()));
        assert_eq!(player.db.get_credits().unwrap(), 2);
        assert!(player.db.pending_tracks().unwrap().is_empty());
        track.file_path = folder.join("other.mp3").to_string_lossy().into();
        player.command(PlayerCommand::Enqueue(track));
        assert_eq!(player.db.get_credits().unwrap(), 1);
        assert_eq!(player.queue.len(), 1);
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn background_folder_finds_only_video_in_nested_directories() {
        let dir = std::env::temp_dir().join(format!("jukebox-fundos-{}", std::process::id()));
        let sub = dir.join("festa");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("loop.MP4"), b"video").unwrap();
        std::fs::write(dir.join("01.mp3"), b"audio").unwrap();
        std::fs::write(dir.join(".copiando.usb-part"), b"partial").unwrap();
        assert_eq!(background_files(&dir), vec![sub.join("loop.MP4")]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn retries_failed_completion_and_refunds_exactly_once() {
        let _guard = MEDIA_TEST.lock().unwrap();
        gst::init().unwrap();
        let mut db = Database::in_memory();
        db.increment_credits(2).unwrap();
        let track = TrackInfo { id: 1, title: "Test".into(), artist: "Artist".into(),
            album: "Album".into(), file_path: "/test.mp3".into(), file_type: "mp3".into(), genre: "Rock".into() };
        let (id, _) = db.reserve_track(&track).unwrap().unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut player = Player::with_database(db, tx, true).unwrap();
        player.queue.clear();
        player.current_id = Some(id); player.current = Some(track);
        player.db.test_sql("CREATE TRIGGER fail_finish BEFORE DELETE ON playback_queue BEGIN SELECT RAISE(ABORT, 'temporary failure'); END;");
        player.finish(true);
        assert_eq!(player.db.get_credits().unwrap(), 1);
        assert_eq!(player.db.pending_tracks().unwrap().len(), 1);
        assert!(player.pending_finish.unwrap().0);
        player.command(PlayerCommand::SkipTrack);
        assert!(player.pending_finish.unwrap().0);
        player.db.test_sql("DROP TRIGGER fail_finish;");
        player.pending_finish.as_mut().unwrap().1 = Instant::now();
        player.retry_finish();
        assert_eq!(player.db.get_credits().unwrap(), 2);
        assert!(player.db.pending_tracks().unwrap().is_empty());
        assert!(player.current_id.is_none());
        assert!(player.pending_finish.is_none());
        player.retry_finish();
        assert_eq!(player.db.get_credits().unwrap(), 2);
    }

    #[test]
    fn composed_video_is_bounded_and_secondary_effect_does_not_pause_audio() {
        let _guard = MEDIA_TEST.lock().unwrap();
        gst::init().unwrap();
        SOURCE.store(1, Ordering::Relaxed);
        clear_frame();
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("videotestsrc")
            .property("num-buffers", 5i32)
            .build()
            .unwrap();
        let sink = video_sink(1).unwrap();
        pipeline.add_many([&source, sink.upcast_ref()]).unwrap();
        source.link(&sink).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        let message = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(5),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            )
            .expect("video pipeline timeout");
        assert!(
            matches!(message.view(), gst::MessageView::Eos(_)),
            "{message:?}"
        );
        let frame = take_frame().expect("RGB frame");
        assert_eq!((frame.width, frame.height), (640, 360));
        assert_eq!(frame.rgb.len(), 640 * 360 * 3);
        assert!(take_frame().is_none());
        pipeline.set_state(gst::State::Null).unwrap();
        let music = gst::parse::launch("audiotestsrc is-live=true ! fakesink sync=true").unwrap();
        let mut effect = CreditArpeggio::new(true).unwrap();
        music.set_state(gst::State::Playing).unwrap();
        let now = Instant::now();
        effect.trigger(now);
        assert_eq!(effect.hi.current_state(), gst::State::Null);
        assert!(!effect.tick(now + Duration::from_millis(49)));
        // Another receipt before the deadline restarts the pair.
        effect.trigger(now + Duration::from_millis(50));
        assert!(!effect.tick(now + Duration::from_millis(75)));
        assert!(!effect.tick(now + Duration::from_millis(124)));
        assert!(effect.tick(now + Duration::from_millis(125)));
        assert!(!effect.tick(now + Duration::from_millis(126)));
        // A receipt while the high note is active cancels that note as well.
        effect.trigger(now + Duration::from_millis(130));
        assert_eq!(effect.hi.current_state(), gst::State::Null);
        assert!(!effect.tick(now + Duration::from_millis(204)));
        assert!(effect.tick(now + Duration::from_millis(205)));
        for note in [&effect.lo, &effect.hi] {
            let msg = note.bus().unwrap().timed_pop_filtered(
                gst::ClockTime::from_seconds(3),
                &[gst::MessageType::Eos, gst::MessageType::Error],
            ).unwrap();
            assert!(matches!(msg.view(), gst::MessageView::Eos(_)));
        }
        assert_eq!(music.current_state(), gst::State::Playing);
        effect.stop();
        assert!(!effect.tick(now + Duration::from_secs(1)));
        music.set_state(gst::State::Null).unwrap();
    }
}
