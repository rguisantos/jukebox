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
    Enqueue(TrackInfo),
    SetVolume(f64),
    SkipTrack,
    CreditFx,
    Activity,
    Operator(bool),
    ReloadSettings,
    // Older dialogs still emit these; Slint now handles visibility without stopping video.
    HideVideo,
    RestoreVideo,
}

pub enum PlayerEvent {
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
fn credit_effect(freq: u32) -> Result<gst::Element, String> {
    if std::env::var("JUKEBOX_AUDIO_SINK").as_deref() == Ok("fakesink") {
        gst::parse::launch(&format!(
            "audiotestsrc wave=sine freq={freq} num-buffers=3 samplesperbuffer=1024 volume=0.12 \
             ! audioconvert ! audioresample ! queue ! fakesink sync=true"
        ))
        .map_err(|e| e.to_string())
    } else {
        gst::parse::launch(&format!(
            "audiotestsrc wave=sine freq={freq} num-buffers=3 samplesperbuffer=1024 volume=0.12 \
             ! audioconvert ! audioresample ! alsasink device=default"
        ))
        .map_err(|e| e.to_string())
    }
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
    /// Primeira nota do arpejo de crédito (C5 — 523Hz).
    effect_lo: gst::Element,
    /// Segunda nota do arpejo (C6 — 1046Hz), disparada ~75ms depois.
    effect_hi: gst::Element,
    /// Deadline da segunda nota; `None` quando nenhum arpejo está em curso.
    fx_hi_at: Option<Instant>,
    db: Database,
    queue: VecDeque<(i64, TrackInfo)>,
    current: Option<TrackInfo>,
    current_id: Option<i64>,
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
}
impl Player {
    fn new(tx: Sender<PlayerEvent>) -> Result<Self, String> {
        let music = element("playbin")?;
        music.set_property("video-sink", video_sink(1)?);
        music.set_property("audio-sink", audio_sink()?);
        let background = element("playbin")?;
        background.set_property("video-sink", video_sink(2)?);
        background.set_property("audio-sink", element("fakesink")?);
        background.set_property("mute", true);
        // Built-in two-note arcade arpeggio, no external file or license dependency.
        let effect_lo = credit_effect(FX_NOTE_LO_HZ)?;
        let effect_hi = credit_effect(FX_NOTE_HI_HZ)?;
        let db = Database::open().map_err(|e| e.to_string())?;
        let settings = db.settings().map_err(|e| e.to_string())?;
        let queue = db.pending_tracks().map_err(|e| e.to_string())?.into();
        let mut p = Self {
            music,
            background,
            effect_lo,
            effect_hi,
            fx_hi_at: None,
            db,
            queue,
            current: None,
            current_id: None,
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
        self.backgrounds = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && matches!(
                        p.extension().and_then(|s| s.to_str()),
                        Some("mp4" | "mpeg" | "wmv")
                    )
            })
            .collect();
        self.failed_backgrounds.clear();
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
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(cmd) => self.command(cmd),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                _ => {}
            }
            // Segunda nota do arpejo de crédito: dispara no deadline agendado.
            // O poll de 25ms acrescenta no máximo um ciclo de atraso — o ouvido
            // percebe apenas um arpejo contínuo de duas notas.
            if let Some(deadline) = self.fx_hi_at {
                if Instant::now() >= deadline {
                    self.fx_hi_at = None;
                    let _ = self.effect_hi.set_state(gst::State::Null);
                    let _ = self.effect_hi.set_state(gst::State::Playing);
                }
            }
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
            for pipe in [&self.effect_lo, &self.effect_hi] {
                if let Some(bus) = pipe.bus() {
                    if bus
                        .pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error])
                        .is_some()
                    {
                        let _ = pipe.set_state(gst::State::Null);
                    }
                }
            }
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
        for pipe in [&self.music, &self.background, &self.effect_lo, &self.effect_hi] {
            let _ = pipe.set_state(gst::State::Null);
        }
    }
    fn command(&mut self, cmd: PlayerCommand) {
        match cmd {
            PlayerCommand::Enqueue(t) => {
                self.last_activity = Instant::now();
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
            PlayerCommand::CreditFx => {
                // Arpejo ascendente de duas notas (C5 → C6): a segunda é
                // agendada ~75ms depois; o dmix mistura as notas com a
                // música sem interromper a reprodução em andamento.
                let _ = self.effect_lo.set_state(gst::State::Null);
                let _ = self.effect_lo.set_state(gst::State::Playing);
                self.fx_hi_at = Some(Instant::now() + FX_NOTE_GAP);
            }
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
            PlayerCommand::HideVideo | PlayerCommand::RestoreVideo => {}
        }
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
    fn finish(&mut self, refund: bool) {
        if let Some(id) = self.current_id {
            if let Err(e) = self.db.finish_track(id, refund) {
                let _ = self.music.set_state(gst::State::Null);
                let _ = self.background.set_state(gst::State::Null);
                SOURCE.store(0, Ordering::Relaxed);
                clear_frame();
                let _ = self.tx.send(PlayerEvent::Visual(false));
                let _ = self.tx.send(PlayerEvent::Error {
                    context: "Persistência da fila".into(),
                    detail: e.to_string(),
                });
                return;
            }
            let _ = self.tx.send(PlayerEvent::CreditsChanged);
        }
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
    #[test]
    fn composed_video_is_bounded_and_secondary_effect_does_not_pause_audio() {
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
        // As duas notas do arpejo de crédito com a topologia real (C5 → C6):
        // cada uma encerra sozinha (EOS) sem pausar a música — as rotas de
        // áudio seguem independentes, exatamente como no dmix do appliance.
        let lo = gst::parse::launch(
            "audiotestsrc wave=sine freq=523 num-buffers=3 samplesperbuffer=1024 volume=0.12 \
             ! audioconvert ! audioresample ! queue ! fakesink sync=true",
        )
        .unwrap();
        let hi = gst::parse::launch(
            "audiotestsrc wave=sine freq=1046 num-buffers=3 samplesperbuffer=1024 volume=0.12 \
             ! audioconvert ! audioresample ! queue ! fakesink sync=true",
        )
        .unwrap();
        music.set_state(gst::State::Playing).unwrap();
        lo.set_state(gst::State::Playing).unwrap();
        hi.set_state(gst::State::Playing).unwrap();
        for note in [&lo, &hi] {
            let msg = note
                .bus()
                .unwrap()
                .timed_pop_filtered(
                    gst::ClockTime::from_seconds(3),
                    &[gst::MessageType::Eos, gst::MessageType::Error],
                )
                .unwrap();
            assert!(matches!(msg.view(), gst::MessageView::Eos(_)));
            note.set_state(gst::State::Null).unwrap();
        }
        assert_eq!(music.current_state(), gst::State::Playing);
        music.set_state(gst::State::Null).unwrap();
    }
}
