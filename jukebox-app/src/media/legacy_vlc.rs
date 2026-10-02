//! Player do perfil legacy — reprodução via **libVLC** (fase 2).
//!
//! O VLC 1.1 da base antiga é carregado por soname (`libvlc.so.5`, ABI
//! estável do 1.1 ao 3.x) com `libloading` — o binário não linka VLC em
//! tempo de build. O desenho é o do vlcj original:
//!
//! - **um media player** reutilizado entre faixas (`stop → set_media →
//!   play`, a receita do vlcj 2.1 para o VLC 1.1);
//! - **vídeo embutido**: `libvlc_video_set_xwindow` numa janela filha da
//!   janela Slint (`legacy_x11`) — o VLC renderiza direto com Xv/X11,
//!   zero cópia de pixels pela CPU no Pentium 4;
//! - **eventos por canal**: o callback do VLC nunca chama funções da
//!   libVLC (regra de ouro do 1.1 — deadlock); ele só empurra um sinal
//!   no canal e o loop do player decide.
//!
//! Dinheiro e histórico seguem o caminho decifrado do **bytecode v28**
//! (jjbox.res + disassemblagem das classes):
//!
//! - `Enqueue` debita, grava `registro_musicas` **na seleção**, snapshota
//!   o saldo em `registro_creditos` e enfileira em `filamidia` — tudo numa
//!   transação; `modofesta` libera seleção sem saldo (free play);
//! - o **brinde** é contado no CRÉDITO (não por música): cada moeda
//!   decrementa `1/relacaocredito` de `contbrinde`; ao cruzar zero o
//!   prêmio é creditado e a linha vai para a tabela `brinde` (ver
//!   `entrada_moeda` do cliente);
//! - com a fila vazia e `modoaleatorio`, o player espera
//!   `mininicioaleatorio` **minutos** de silêncio e sorteia uma
//!   `midia.aleatorio` — sem débito, sem fila, sem histórico (música da
//!   casa; um rollback para o Java não repassaria o que tocou de graça).
//!
//! Sem libVLC no sistema (ou sem X), o spawn degrada para o modo de
//! validação (`legacy_player::run`): débito e fila reais, sem mídia.

use super::legacy_x11::VideoOverlay;
use super::player::{PlayerCommand, PlayerEvent};
use crate::db::legacy_pg::client::LegacyDb;
use crate::db::legacy_pg::{atraso_aleatorio, PgConfig, SystemRow};
use crate::state::models::TrackInfo;
use libloading::Library;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

/// Preço por seleção: constante do sistema original (1 crédito).
const CUSTO_POR_MUSICA: f64 = 1.0;

/// Soname da biblioteca (VLC 1.1 → 3.x mantêm a ABI major 5).
const LIBVLC_SONAME: &str = "libvlc.so.5";

/// `libvlc_MediaPlayerEndReached` (0x109) — inteiro estável na ABI.
const VLC_EVENT_END_REACHED: c_int = 0x109;
/// `libvlc_MediaPlayerEncounteredError` (0x10A) — inteiro estável na ABI.
const VLC_EVENT_ERROR: c_int = 0x10A;

/// Intervalo do laço do player (drenar sinais do VLC e reagir).
const TICK: Duration = Duration::from_millis(100);
/// Nova tentativa de avançar a fila após erro de banco (o Java também
/// apenas logava e seguia; aqui a faixa não se perde).
const RETRY_ADVANCE: Duration = Duration::from_secs(2);
/// Janela em que sinais do VLC são descartados após um `stop()` nosso —
/// defesa contra builds do 1.1 que emitem `EndReached` no stop manual.
const SIGNAL_SUPPRESS: Duration = Duration::from_millis(250);
/// Limite de tentativas por `advance()` — protege contra uma árvore de
/// mídia inteira sem arquivos (o loop para no modo ocioso).
const MAX_ADVANCE_ATTEMPTS: usize = 50;
/// Volume do efeito de crédito na escala do VLC (0..=100).
const FX_VOLUME: c_int = 30;

/// Sinais entregues pelo trampolim de eventos do VLC.
#[derive(Debug, Clone, Copy, PartialEq)]
enum VlcSignal {
    EndReached,
    Error,
}

/// Converte o volume linear cúbico do app (0..1) para a escala 0..=100 da
/// libVLC — devolve exatamente o `sistema.volume` original (o Java passava
/// a coluna direto ao `libvlc_audio_set_volume`; o app aplica a curva
/// perceptual antes de enviar, aqui é invertida).
pub fn vlc_volume(linear: f64) -> u8 {
    let fraction = if linear.is_finite() { linear.clamp(0.0, 1.0) } else { 0.0 };
    (fraction.powf(1.0 / 3.0) * 100.0).round() as u8
}

// =============================================================================
// Fronteira de banco (testável sem PostgreSQL)
// =============================================================================

/// Operações de banco que o player precisa — `LegacyDb` as implementa;
/// os testes usam um banco roteirizado. (`registro_musicas` e o brinde
/// ficam no `enqueue`/`entrada_moeda` do cliente — como o original, que
/// registra na seleção e conta o brinde no crédito.)
trait QueueDb {
    fn enqueue(&mut self, midia_id: i64, custo: f64) -> Result<(), String>;
    fn dequeue(&mut self) -> Result<Option<TrackInfo>, String>;
    fn sistema(&mut self) -> Result<SystemRow, String>;
    fn sortear_aleatoria(&mut self, aleatoriovideo: bool) -> Result<Option<TrackInfo>, String>;
    fn queue_snapshot(&mut self) -> Result<Vec<TrackInfo>, String>;
}

impl QueueDb for LegacyDb {
    fn enqueue(&mut self, midia_id: i64, custo: f64) -> Result<(), String> {
        LegacyDb::enqueue(self, midia_id, custo)
    }
    fn dequeue(&mut self) -> Result<Option<TrackInfo>, String> {
        LegacyDb::dequeue(self)
    }
    fn sistema(&mut self) -> Result<SystemRow, String> {
        LegacyDb::sistema(self)
    }
    fn sortear_aleatoria(&mut self, aleatoriovideo: bool) -> Result<Option<TrackInfo>, String> {
        LegacyDb::sortear_aleatoria(self, aleatoriovideo)
    }
    fn queue_snapshot(&mut self) -> Result<Vec<TrackInfo>, String> {
        LegacyDb::queue_snapshot(self)
    }
}

// =============================================================================
// Motor de reprodução (libVLC real ou stub de teste)
// =============================================================================

/// Motor que executa mídia. O real conversa com a libVLC; o de teste
/// registra as chamadas.
trait Engine {
    fn play(&mut self, path: &str, volume: u8) -> Result<(), String>;
    fn stop(&mut self);
    fn credit_fx(&mut self);
    fn set_volume(&mut self, volume: u8);
}

#[allow(non_snake_case)]
mod ffi {
    use super::*;

    pub type NewFn =
        unsafe extern "C" fn(argc: c_int, argv: *const *const c_char) -> *mut c_void;
    pub type ReleaseFn = unsafe extern "C" fn(instance: *mut c_void);
    pub type GetVersionFn = unsafe extern "C" fn() -> *const c_char;
    /// `libvlc_media_new_path` (≥ 2.0) ou `libvlc_media_new` (1.1) — mesma forma.
    pub type MediaNewFn = unsafe extern "C" fn(instance: *mut c_void, path: *const c_char)
        -> *mut c_void;
    pub type MediaReleaseFn = unsafe extern "C" fn(media: *mut c_void);
    pub type MediaPlayerNewFn = unsafe extern "C" fn(instance: *mut c_void) -> *mut c_void;
    pub type MediaPlayerReleaseFn = unsafe extern "C" fn(player: *mut c_void);
    pub type SetMediaFn = unsafe extern "C" fn(player: *mut c_void, media: *mut c_void);
    pub type PlayFn = unsafe extern "C" fn(player: *mut c_void) -> c_int;
    pub type StopFn = unsafe extern "C" fn(player: *mut c_void);
    /// `libvlc_video_set_xwindow` (≥ 2.0) ou `libvlc_video_set_drawable` (1.1).
    pub type SetXwindowFn = unsafe extern "C" fn(player: *mut c_void, drawable: u32);
    pub type AudioSetVolumeFn = unsafe extern "C" fn(player: *mut c_void, volume: c_int) -> c_int;
    pub type EventManagerFn = unsafe extern "C" fn(player: *mut c_void) -> *mut c_void;
    pub type EventAttachFn = unsafe extern "C" fn(
        manager: *mut c_void,
        event_type: c_int,
        callback: *const c_void,
        user_data: *mut c_void,
    ) -> c_int;
    pub type EventDetachFn = unsafe extern "C" fn(
        manager: *mut c_void,
        event_type: c_int,
        callback: *const c_void,
        user_data: *mut c_void,
    );
}

/// Símbolos da libVLC (dlopen por soname).
struct VlcApi {
    new: ffi::NewFn,
    release: ffi::ReleaseFn,
    get_version: ffi::GetVersionFn,
    media_new: ffi::MediaNewFn,
    media_release: ffi::MediaReleaseFn,
    media_player_new: ffi::MediaPlayerNewFn,
    media_player_release: ffi::MediaPlayerReleaseFn,
    set_media: ffi::SetMediaFn,
    play: ffi::PlayFn,
    stop: ffi::StopFn,
    set_xwindow: ffi::SetXwindowFn,
    audio_set_volume: ffi::AudioSetVolumeFn,
    event_manager: ffi::EventManagerFn,
    event_attach: ffi::EventAttachFn,
    event_detach: ffi::EventDetachFn,
}

impl VlcApi {
    unsafe fn load(lib: &Library) -> Result<Self, String> {
        // O 1.1 chama `libvlc_media_new`; o 2.0+ renomeou para
        // `libvlc_media_new_path` (mesma assinatura de fato).
        let media_new: ffi::MediaNewFn = *lib
            .get(b"libvlc_media_new_path\0")
            .or_else(|_| lib.get(b"libvlc_media_new\0"))
            .map_err(|e| format!("dlsym libvlc_media_new(_path): {e}"))?;
        // Vídeo embutido: `set_xwindow` no 2.0+, `set_drawable` no 1.1.
        let set_xwindow: ffi::SetXwindowFn = *lib
            .get(b"libvlc_video_set_xwindow\0")
            .or_else(|_| lib.get(b"libvlc_video_set_drawable\0"))
            .map_err(|e| format!("dlsym libvlc_video_set_xwindow/_drawable: {e}"))?;
        Ok(Self {
            new: *lib.get(b"libvlc_new\0").map_err(|e| format!("dlsym libvlc_new: {e}"))?,
            release: *lib
                .get(b"libvlc_release\0")
                .map_err(|e| format!("dlsym libvlc_release: {e}"))?,
            get_version: *lib
                .get(b"libvlc_get_version\0")
                .map_err(|e| format!("dlsym libvlc_get_version: {e}"))?,
            media_new,
            media_release: *lib
                .get(b"libvlc_media_release\0")
                .map_err(|e| format!("dlsym libvlc_media_release: {e}"))?,
            media_player_new: *lib
                .get(b"libvlc_media_player_new\0")
                .map_err(|e| format!("dlsym libvlc_media_player_new: {e}"))?,
            media_player_release: *lib
                .get(b"libvlc_media_player_release\0")
                .map_err(|e| format!("dlsym libvlc_media_player_release: {e}"))?,
            set_media: *lib
                .get(b"libvlc_media_player_set_media\0")
                .map_err(|e| format!("dlsym libvlc_media_player_set_media: {e}"))?,
            play: *lib
                .get(b"libvlc_media_player_play\0")
                .map_err(|e| format!("dlsym libvlc_media_player_play: {e}"))?,
            stop: *lib
                .get(b"libvlc_media_player_stop\0")
                .map_err(|e| format!("dlsym libvlc_media_player_stop: {e}"))?,
            set_xwindow,
            audio_set_volume: *lib
                .get(b"libvlc_audio_set_volume\0")
                .map_err(|e| format!("dlsym libvlc_audio_set_volume: {e}"))?,
            event_manager: *lib
                .get(b"libvlc_media_player_event_manager\0")
                .map_err(|e| format!("dlsym libvlc_media_player_event_manager: {e}"))?,
            event_attach: *lib
                .get(b"libvlc_event_attach\0")
                .map_err(|e| format!("dlsym libvlc_event_attach: {e}"))?,
            event_detach: *lib
                .get(b"libvlc_event_detach\0")
                .map_err(|e| format!("dlsym libvlc_event_detach: {e}"))?,
        })
    }
}

/// Trampolim dos eventos do VLC: lê o `type` (primeiro campo do
/// `libvlc_event_t`) e empurra o sinal no canal. **Nunca** chama funções
/// da libVLC daqui (deadlock no 1.1).
unsafe extern "C" fn on_vlc_event(event: *const c_void, user_data: *mut c_void) {
    if event.is_null() || user_data.is_null() {
        return;
    }
    let event_type = unsafe { *(event as *const c_int) };
    let signal = match event_type {
        VLC_EVENT_END_REACHED => VlcSignal::EndReached,
        VLC_EVENT_ERROR => VlcSignal::Error,
        _ => return,
    };
    let sender = unsafe { &*(user_data as *const Mutex<Sender<VlcSignal>>) };
    if let Ok(tx) = sender.lock() {
        let _ = tx.send(signal);
    }
}

/// Motor libVLC real: uma instância, dois media players (mídia + efeito de
/// crédito) e a janela X11 do overlay.
struct RealEngine {
    api: VlcApi,
    instance: *mut c_void,
    player: *mut c_void,
    fx: *mut c_void,
    fx_path: Option<PathBuf>,
    overlay_xid: u32,
    /// Contexto do trampolim (liberado por último no Drop).
    callback_ctx: Option<*mut Mutex<Sender<VlcSignal>>>,
    /// Fecha a biblioteca no fim (os ponteiros acima apontam para ela).
    _lib: Library,
}

// A libVLC é thread-safe para este uso (uma thread do player + a thread
// interna de eventos); os ponteiros nunca cruzam threads além desta.
unsafe impl Send for RealEngine {}

impl RealEngine {
    /// Abre a libVLC, cria instância/players, anexa eventos e (se houver
    /// XID) registra o overlay X11.
    fn new(parent_xid: Option<u64>) -> Result<(Self, Receiver<VlcSignal>), String> {
        let soname = std::env::var("JUKEBOX_LIBVLC")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| LIBVLC_SONAME.into());
        unsafe {
            let lib = Library::new(&soname).map_err(|e| format!("dlopen {soname}: {e}"))?;
            let api = VlcApi::load(&lib)?;

            let version = {
                let ptr = (api.get_version)();
                if ptr.is_null() {
                    "?".to_string()
                } else {
                    CStr::from_ptr(ptr).to_string_lossy().into_owned()
                }
            };

            // Bandeiras do vlcj original: sem marca d'água de título, log calmo.
            let no_title = CString::new("--no-video-title-show").expect("bandeira constante");
            let quiet = CString::new("--quiet").expect("bandeira constante");
            let argv = [no_title.as_ptr(), quiet.as_ptr()];
            let instance = (api.new)(argv.len() as c_int, argv.as_ptr());
            if instance.is_null() {
                return Err("libvlc_new falhou".into());
            }

            let cleanup_instance = |api: &VlcApi, instance: *mut c_void| {
                if !instance.is_null() {
                    (api.release)(instance);
                }
            };

            let player = (api.media_player_new)(instance);
            if player.is_null() {
                cleanup_instance(&api, instance);
                return Err("libvlc_media_player_new falhou".into());
            }
            let fx = (api.media_player_new)(instance);

            // Overlay X11 (opcional): sem ele o áudio segue normalmente.
            let overlay_xid = parent_xid
                .filter(|xid| *xid != 0)
                .and_then(|xid| match VideoOverlay::new(xid) {
                    Ok(overlay) => {
                        let xid = overlay.window_id();
                        overlay_register(overlay);
                        Some(xid)
                    }
                    Err(e) => {
                        log::warn!("Overlay X11 indisponível (vídeo sem janela embutida): {e}");
                        None
                    }
                })
                .unwrap_or(0);
            if overlay_xid != 0 {
                (api.set_xwindow)(player, overlay_xid);
            }

            // Fim/erro → canal (o trampolim não pode tocar a libVLC).
            let (signal_tx, signal_rx) = std::sync::mpsc::channel::<VlcSignal>();
            let callback_ctx: *mut Mutex<Sender<VlcSignal>> =
                Box::into_raw(Box::new(Mutex::new(signal_tx)));
            let manager = (api.event_manager)(player);
            if manager.is_null() {
                if !fx.is_null() {
                    (api.media_player_release)(fx);
                }
                (api.media_player_release)(player);
                cleanup_instance(&api, instance);
                drop(Box::from_raw(callback_ctx));
                return Err("libvlc_media_player_event_manager falhou".into());
            }
            let callback = on_vlc_event as *const c_void;
            let ctx = callback_ctx as *mut c_void;
            let attached = (api.event_attach)(manager, VLC_EVENT_END_REACHED, callback, ctx) == 0
                && (api.event_attach)(manager, VLC_EVENT_ERROR, callback, ctx) == 0;
            if !attached {
                if !fx.is_null() {
                    (api.media_player_release)(fx);
                }
                (api.media_player_release)(player);
                cleanup_instance(&api, instance);
                drop(Box::from_raw(callback_ctx));
                return Err("libvlc_event_attach falhou".into());
            }

            // Efeito de crédito: WAV gerado em /tmp (o VLC 1.1 não tem
            // fonte de tom; o Java tocava um arquivo também).
            let fx_path = write_credit_fx();

            log::info!(
                "libVLC {version} carregado (overlay XID {}, efeito de crédito {})",
                if overlay_xid == 0 { "ausente" } else { "ativo" },
                if fx_path.is_some() { "ativo" } else { "desativado" }
            );
            Ok((
                Self {
                    api,
                    instance,
                    player,
                    fx,
                    fx_path,
                    overlay_xid,
                    callback_ctx: Some(callback_ctx),
                    _lib: lib,
                },
                signal_rx,
            ))
        }
    }
}

impl Engine for RealEngine {
    fn play(&mut self, path: &str, volume: u8) -> Result<(), String> {
        let path = CString::new(path).map_err(|e| format!("caminho com NUL: {e}"))?;
        unsafe {
            // Receita vlcj para o 1.1: stop → nova mídia → play.
            (self.api.stop)(self.player);
            let media = (self.api.media_new)(self.instance, path.as_ptr());
            if media.is_null() {
                return Err("libvlc_media_new falhou".into());
            }
            (self.api.set_xwindow)(self.player, self.overlay_xid);
            (self.api.audio_set_volume)(self.player, volume as c_int);
            (self.api.set_media)(self.player, media);
            (self.api.media_release)(media); // o player segura a referência
            if (self.api.play)(self.player) != 0 {
                return Err("libvlc_media_player_play falhou".into());
            }
        }
        Ok(())
    }

    fn stop(&mut self) {
        unsafe { (self.api.stop)(self.player) };
    }

    fn credit_fx(&mut self) {
        let Some(path) = &self.fx_path else { return };
        let Ok(path) = CString::new(path.to_string_lossy().as_bytes()) else {
            return;
        };
        unsafe {
            if self.fx.is_null() {
                return;
            }
            let media = (self.api.media_new)(self.instance, path.as_ptr());
            if media.is_null() {
                return;
            }
            (self.api.set_media)(self.fx, media);
            (self.api.media_release)(media);
            (self.api.audio_set_volume)(self.fx, FX_VOLUME);
            let _ = (self.api.play)(self.fx);
        }
    }

    fn set_volume(&mut self, volume: u8) {
        unsafe { (self.api.audio_set_volume)(self.player, volume as c_int) };
    }
}

impl Drop for RealEngine {
    fn drop(&mut self) {
        unsafe {
            if !self.player.is_null() {
                (self.api.stop)(self.player);
                let manager = (self.api.event_manager)(self.player);
                if !manager.is_null() {
                    if let Some(ctx) = self.callback_ctx {
                        let callback = on_vlc_event as *const c_void;
                        (self.api.event_detach)(
                            manager,
                            VLC_EVENT_END_REACHED,
                            callback,
                            ctx as *mut c_void,
                        );
                        (self.api.event_detach)(
                            manager,
                            VLC_EVENT_ERROR,
                            callback,
                            ctx as *mut c_void,
                        );
                    }
                }
                (self.api.media_player_release)(self.player);
            }
            if !self.fx.is_null() {
                (self.api.stop)(self.fx);
                (self.api.media_player_release)(self.fx);
            }
            if !self.instance.is_null() {
                (self.api.release)(self.instance);
            }
        }
        // Contexto dos callbacks por último — nada pode disparar evento
        // depois disso.
        if let Some(ctx) = self.callback_ctx.take() {
            unsafe { drop(Box::from_raw(ctx)) };
        }
    }
}

// =============================================================================
// Overlay X11: o event loop sincroniza a geometria, o player decide visibilidade
// =============================================================================

static OVERLAY: OnceLock<Mutex<OverlayProxy>> = OnceLock::new();

struct OverlayProxy {
    overlay: Option<VideoOverlay>,
    /// Há faixa de vídeo em execução? (decide o mapeamento da janela)
    video_active: bool,
    /// Última geometria recebida do event loop (pixels físicos).
    geometry: Option<(i32, i32, u32, u32)>,
}

impl OverlayProxy {
    fn apply(&mut self) {
        let Some(overlay) = self.overlay.as_mut() else { return };
        let Some((x, y, w, h)) = self.geometry else { return };
        overlay.place(x, y, w, h, self.video_active);
    }
}

/// Registra a janela do overlay (thread do player, no boot do motor).
fn overlay_register(overlay: VideoOverlay) {
    let proxy = OVERLAY.get_or_init(|| {
        Mutex::new(OverlayProxy { overlay: None, video_active: false, geometry: None })
    });
    if let Ok(mut guard) = proxy.lock() {
        guard.overlay = Some(overlay);
    }
}

/// Liga/desliga o overlay: chamado ao iniciar faixa de vídeo e ao ociosar.
fn overlay_set_active(active: bool) {
    if let Some(proxy) = OVERLAY.get() {
        if let Ok(mut guard) = proxy.lock() {
            guard.video_active = active;
            guard.apply();
        }
    }
}

/// Sincroniza a geometria do overlay — chamado pelo event loop do Slint a
/// cada tick com a área de vídeo (ou a janela inteira, em tela cheia), em
/// **pixels físicos** e coordenadas relativas à janela do app.
pub fn overlay_sync(x: i32, y: i32, w: u32, h: u32) {
    if let Some(proxy) = OVERLAY.get() {
        if let Ok(mut guard) = proxy.lock() {
            guard.geometry = Some((x, y, w, h));
            guard.apply();
        }
    }
}

// =============================================================================
// Efeito de crédito (arpejo C5→C6 em WAV PCM gerado)
// =============================================================================

/// Notas do avpejo (Hz, duração) — o mesmo desenho do player modern.
const FX_NOTES_HZ: [f64; 2] = [523.25, 1046.50];
const FX_NOTE_MS: u32 = 75;
const FX_RATE: u32 = 8000;
const FX_ENV_MS: u32 = 5;

/// Envelope trapezoidal (ataque/solta de `env` amostras) — anti-estalo.
fn fx_envelope(i: u32, note_len: u32, env: u32) -> f64 {
    let ramp = |x: u32| x.min(env) as f64 / env as f64;
    ramp(i).min(ramp(note_len.saturating_sub(1) - i))
}

/// Gera o arpejo de crédito como WAV PCM 16-bit mono 8 kHz (~150 ms,
/// ~2,4 KiB). Tocado num media player separado, sem drawable — o VLC
/// mistura no ALSA como o GStreamer fazia no dmix.
pub fn credit_fx_wav() -> Vec<u8> {
    let note_len = FX_NOTE_MS * FX_RATE / 1000;
    let env = FX_ENV_MS * FX_RATE / 1000;
    let mut samples: Vec<i16> = Vec::with_capacity(note_len as usize * 2);
    for &hz in &FX_NOTES_HZ {
        for i in 0..note_len {
            let t = i as f64 / FX_RATE as f64;
            let amp = fx_envelope(i, note_len, env.max(1)) * 0.22;
            let v = amp * (2.0 * std::f64::consts::PI * hz * t).sin();
            samples.push((v * i16::MAX as f64) as i16);
        }
    }
    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // tamanho do chunk fmt
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&FX_RATE.to_le_bytes());
    wav.extend_from_slice(&(FX_RATE * 2).to_le_bytes()); // bytes/s
    wav.extend_from_slice(&2u16.to_le_bytes()); // bloco
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// Grava o WAV do efeito em `/tmp` (best-effort — sem ele, sem som de moeda).
fn write_credit_fx() -> Option<PathBuf> {
    let path = std::env::temp_dir().join("jukeboxtv-credit-fx.wav");
    match std::fs::write(&path, credit_fx_wav()) {
        Ok(()) => Some(path),
        Err(e) => {
            log::warn!("Efeito de crédito indisponível: {e}");
            None
        }
    }
}

// =============================================================================
// Sessão do player (máquina de estados pura em cima de Db + Engine)
// =============================================================================

/// Sessão do player legacy: fila do banco + motor de reprodução.
struct Session<D: QueueDb, E: Engine> {
    db: D,
    engine: E,
    event_tx: Sender<PlayerEvent>,
    signal_rx: Receiver<VlcSignal>,
    /// Faixa em execução (para `Previous` ao terminar e para saber se
    /// `Enqueue` deve disparar a reprodução imediata).
    current: Option<TrackInfo>,
    /// Volume na escala do VLC (0..=100).
    volume: u8,
    /// Fim de faixa agendado por erro de banco (reprocessa a fila).
    retry_at: Option<Instant>,
    retry_delay: Duration,
    /// Sorteio do modo aleatório agendado (`mininicioaleatorio` minutos
    /// após a fila esvaziar — thread `jjbox/d` do original).
    random_draw_at: Option<Instant>,
    /// Sobrescrita de teste para o atraso do aleatório.
    random_delay_override: Option<Duration>,
    /// Sinais do VLC ignorados até este instante (pós `stop()` manual).
    suppress_signals_until: Option<Instant>,
    tick: Duration,
}

impl<D: QueueDb, E: Engine> Session<D, E> {
    fn new(db: D, engine: E, signal_rx: Receiver<VlcSignal>, event_tx: Sender<PlayerEvent>) -> Self {
        Self {
            db,
            engine,
            event_tx,
            signal_rx,
            current: None,
            volume: 70,
            retry_at: None,
            retry_delay: RETRY_ADVANCE,
            random_draw_at: None,
            random_delay_override: None,
            suppress_signals_until: None,
            tick: TICK,
        }
    }

    /// Laço principal: publica a fila residual, processa sinais do VLC e
    /// comandos do app até o canal fechar.
    fn run(&mut self, cmd_rx: &Receiver<PlayerCommand>) {
        self.emit_snapshot();
        let _ = self.event_tx.send(PlayerEvent::CreditsChanged);
        loop {
            if let Some(when) = self.retry_at {
                if Instant::now() >= when {
                    self.retry_at = None;
                    self.advance();
                }
            }
            // Modo aleatório: silêncio cronometrado (mininicioaleatorio
            // minutos) e nova conferência do modo antes de sortear.
            if let Some(when) = self.random_draw_at {
                if Instant::now() >= when {
                    self.random_draw_at = None;
                    self.draw_random_if_idle();
                }
            }
            // Sinais do VLC primeiro: o fim da faixa não espera o próximo
            // comando do usuário.
            while let Ok(signal) = self.signal_rx.try_recv() {
                self.on_signal(signal);
            }
            match cmd_rx.recv_timeout(self.tick) {
                Ok(cmd) => self.on_command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.engine.stop();
        overlay_set_active(false);
    }

    fn on_signal(&mut self, signal: VlcSignal) {
        if let Some(until) = self.suppress_signals_until {
            if Instant::now() < until {
                return; // eco do stop() manual
            }
            self.suppress_signals_until = None;
        }
        if signal == VlcSignal::Error {
            if let Some(track) = &self.current {
                let _ = self.event_tx.send(PlayerEvent::Error {
                    context: track.title.clone(),
                    detail: "o VLC não conseguiu reproduzir (arquivo ausente ou corrompido?)"
                        .into(),
                });
            }
        }
        self.finish_current();
        self.advance();
    }

    fn on_command(&mut self, cmd: PlayerCommand) {
        match cmd {
            PlayerCommand::Enqueue(track) => {
                match self.db.enqueue(track.id, CUSTO_POR_MUSICA) {
                    Ok(()) => {
                        let _ = self.event_tx.send(PlayerEvent::CreditsChanged);
                        if self.current.is_none() {
                            self.advance(); // primeira seleção toca na hora
                        } else {
                            let _ = self
                                .event_tx
                                .send(PlayerEvent::Notice(format!("Na fila: {}", track.title)));
                            self.emit_snapshot();
                        }
                    }
                    Err(e) => {
                        let message = if e.contains("saldo insuficiente") {
                            "Créditos insuficientes. Insira saldo para escolher uma música."
                        } else {
                            e.as_str()
                        };
                        let _ = self.event_tx.send(PlayerEvent::Notice(message.into()));
                    }
                }
            }
            PlayerCommand::SkipTrack => {
                if self.current.is_some() {
                    self.suppress_signals_until =
                        Some(Instant::now() + SIGNAL_SUPPRESS);
                    self.engine.stop();
                    self.finish_current();
                    self.advance();
                } else {
                    match self.db.dequeue() {
                        Ok(Some(_)) => self.emit_snapshot(),
                        Ok(None) => {}
                        Err(e) => {
                            let _ = self.event_tx.send(PlayerEvent::Error {
                                context: "Fila legacy".into(),
                                detail: e,
                            });
                        }
                    }
                }
            }
            PlayerCommand::SetVolume(linear) => {
                self.volume = vlc_volume(linear);
                self.engine.set_volume(self.volume);
            }
            PlayerCommand::CreditFx => self.engine.credit_fx(),
            PlayerCommand::Storage(_) => {
                // O menu do operador fica aguardando a resposta — responder
                // vazio libera o spinner (o acervo é do importador).
                let _ = self.event_tx.send(PlayerEvent::Storage {
                    rows: Vec::new(),
                    catalog: None,
                    message: "Gerenciamento de armazenamento não se aplica ao perfil legacy"
                        .into(),
                    restore: false,
                });
            }
            PlayerCommand::Activity
            | PlayerCommand::Operator(_)
            | PlayerCommand::HideVideo
            | PlayerCommand::RestoreVideo
            | PlayerCommand::ReloadSettings
            | PlayerCommand::RefreshBackgrounds => {}
        }
    }

    /// Conclui a faixa atual publicando-a como "anterior".
    fn finish_current(&mut self) {
        if let Some(track) = self.current.take() {
            let _ = self.event_tx.send(PlayerEvent::Previous {
                title: track.title,
                artist: track.artist,
            });
        }
    }

    /// Toca a próxima faixa da fila; vazia, agenda o sorteio do modo
    /// aleatório (após `mininicioaleatorio` minutos de silêncio) ou entra
    /// no modo ocioso. Iterativo: uma árvore com arquivos ausentes não
    /// explode a pilha nem trava o player.
    fn advance(&mut self) {
        for _ in 0..MAX_ADVANCE_ATTEMPTS {
            match self.db.dequeue() {
                Ok(Some(track)) => {
                    if self.try_start(&track) {
                        self.retry_at = None;
                        self.emit_snapshot();
                        return;
                    }
                    // Não tocou (arquivo quebrado): o histórico já contou a
                    // execução — o crédito foi pago no enqueue.
                    continue;
                }
                Ok(None) => {
                    // Fila vazia: o modo aleatório agenda o sorteio após o
                    // silêncio de `mininicioaleatorio` minutos (o Java
                    // espera na thread jjbox/d e re-confere o modo); sem
                    // modo aleatório, fica ocioso mesmo.
                    match self.db.sistema() {
                        Ok(sys) if sys.modoaleatorio => {
                            let delay = self.random_delay_override.unwrap_or_else(|| {
                                atraso_aleatorio(sys.mininicioaleatorio)
                            });
                            self.random_draw_at = Some(Instant::now() + delay);
                        }
                        _ => {}
                    }
                    self.idle();
                    return;
                }
                Err(e) => {
                    // Banco fora do ar no meio da fila: mantém o estado e
                    // re-tenta no próximo tick (a faixa não se perde).
                    self.report_error("Fila legacy", e);
                    self.retry_at = Some(Instant::now() + self.retry_delay);
                    return;
                }
            }
        }
        self.idle();
    }

    /// Sorteio do modo aleatório: só com a máquina parada (uma seleção
    /// paga que chegue durante o silêncio adia o sorteio, não o cancela).
    fn draw_random_if_idle(&mut self) {
        if self.current.is_some() {
            // Alguém pagou durante a espera: confere de novo em 5 s.
            self.random_draw_at = Some(Instant::now() + Duration::from_secs(5));
            return;
        }
        // Re-confere o modo (o operador pode ter desligado na espera) —
        // como a thread original faz após o sleep.
        let Ok(sys) = self.db.sistema() else { return };
        if !sys.modoaleatorio {
            return;
        }
        match self.db.sortear_aleatoria(sys.aleatoriovideo) {
            Ok(Some(track)) => {
                // Música da casa: sem débito, sem fila, sem histórico
                // (o modo 1 do original não passa pelo filamidia).
                if self.try_start(&track) {
                    self.emit_snapshot();
                } else {
                    // Arquivo quebrado: tenta de novo no próximo tick.
                    self.random_draw_at = Some(Instant::now() + self.tick);
                }
            }
            Ok(None) => {
                // Pool vazio: confere de novo em 1 min (catálogo pode
                // mudar com o importador rodando).
                self.random_draw_at = Some(Instant::now() + Duration::from_secs(60));
            }
            Err(e) => {
                self.report_error("Modo aleatório", e);
                self.random_draw_at = Some(Instant::now() + Duration::from_secs(60));
            }
        }
    }

    /// Inicia a faixa: eventos e reprodução (histórico e brinde ficam no
    /// `enqueue`/`entrada_moeda` do banco — como o original).
    fn try_start(&mut self, track: &TrackInfo) -> bool {
        let is_video = track.is_video();
        let _ = self.event_tx.send(PlayerEvent::TrackStarted {
            id: track.id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            is_video,
        });
        overlay_set_active(is_video);
        if is_video {
            let _ = self.event_tx.send(PlayerEvent::Visual(true));
        }
        match self.engine.play(&track.file_path, self.volume) {
            Ok(()) => {
                self.current = Some(track.clone());
                true
            }
            Err(e) => {
                let _ = self.event_tx.send(PlayerEvent::Error {
                    context: track.title.clone(),
                    detail: e,
                });
                overlay_set_active(false);
                false
            }
        }
    }

    /// Modo ocioso: sem faixa, overlay fechado, painel limpo.
    fn idle(&mut self) {
        self.engine.stop();
        overlay_set_active(false);
        let _ = self.event_tx.send(PlayerEvent::Visual(false));
        let _ = self.event_tx.send(PlayerEvent::QueueFinished);
        self.emit_snapshot();
    }

    fn report_error(&mut self, context: &str, detail: String) {
        let _ = self.event_tx.send(PlayerEvent::Error { context: context.into(), detail });
    }

    /// Publica a fila atual do banco no painel da UI.
    fn emit_snapshot(&mut self) {
        match self.db.queue_snapshot() {
            Ok(upcoming) => {
                let _ = self.event_tx.send(PlayerEvent::QueueUpdated { upcoming });
            }
            Err(e) => {
                let _ = self
                    .event_tx
                    .send(PlayerEvent::Error { context: "Fila legacy".into(), detail: e });
            }
        }
    }
}

/// Sobe o player legacy com o motor libVLC. Assinatura espelha
/// `player::spawn` (mais a configuração do banco e o XID da janela para
/// o vídeo embutido). Sem libVLC no sistema: modo de validação.
pub fn spawn(
    cmd_rx: Receiver<PlayerCommand>,
    event_tx: Sender<PlayerEvent>,
    config: PgConfig,
    parent_xid: Option<u64>,
) {
    thread::Builder::new()
        .name("legacy-player".into())
        .spawn(move || {
            let db = match LegacyDb::connect(&config) {
                Ok(db) => db,
                Err(e) => {
                    let _ = event_tx.send(PlayerEvent::Error {
                        context: "Player legacy".into(),
                        detail: format!("sem conexão com o jukeboxtvdb: {e}"),
                    });
                    while let Ok(_cmd) = cmd_rx.recv() {} // drena o app
                    return;
                }
            };
            let engine = RealEngine::new(parent_xid);
            let (engine, signal_rx) = match engine {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = event_tx.send(PlayerEvent::Error {
                        context: "libVLC".into(),
                        detail: format!(
                            "{e}; seguindo no modo de validação (débito e fila no banco, \
                             sem mídia)"
                        ),
                    });
                    let _ = event_tx.send(PlayerEvent::Notice(
                        "libVLC ausente: modo de validação (dinheiro real, sem mídia)".into(),
                    ));
                    // O modo de validação abre a própria conexão:
                    // solta esta (runtime tokio embutido) antes.
                    #[allow(clippy::drop_non_drop)] // stub do harness não tem Drop
                    drop(db);
                    super::legacy_player::run(cmd_rx, event_tx, config);
                    return;
                }
            };
            let mut session = Session::new(db, engine, signal_rx, event_tx);
            session.run(&cmd_rx);
        })
        .expect("thread do player legacy");
}

// =============================================================================
// Testes (banco roteirizado + motor de teste — sem libVLC/PostgreSQL)
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::track;
    use std::collections::VecDeque;
    use std::sync::Arc;

    /// Banco roteirizado: as respostas saem em ordem; as chamadas ficam
    /// registradas para asserção.
    #[derive(Default)]
    struct ScriptedDb {
        dequeue: VecDeque<Result<Option<TrackInfo>, String>>,
        sortear: VecDeque<Result<Option<TrackInfo>, String>>,
        sistema: VecDeque<SystemRow>,
        enqueued: Vec<(i64, f64)>,
    }

    impl QueueDb for Arc<Mutex<ScriptedDb>> {
        fn enqueue(&mut self, midia_id: i64, custo: f64) -> Result<(), String> {
            self.lock().unwrap().enqueued.push((midia_id, custo));
            Ok(())
        }
        fn dequeue(&mut self) -> Result<Option<TrackInfo>, String> {
            self.lock().unwrap().dequeue.pop_front().unwrap_or(Ok(None))
        }
        fn sistema(&mut self) -> Result<SystemRow, String> {
            Ok(self.lock().unwrap().sistema.front().cloned().unwrap_or_default())
        }
        fn sortear_aleatoria(&mut self, _aleatoriovideo: bool) -> Result<Option<TrackInfo>, String> {
            match self.lock().unwrap().sortear.pop_front() {
                Some(result) => result,
                None => Ok(None),
            }
        }
        fn queue_snapshot(&mut self) -> Result<Vec<TrackInfo>, String> {
            Ok(Vec::new())
        }
    }

    /// Motor de teste: registra reproduções/paradas/volumes.
    #[derive(Default)]
    struct StubEngine {
        plays: Vec<(String, u8)>,
        stops: usize,
        volumes: Vec<u8>,
        fx: usize,
        fail_plays: bool,
    }

    impl Engine for Arc<Mutex<StubEngine>> {
        fn play(&mut self, path: &str, volume: u8) -> Result<(), String> {
            let mut engine = self.lock().unwrap();
            if engine.fail_plays {
                return Err("stub: falha simulada".into());
            }
            engine.plays.push((path.into(), volume));
            Ok(())
        }
        fn stop(&mut self) {
            self.lock().unwrap().stops += 1;
        }
        fn credit_fx(&mut self) {
            self.lock().unwrap().fx += 1;
        }
        fn set_volume(&mut self, volume: u8) {
            self.lock().unwrap().volumes.push(volume);
        }
    }

    /// Sobe a sessão numa thread com tick curto e devolve os canais.
    struct Harness {
        cmd_tx: Sender<PlayerCommand>,
        event_rx: Receiver<PlayerEvent>,
        signal_tx: Sender<VlcSignal>,
        db: Arc<Mutex<ScriptedDb>>,
        engine: Arc<Mutex<StubEngine>>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_system(SystemRow::default())
        }

        fn with_system(system: SystemRow) -> Self {
            let db = Arc::new(Mutex::new(ScriptedDb {
                sistema: VecDeque::from(vec![system]),
                ..ScriptedDb::default()
            }));
            let engine = Arc::new(Mutex::new(StubEngine::default()));
            let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
            let (event_tx, event_rx) = std::sync::mpsc::channel();
            let (signal_tx, signal_rx) = std::sync::mpsc::channel();
            let mut session = Session::new(db.clone(), engine.clone(), signal_rx, event_tx);
            session.tick = Duration::from_millis(10);
            session.retry_delay = Duration::from_millis(40);
            // Teste não espera minutos: o atraso vem sobrescrito.
            session.random_delay_override = Some(Duration::from_millis(40));
            let join = thread::spawn(move || session.run(&cmd_rx));
            Self { cmd_tx, event_rx, signal_tx, db, engine, join: Some(join) }
        }

        fn enqueue(&self, id: i64) {
            self.cmd_tx
                .send(PlayerCommand::Enqueue(track(id, "Faixa", "mp3")))
                .unwrap();
        }

        fn end_reached(&self) {
            self.signal_tx.send(VlcSignal::EndReached).unwrap();
        }

        /// Próximo evento relevante (pulando os de fundo: créditos/fila).
        fn next_event(&self) -> PlayerEvent {
            for _ in 0..64 {
                let event = self
                    .event_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("evento do player");
                if matches!(
                    event,
                    PlayerEvent::CreditsChanged | PlayerEvent::QueueUpdated { .. }
                ) {
                    continue;
                }
                return event;
            }
            panic!("fluxo de eventos excessivo");
        }

        /// Primeiro evento que casa com o predicado (pulando os de fundo).
        fn expect<F: Fn(&PlayerEvent) -> bool>(&self, pred: F) -> PlayerEvent {
            for _ in 0..64 {
                let event = self.next_event();
                if pred(&event) {
                    return event;
                }
            }
            panic!("evento esperado não chegou");
        }

        /// Consome eventos por um tempo garantindo que nada mais chega.
        fn assert_quiet(&self, millis: u64) {
            std::thread::sleep(Duration::from_millis(millis));
            while let Ok(event) = self.event_rx.try_recv() {
                if matches!(
                    event,
                    PlayerEvent::CreditsChanged | PlayerEvent::QueueUpdated { .. }
                ) {
                    continue;
                }
                panic!("evento inesperado durante o silêncio");
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = std::mem::replace(&mut self.cmd_tx, tx);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn started(id: i64) -> impl Fn(&PlayerEvent) -> bool {
        move |event| {
            matches!(event, PlayerEvent::TrackStarted { id: got, .. } if *got == id)
        }
    }

    fn previous_of(title: &str) -> impl Fn(&PlayerEvent) -> bool + '_ {
        move |event| matches!(event, PlayerEvent::Previous { title: got, .. } if got == title)
    }

    #[test]
    fn vlc_volume_inverts_the_perceptual_curve() {
        // volume_to_linear(50) = 0.5³ = 0.125 → recupera 50 (o Java
        // passava sistema.volume direto para a libVLC).
        assert_eq!(vlc_volume(0.125), 50);
        assert_eq!(vlc_volume(1.0), 100);
        assert_eq!(vlc_volume(0.0), 0);
        assert_eq!(vlc_volume(f64::NAN), 0);
        assert_eq!(vlc_volume(2.0), 100, "acima de 1 satura");
        assert_eq!(vlc_volume(-1.0), 0, "negativo satura em silêncio");
    }

    #[test]
    fn credit_fx_wav_is_well_formed_pcm() {
        let wav = credit_fx_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap());
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(riff_len, 36 + data_len);
        assert_eq!(wav.len() as u32, 44 + data_len);
        // PCM mono 16-bit 8 kHz.
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), FX_RATE);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        // Duas notas de 75 ms a 8 kHz = 1200 amostras.
        assert_eq!(data_len, 2 * 2 * FX_NOTE_MS * FX_RATE / 1000);
        // Sem clipping (o envelope limita a ~22%).
        let peak = wav[44..]
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).abs())
            .max()
            .unwrap();
        assert!(peak > 0 && peak < i16::MAX / 2);
    }

    #[test]
    fn first_enqueue_debits_and_plays() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue =
            VecDeque::from(vec![Ok(Some(track(55230, "Faixa", "mp3")))]);
        h.enqueue(55230);
        match h.expect(started(55230)) {
            PlayerEvent::TrackStarted { is_video, .. } => assert!(!is_video),
            _ => unreachable!("expect filtra pelo id"),
        }
        // O débito (e o registro_musicas, no cliente real) acontecem no
        // enqueue — a sessão só pede.
        assert_eq!(h.db.lock().unwrap().enqueued, vec![(55230, 1.0)]);
        let engine = h.engine.lock().unwrap();
        assert_eq!(engine.plays.len(), 1);
        assert!(engine.plays[0].0.ends_with("Faixa.mp3"));
        assert_eq!(engine.plays[0].1, 70, "volume padrão até o SetVolume chegar");
    }

    #[test]
    fn enqueue_while_playing_goes_to_the_queue() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![
            Ok(Some(track(1, "Primeira", "mp3"))),
            Ok(Some(track(2, "Segunda", "mp3"))),
        ]);
        h.enqueue(1);
        h.expect(started(1));
        h.enqueue(2);
        match h.expect(|event| matches!(event, PlayerEvent::Notice(_))) {
            PlayerEvent::Notice(message) => assert!(message.contains("Na fila")),
            _ => unreachable!("expect filtra Notice"),
        }
        // A segunda só toca quando a primeira terminar.
        assert_eq!(h.engine.lock().unwrap().plays.len(), 1);
    }

    #[test]
    fn end_reached_publishes_previous_and_advances() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![
            Ok(Some(track(1, "Primeira", "mp3"))),
            Ok(Some(track(2, "Segunda", "mp3"))),
        ]);
        h.enqueue(1);
        h.expect(started(1));
        h.end_reached();
        h.expect(previous_of("Primeira"));
        h.expect(started(2));
        assert_eq!(h.engine.lock().unwrap().plays.len(), 2);
    }

    #[test]
    fn last_track_finishes_the_queue() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![Ok(Some(track(7, "Única", "mp3")))]);
        h.enqueue(7);
        h.expect(started(7));
        h.end_reached();
        h.expect(previous_of("Única"));
        // Sem modo aleatório: overlay fechado e painel limpo.
        match h.expect(|event| matches!(event, PlayerEvent::Visual(_))) {
            PlayerEvent::Visual(active) => assert!(!active),
            _ => unreachable!("expect filtra Visual"),
        }
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        h.assert_quiet(80);
    }

    #[test]
    fn video_track_maps_the_overlay() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue =
            VecDeque::from(vec![Ok(Some(track(9, "Clipe", "mpeg")))]);
        h.enqueue(9);
        match h.expect(started(9)) {
            PlayerEvent::TrackStarted { is_video, .. } => assert!(is_video),
            _ => unreachable!("expect filtra pelo id"),
        }
        match h.expect(|event| matches!(event, PlayerEvent::Visual(_))) {
            PlayerEvent::Visual(active) => assert!(active, "clipe liga o overlay"),
            _ => unreachable!("expect filtra Visual"),
        }
    }

    #[test]
    fn random_mode_draws_after_the_silence_window() {
        let system = SystemRow {
            modoaleatorio: true,
            mininicioaleatorio: 15.0, // minutos — ignorado: override de teste
            aleatoriovideo: true,
            ..SystemRow::default()
        };
        let h = Harness::with_system(system);
        h.db.lock().unwrap().dequeue =
            VecDeque::from(vec![Ok(Some(track(1, "Paga", "mp3"))), Ok(None)]);
        h.db.lock().unwrap().sortear =
            VecDeque::from(vec![Ok(Some(track(2, "Sorteada", "mp3")))]);
        h.enqueue(1);
        h.expect(started(1));
        h.end_reached();
        h.expect(previous_of("Paga"));
        // Ocio imediato (painel limpa)...
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        // ...e o sorteio chega depois da janela de silêncio.
        h.expect(started(2));
        let db = h.db.lock().unwrap();
        // A sorteada não foi debitada (só a seleção paga passa pelo enqueue).
        assert_eq!(db.enqueued, vec![(1, 1.0)]);
    }

    #[test]
    fn random_mode_stays_idle_when_off() {
        let system = SystemRow {
            modoaleatorio: false,
            mininicioaleatorio: 0.0,
            ..SystemRow::default()
        };
        let h = Harness::with_system(system);
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![Ok(None)]);
        // Isca: se o sorteio rodasse, consumiria esta faixa.
        h.db.lock().unwrap().sortear =
            VecDeque::from(vec![Ok(Some(track(99, "Isca", "mp3")))]);
        h.enqueue(1);
        match h.expect(|event| matches!(event, PlayerEvent::Visual(_))) {
            PlayerEvent::Visual(active) => assert!(!active),
            _ => unreachable!("expect filtra Visual"),
        }
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        h.assert_quiet(120);
        assert_eq!(
            h.db.lock().unwrap().sortear.len(),
            1,
            "o sorteio nem rodou: modo desligado"
        );
    }

    #[test]
    fn party_mode_does_not_trigger_the_random_draw() {
        // modofesta = free play na SELEÇÃO (c() do original); não tem
        // relação com o modo aleatório.
        let system = SystemRow {
            modoaleatorio: false,
            modofesta: true,
            mininicioaleatorio: 0.0,
            ..SystemRow::default()
        };
        let h = Harness::with_system(system);
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![Ok(None)]);
        h.db.lock().unwrap().sortear =
            VecDeque::from(vec![Ok(Some(track(3, "Isca", "mp3")))]);
        h.enqueue(1);
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        h.assert_quiet(120);
        assert_eq!(h.db.lock().unwrap().sortear.len(), 1, "festa não sorteia");
    }

    #[test]
    fn random_draw_rechecks_the_mode_after_the_window() {
        // O operador desligou o modo durante o silêncio: nada toca.
        let system = SystemRow {
            modoaleatorio: true,
            mininicioaleatorio: 0.0,
            ..SystemRow::default()
        };
        let h = Harness::with_system(system);
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![Ok(None)]);
        h.db.lock().unwrap().sortear =
            VecDeque::from(vec![Ok(Some(track(4, "Isca", "mp3")))]);
        // ...mas a releitura devolve o modo DESLIGADO.
        h.db.lock().unwrap().sistema = VecDeque::from(vec![SystemRow {
            modoaleatorio: false,
            ..SystemRow::default()
        }]);
        h.enqueue(1);
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        h.assert_quiet(120);
        assert_eq!(h.db.lock().unwrap().sortear.len(), 1, "re-conferiu e desistiu");
    }

    #[test]
    fn skip_stops_and_advances() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![
            Ok(Some(track(1, "Primeira", "mp3"))),
            Ok(Some(track(2, "Segunda", "mp3"))),
        ]);
        h.enqueue(1);
        h.expect(started(1));
        h.cmd_tx.send(PlayerCommand::SkipTrack).unwrap();
        h.expect(previous_of("Primeira"));
        h.expect(started(2));
        assert_eq!(h.engine.lock().unwrap().stops, 1, "o skip para a faixa atual");
    }

    #[test]
    fn skip_on_idle_discards_the_queue_head() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue =
            VecDeque::from(vec![Ok(Some(track(1, "Descartada", "mp3")))]);
        h.cmd_tx.send(PlayerCommand::SkipTrack).unwrap();
        // Nada toca: a cabeça foi descartada (o histórico já contou na
        // seleção — pular não "devolve" o registro, como no original).
        h.assert_quiet(80);
        assert!(h.engine.lock().unwrap().plays.is_empty());
    }

    #[test]
    fn set_volume_maps_the_cubic_scale() {
        let h = Harness::new();
        h.cmd_tx.send(PlayerCommand::SetVolume(0.125)).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(h.engine.lock().unwrap().volumes, vec![50]);
    }

    #[test]
    fn credit_fx_goes_to_the_engine() {
        let h = Harness::new();
        h.cmd_tx.send(PlayerCommand::CreditFx).unwrap();
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(h.engine.lock().unwrap().fx, 1);
    }

    #[test]
    fn broken_files_advance_until_idle() {
        let h = Harness::new();
        h.engine.lock().unwrap().fail_plays = true;
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![
            Ok(Some(track(1, "Quebrada", "mp3"))),
            Ok(Some(track(2, "Ausente", "mp3"))),
            Ok(None),
        ]);
        h.enqueue(1);
        h.expect(|event| matches!(event, PlayerEvent::QueueFinished));
        let engine = h.engine.lock().unwrap();
        assert!(engine.plays.is_empty());
        drop(engine);
        // As duas saíram da fila (pagas): o advance as consumiu.
        assert!(h.db.lock().unwrap().dequeue.is_empty());
    }

    #[test]
    fn dequeue_error_keeps_the_queue_for_retry() {
        let h = Harness::new();
        h.db.lock().unwrap().dequeue = VecDeque::from(vec![
            Err("banco sumiu".into()),
            Ok(Some(track(1, "Recuperada", "mp3"))),
        ]);
        h.enqueue(1);
        match h.expect(|event| matches!(event, PlayerEvent::Error { .. })) {
            PlayerEvent::Error { context, .. } => assert_eq!(context, "Fila legacy"),
            _ => unreachable!("expect filtra Error"),
        }
        // Sem QueueFinished: a fila não foi perdida — o tick re-tenta.
        h.expect(started(1));
    }

    #[test]
    fn insufficient_balance_is_a_notice_not_an_error() {
        struct RefusingDb;
        impl QueueDb for RefusingDb {
            fn enqueue(&mut self, _: i64, _: f64) -> Result<(), String> {
                Err("saldo insuficiente: atual 0.00, custo 1.00".into())
            }
            fn dequeue(&mut self) -> Result<Option<TrackInfo>, String> {
                Ok(None)
            }
            fn sistema(&mut self) -> Result<SystemRow, String> {
                Ok(SystemRow::default())
            }
            fn sortear_aleatoria(&mut self, _: bool) -> Result<Option<TrackInfo>, String> {
                Ok(None)
            }
            fn queue_snapshot(&mut self) -> Result<Vec<TrackInfo>, String> {
                Ok(Vec::new())
            }
        }
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (_signal_tx, signal_rx) = std::sync::mpsc::channel();
        let engine = Arc::new(Mutex::new(StubEngine::default()));
        let mut session = Session::new(RefusingDb, engine, signal_rx, event_tx);
        session.tick = Duration::from_millis(5);
        let join = thread::spawn(move || session.run(&cmd_rx));
        cmd_tx.send(PlayerCommand::Enqueue(track(1, "Faixa", "mp3"))).unwrap();
        let notice = loop {
            let event = event_rx.recv_timeout(Duration::from_secs(2)).expect("evento");
            if matches!(
                event,
                PlayerEvent::CreditsChanged | PlayerEvent::QueueUpdated { .. }
            ) {
                continue;
            }
            break event;
        };
        match notice {
            PlayerEvent::Notice(message) => {
                assert!(message.contains("insuficiente"), "mensagem: {message}")
            }
            _ => panic!("esperava Notice de saldo insuficiente"),
        }
        drop(cmd_tx);
        join.join().unwrap();
    }
}
