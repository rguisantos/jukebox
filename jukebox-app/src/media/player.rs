//! MÓDULO 3 — Player de Mídia (GStreamer playbin + fila de reprodução)
//!
//! Arquitetura dedicada ao hardware legado (Sempron 145, single-core):
//!
//!   * **playbin** cuida de decodebin/decodificadores automaticamente —
//!     zero montagem de pipeline manual, menos superfície de erro;
//!   * **xvimagesink** com `force-aspect-ratio`: vídeo via extensão XVideo
//!     (composição clássica em hardware, SEM OpenGL — a GPU legada fica
//!     livre para renderizar a interface FemtoVG);
//!   * **alsasink**: áudio direto no ALSA (PulseAudio/PipeWire mascarados
//!     na distro — um menos na RAM e na CPU);
//!   * O vídeo é embutido NA PRÓPRIA janela X11 do Slint via
//!     `GstVideoOverlay::set_window_handle()` + `set_render_rectangle()`:
//!     o GStreamer pinta direto na janela, por cima de qualquer conteúdo
//!     desenhado pelo Slint naquela região. Por isso, quando o overlay de
//!     sincronização USB (Módulo 4) abre, a UI envia `HideVideo` — o
//!     retângulo de renderização é zerado para o popup ficar 100% visível.
//!
//! Thread única e bloqueante: comandos chegam por canal mpsc (drenados a
//! cada 50ms junto com o poll do GstBus) e eventos voltam por canal mpsc —
//! a thread da interface NUNCA toca em GStreamer diretamente.

use crate::state::models::TrackInfo;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video::prelude::{VideoOverlayExt, VideoOverlayExtManual};
use gstreamer_video::VideoOverlay;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/// Limite da fila de reprodução (evita fila infinita numa noite cheia)
const MAX_QUEUE: usize = 20;

/// Ciclo do poll do GstBus — também define a latência máxima de comandos
const BUS_POLL_INTERVAL_MS: u64 = 50;

/// Tipos considerados ÁUDIO (tudo o mais é tratado como vídeo)
const AUDIO_TYPES: &[&str] = &["mp3", "wav"];

// =============================================================================
// Canais de comunicação com a thread principal
// =============================================================================

/// Comandos enviados PARA o player (da UI / threads de controle)
pub enum PlayerCommand {
    /// XID da janela X11 principal (Slint) — recebe o vídeo embutido
    SetWindowHandle(u64),
    /// Geometria atual da área de vídeo dentro da janela (px lógicos,
    /// já escalados pelo fator de escala no main.rs)
    VideoGeometry { x: i32, y: i32, width: i32, height: i32 },
    /// Overlay USB aberto: esconde o vídeo (zera o retângulo de render)
    HideVideo,
    /// Overlay USB fechado: restaura o último retângulo conhecido
    RestoreVideo,
    /// Enfileira uma faixa (o débito de crédito já foi feito pela UI/DB)
    Enqueue(TrackInfo),
    /// Para tudo e limpa a fila (reservado ao modo de manutenção)
    #[allow(dead_code)]
    Stop,
}

/// Eventos emitidos PELO player para a thread principal
pub enum PlayerEvent {
    /// Uma faixa começou a tocar agora
    TrackStarted {
        id: i64,
        title: String,
        artist: String,
        is_video: bool,
    },
    /// Estado da fila mudou (após enqueue ou consumo) — para o painel "A seguir"
    QueueUpdated { upcoming: Vec<TrackInfo> },
    /// Fila esgotada — UI volta ao modo catálogo
    QueueFinished,
    /// Falha no pipeline (arquivo corrompido, codec ausente...)
    Error { context: String, detail: String },
}

/// Sobe a thread dedicada do player. Retorna imediatamente.
pub fn spawn(cmd_rx: Receiver<PlayerCommand>, event_tx: Sender<PlayerEvent>) {
    thread::Builder::new()
        .name("gstreamer-player".to_string())
        .spawn(move || {
            log::info!("Player: thread do GStreamer iniciada.");

            // gst::init() é pré-requisito único e global — sem ele nada existe.
            if let Err(e) = gst::init() {
                log::error!("Player: falha ao inicializar GStreamer: {}", e);
                let _ = event_tx.send(PlayerEvent::Error {
                    context: "GStreamer".to_string(),
                    detail: e.to_string(),
                });
                return;
            }

            let mut player = match Player::build(&event_tx) {
                Ok(p) => p,
                Err(e) => {
                    log::error!("Player: falha ao construir o pipeline: {}", e);
                    let _ = event_tx.send(PlayerEvent::Error {
                        context: "Pipeline".to_string(),
                        detail: e,
                    });
                    return;
                }
            };

            player.run(cmd_rx);
        })
        .expect("Falha crítica ao criar a thread do player");
}

// =============================================================================
// Núcleo do player
// =============================================================================

struct Player {
    playbin: gst::Element,
    /// View da interface GstVideoOverlay do playbin (encaminha para o sink)
    overlay: VideoOverlay,
    queue: VecDeque<TrackInfo>,
    current: Option<TrackInfo>,
    event_tx: Sender<PlayerEvent>,
    /// XID da janela X11 do Slint (aplicado assim que chega)
    window_handle: Option<u64>,
    /// Último retângulo de vídeo informado pela UI
    video_rect: Option<(i32, i32, i32, i32)>,
    /// Vídeo oculto no momento (overlay USB aberto ou faixa de áudio)
    video_hidden: bool,
}

impl Player {
    /// Constrói playbin com os sinks legados: xvimagesink → ximagesink,
    /// alsasink → autoaudiosink (fallbacks para máquinas em campo com
    /// pacotes de plugin faltando — o jukebox não pode parar de vender).
    fn build(event_tx: &Sender<PlayerEvent>) -> Result<Self, String> {
        let playbin = gst::ElementFactory::make("playbin")
            .name("jukebox-playbin")
            .build()
            .map_err(|e| format!("playbin indisponível (gstreamer1.0-plugins-base ausente): {e}"))?;

        // ---- Saída de vídeo: xvimagesink (XVideo) com fallback ----
        let video_sink = match gst::ElementFactory::make("xvimagesink")
            .name("jukebox-video-sink")
            .build()
        {
            Ok(sink) => {
                // Mantém proporção correta dentro do retângulo (letterbox)
                sink.set_property("force-aspect-ratio", true);
                // Não rouba eventos de teclado da janela do Slint
                sink.set_property("handle-events", false);
                sink.set_property("handle-expose", true);
                sink
            }
            Err(_) => {
                log::warn!("Player: xvimagesink indisponível (gstreamer1.0-x ausente?). Usando ximagesink.");
                gst::ElementFactory::make("ximagesink")
                    .name("jukebox-video-sink")
                    .build()
                    .map_err(|e| format!("nenhum sink de vídeo X11 disponível: {e}"))?
            }
        };

        // ---- Saída de áudio: ALSA direto, sem servidor de som ----
        let audio_sink = match gst::ElementFactory::make("alsasink")
            .name("jukebox-audio-sink")
            .build()
        {
            Ok(sink) => sink,
            Err(_) => {
                log::warn!("Player: alsasink indisponível. Usando autoaudiosink.");
                gst::ElementFactory::make("autoaudiosink")
                    .name("jukebox-audio-sink")
                    .build()
                    .map_err(|e| format!("nenhum sink de áudio disponível: {e}"))?
            }
        };

        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-sink", &audio_sink);
        playbin.set_property("volume", 1.0_f64);

        // playbin implementa a interface GstVideoOverlay: repassa o handle
        // e o retângulo para o sink real mesmo trocando de sink internamente
        let overlay = playbin
            .dynamic_cast_ref::<VideoOverlay>()
            .ok_or("playbin não expõe GstVideoOverlay")?
            .clone();

        // Sinaliza ao playbin que NÓS controlamos a janela antes do READY:
        // evita ele criar uma janela própria de vídeo
        overlay.prepare_window_handle();

        Ok(Self {
            playbin,
            overlay,
            queue: VecDeque::new(),
            current: None,
            event_tx: event_tx.clone(),
            window_handle: None,
            video_rect: None,
            video_hidden: false,
        })
    }

    /// Loop principal: drena comandos → poll do bus → avança a fila
    fn run(&mut self, cmd_rx: Receiver<PlayerCommand>) {
        let bus = match self.playbin.bus() {
            Some(bus) => bus,
            None => {
                log::error!("Player: playbin sem bus — encerrando thread.");
                return;
            }
        };

        loop {
            // 1. Drena todos os comandos pendentes (chegaram desde o último ciclo)
            while let Ok(cmd) = cmd_rx.try_recv() {
                self.handle_command(cmd);
            }

            // 2. Poll do GstBus com timeout de 50ms — é o "clock" do loop:
            //    bloqueia a thread (econômico no single-core) sem travar a UI
            if let Some(msg) = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(BUS_POLL_INTERVAL_MS),
                &[
                    gst::MessageType::Eos,
                    gst::MessageType::Error,
                    gst::MessageType::Warning,
                ],
            ) {
                match msg.view() {
                    gst::MessageView::Eos(_) => self.on_track_end(),
                    gst::MessageView::Error(err) => {
                        let detail = format!(
                            "{} ({})",
                            err.error(),
                            err.debug().unwrap_or_default()
                        );
                        log::error!("Player: erro no pipeline: {}", detail);
                        let _ = self.event_tx.send(PlayerEvent::Error {
                            context: self
                                .current
                                .as_ref()
                                .map(|t| t.title.clone())
                                .unwrap_or_else(|| "pipeline".to_string()),
                            detail: detail.clone(),
                        });
                        // Pula para a próxima — o show não pode parar
                        self.on_track_end();
                    }
                    gst::MessageView::Warning(warn) => {
                        log::warn!("Player: aviso do pipeline: {}", warn.error());
                    }
                    _ => {}
                }
            }

            // 3. Se nenhuma faixa está ativa e a fila tem gente → toca a próxima
            if self.current.is_none() {
                if let Some(track) = self.queue.pop_front() {
                    self.start_track(track);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Comandos
    // -----------------------------------------------------------------------

    fn handle_command(&mut self, cmd: PlayerCommand) {
        match cmd {
            PlayerCommand::SetWindowHandle(xid) => {
                if self.window_handle != Some(xid) {
                    log::info!("Player: janela X11 do Slint registrada (xid=0x{:X}).", xid);
                    // unsafe: contrato do GStreamer — o xid deve permanecer
                    // válido enquanto o pipeline existir (a janela é da UI,
                    // que vive mais que o player)
                    unsafe { self.overlay.set_window_handle(xid as usize) };
                    self.window_handle = Some(xid);
                    self.apply_video_rect();
                }
            }
            PlayerCommand::VideoGeometry { x, y, width, height } => {
                if width > 0 && height > 0 {
                    self.video_rect = Some((x, y, width, height));
                    self.apply_video_rect();
                }
            }
            PlayerCommand::HideVideo => {
                self.video_hidden = true;
                self.apply_video_rect();
            }
            PlayerCommand::RestoreVideo => {
                self.video_hidden = false;
                self.apply_video_rect();
            }
            PlayerCommand::Enqueue(track) => {
                if self.queue.len() >= MAX_QUEUE {
                    log::warn!("Player: fila cheia ({}). Faixa rejeitada.", MAX_QUEUE);
                    let _ = self.event_tx.send(PlayerEvent::Error {
                        context: "Fila de reprodução".to_string(),
                        detail: "fila cheia — tente novamente mais tarde".to_string(),
                    });
                    return;
                }
                log::info!(
                    "Player: enfileirando '{} - {}' (posição {}).",
                    track.artist,
                    track.title,
                    self.queue.len() + 1
                );
                self.queue.push_back(track);
                self.emit_queue_state();
            }
            PlayerCommand::Stop => {
                log::info!("Player: stop + limpeza da fila.");
                let _ = self.playbin.set_state(gst::State::Null);
                self.queue.clear();
                self.current = None;
                let _ = self.event_tx.send(PlayerEvent::QueueFinished);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Controle de faixa
    // -----------------------------------------------------------------------

    /// Inicia a reprodução de uma faixa (vinda do topo da fila)
    fn start_track(&mut self, track: TrackInfo) {
        let is_video = !AUDIO_TYPES.contains(&track.file_type.as_str());

        // URI file:// escapada (arquivos com espaço/acento no nome quebram
        // URI ingênua); canonicalize garante caminho absoluto e existente
        let path = Path::new(&track.file_path);
        let absolute = match std::fs::canonicalize(path) {
            Ok(abs) => abs,
            Err(e) => {
                log::error!(
                    "Player: arquivo não encontrado {:?}: {} — pulando faixa.",
                    track.file_path,
                    e
                );
                let _ = self.event_tx.send(PlayerEvent::Error {
                    context: track.title.clone(),
                    detail: "arquivo não encontrado no disco".to_string(),
                });
                self.current = None;
                return;
            }
        };

        let uri = match gst::glib::filename_to_uri(&absolute, None) {
            Ok(uri) => uri,
            Err(e) => {
                log::error!("Player: falha ao converter caminho em URI: {}", e);
                self.current = None;
                return;
            }
        };

        // Reset do pipeline para trocar de mídia com estado limpo
        let _ = self.playbin.set_state(gst::State::Null);
        self.playbin.set_property("uri", uri);

        // Antes do 1º frame: garante que o vídeo não "vaze" para a janela
        // inteira. Faixa de vídeo usa o último retângulo da UI (se houver);
        // faixa de áudio (ou retângulo desconhecido) fica com retângulo zerado
        if is_video && self.video_rect.is_some() {
            self.video_hidden = false;
        } else {
            self.video_hidden = true;
        }
        self.apply_video_rect();

        match self.playbin.set_state(gst::State::Playing) {
            Ok(_) => {
                log::info!(
                    "Player: tocando '{}' [{}] (fila restante: {}).",
                    track.title,
                    if is_video { "vídeo" } else { "áudio" },
                    self.queue.len()
                );
                let _ = self.event_tx.send(PlayerEvent::TrackStarted {
                    id: track.id,
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    is_video,
                });
                self.current = Some(track);
                self.emit_queue_state();
            }
            Err(e) => {
                log::error!("Player: falha ao iniciar reprodução: {}", e);
                let _ = self.event_tx.send(PlayerEvent::Error {
                    context: track.title.clone(),
                    detail: e.to_string(),
                });
                self.current = None;
            }
        }
    }

    /// Fim natural (EOS) ou forçado (erro) da faixa atual
    fn on_track_end(&mut self) {
        self.current = None;
        let _ = self.playbin.set_state(gst::State::Null);

        // Zera o retângulo de vídeo imediatamente: o XVideo pinta numa
        // subjanela da janela do Slint que SOBREVIVE ao pipeline — sem isso,
        // o último frame ficaria estampado sobre o catálogo.
        self.video_hidden = true;
        self.apply_video_rect();

        if self.queue.is_empty() {
            log::info!("Player: fila esgotada.");
            let _ = self.event_tx.send(PlayerEvent::QueueFinished);
        }
        // A próxima faixa sobe no passo 3 do loop principal
    }

    /// Aplica o retângulo de vídeo atual (respeitando o estado hidden)
    fn apply_video_rect(&mut self) {
        let rect = if self.video_hidden {
            (0, 0, 0, 0)
        } else {
            self.video_rect.unwrap_or((0, 0, 0, 0))
        };

        let (x, y, w, h) = rect;
        if let Err(e) = self.overlay.set_render_rectangle(x, y, w, h) {
            log::warn!("Player: falha ao aplicar retângulo de vídeo: {}", e);
        }
        // Repinta a região na hora (evita frame velho parado na tela)
        self.overlay.expose();
    }

    /// Notifica a UI sobre o conteúdo atual da fila
    fn emit_queue_state(&self) {
        let _ = self.event_tx.send(PlayerEvent::QueueUpdated {
            upcoming: self.queue.iter().cloned().collect(),
        });
    }
}
