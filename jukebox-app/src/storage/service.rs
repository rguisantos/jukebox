//! Owns the SQLite connection and processes domain commands in order.
//! Neither commands nor events depend on Slint; the UI bridge lives in main.rs.
use crate::{db::{Database, PendingPix}, operator, settings::Settings};
use crate::state::models::{AlbumInfo, GenreInfo, TrackInfo};
use std::{sync::mpsc::{self, Receiver, Sender}, thread, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

const VOLUME_CONFIG_KEY: &str = "volume";

#[derive(Debug)]
enum DbCommand {
    RefreshCredits,
    CashPulse,
    AcceptPix { machine_id: String, txid: String, credits: u32 },
    AcceptPixLogic { machine: String, operation: String, credits: u32, reply: Sender<Result<(),String>> },
    PendingPixLogic { machine: String, reply: Sender<Result<Vec<(String,u32)>,String>> },
    ConfirmPixLogic { machine: String, operation: String, reply: Sender<Result<(),String>> },
    RememberPix(PendingPix, Sender<Result<(), String>>),
    PendingPix(Sender<Result<Vec<PendingPix>, String>>),
    ForgetPix(String, String),
    UnlockOperator,
    LockOperator,
    LoadSettings,
    SaveSettings(operator::SettingsInput),
    CycleGenre,
    RequestPlay(TrackInfo),
    SetVolume(u32),
    QueryOperatorStats,
    SetSongPrice(u32),
    ToggleGenre(String),
    ResetPartial,
    ResetCredits,
    SetRecentDays(u32),
}

pub enum DbEvent {
    Balance(u32),
    CreditAccepted { balance: u32, added: u32 },
    Toast { message: String, kind: i32 },
    SettingsLoaded(Settings),
    SettingsSaved(Settings),
    CycleGenre(Vec<AlbumInfo>),
    Catalog(Vec<AlbumInfo>),
    Enqueue(TrackInfo),
    OperatorStats { partial: i64, absolute: i64, revenue: Result<i64, String>, price: u32,
        recent_days: u32, genres: Vec<GenreInfo> },
    SongPrice(u32),
    PartialReset,
}

#[derive(Clone)]
pub struct DbHandle(Sender<DbCommand>);
impl DbHandle {
    fn send(&self, cmd: DbCommand) -> Result<(), String> {
        self.0.send(cmd).map_err(|e| e.to_string())
    }
    pub fn refresh_credits(&self) -> Result<(), String> { self.send(DbCommand::RefreshCredits) }
    pub fn cash_pulse(&self) -> Result<(), String> { self.send(DbCommand::CashPulse) }
    pub fn accept_pix(&self, machine_id: String, txid: String, credits: u32) -> Result<(), String> {
        self.send(DbCommand::AcceptPix { machine_id, txid, credits })
    }
    pub fn accept_pixlogic(&self, machine: String, operation: String, credits: u32) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.send(DbCommand::AcceptPixLogic { machine, operation, credits, reply })?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
    }
    pub fn pending_pixlogic(&self, machine: String) -> Result<Vec<(String,u32)>,String> {
        let (reply, rx) = mpsc::channel();
        self.send(DbCommand::PendingPixLogic { machine, reply })?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
    }
    pub fn confirm_pixlogic(&self, machine: String, operation: String) -> Result<(),String> {
        let (reply, rx) = mpsc::channel();
        self.send(DbCommand::ConfirmPixLogic { machine, operation, reply })?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
    }
    pub fn remember_pix(&self, item: PendingPix) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(DbCommand::RememberPix(item, tx))?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
    }
    pub fn pending_pix(&self) -> Result<Vec<PendingPix>, String> {
        let (tx, rx) = mpsc::channel();
        self.send(DbCommand::PendingPix(tx))?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|e| e.to_string())?
    }
    pub fn forget_pix(&self, machine_id: String, txid: String) -> Result<(), String> {
        self.send(DbCommand::ForgetPix(machine_id, txid))
    }
    pub fn unlock_operator(&self) -> Result<(), String> { self.send(DbCommand::UnlockOperator) }
    pub fn lock_operator(&self) -> Result<(), String> { self.send(DbCommand::LockOperator) }
    pub fn load_settings(&self) -> Result<(), String> { self.send(DbCommand::LoadSettings) }
    pub fn save_settings(&self, input: operator::SettingsInput) -> Result<(), String> { self.send(DbCommand::SaveSettings(input)) }
    pub fn cycle_genre(&self) -> Result<(), String> { self.send(DbCommand::CycleGenre) }
    pub fn request_play(&self, track: TrackInfo) -> Result<(), String> { self.send(DbCommand::RequestPlay(track)) }
    pub fn set_volume(&self, volume: u32) -> Result<(), String> { self.send(DbCommand::SetVolume(volume)) }
    pub fn operator_stats(&self) -> Result<(), String> { self.send(DbCommand::QueryOperatorStats) }
    pub fn set_song_price(&self, price: u32) -> Result<(), String> { self.send(DbCommand::SetSongPrice(price)) }
    pub fn toggle_genre(&self, genre: String) -> Result<(), String> { self.send(DbCommand::ToggleGenre(genre)) }
    pub fn reset_partial(&self) -> Result<(), String> { self.send(DbCommand::ResetPartial) }
    pub fn reset_credits(&self) -> Result<(), String> { self.send(DbCommand::ResetCredits) }
    pub fn set_recent_days(&self, days: u32) -> Result<(), String> { self.send(DbCommand::SetRecentDays(days)) }
}

fn toast(events: &Sender<DbEvent>, message: impl Into<String>, kind: i32) {
    let _ = events.send(DbEvent::Toast { message: message.into(), kind });
}

/// The initial DB connection is moved to its only command consumer after boot.
pub fn spawn(db: Database) -> (DbHandle, Receiver<DbEvent>) {
    let persistent = std::env::var("JUKEBOX_DATA_PERSISTENT").as_deref() != Ok("0");
    spawn_with_persistence(db, persistent)
}

fn spawn_with_persistence(mut db: Database, persistent: bool) -> (DbHandle, Receiver<DbEvent>) {
    let (tx, rx) = mpsc::channel();
    let (events, responses) = mpsc::channel();
    thread::Builder::new().name("storage-service".into()).spawn(move || {
        let mut unlocked = false;
        while let Ok(cmd) = rx.recv() {
            match cmd {
                DbCommand::RefreshCredits => {
                    if let Ok(balance) = db.get_credits() { let _ = events.send(DbEvent::Balance(balance)); }
                }
                DbCommand::CashPulse => {
                    if !persistent {
                        toast(&events, "Armazenamento temporário: crédito em dinheiro desativado", 2);
                        continue;
                    }
                    let result = db.settings().and_then(|s| {
                        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
                        db.accept_money(s.coin_cents, &format!("coin-{}-{stamp}", std::process::id()))
                    });
                    match result {
                        Ok((balance, added)) => { let _ = events.send(DbEvent::CreditAccepted { balance, added }); }
                        Err(e) => toast(&events, format!("Falha ao registrar saldo: {e}"), 2),
                    }
                }
                DbCommand::AcceptPix { machine_id, txid, credits } => {
                    if !persistent {
                        toast(&events, "Armazenamento temporário: Pix desativado", 2);
                        continue;
                    }
                    match db.accept_pix(&machine_id, &txid, credits) {
                        Ok((balance, true)) => { let _ = events.send(DbEvent::CreditAccepted { balance, added: credits }); }
                        Ok((balance, false)) => { log::info!("PIX repetido ignorado: máquina {machine_id}, txid {txid}"); let _ = events.send(DbEvent::Balance(balance)); }
                        Err(e) => toast(&events, format!("Falha ao registrar PIX: {e}"), 2),
                    }
                }
                DbCommand::AcceptPixLogic { machine, operation, credits, reply } => {
                    if !persistent {
                        let _ = reply.send(Err("Armazenamento temporário: PixLogic desativado".into()));
                        continue;
                    }
                    let result = db.accept_pixlogic(&machine, &operation, credits).map_err(|e| e.to_string());
                    if let Ok((balance, inserted)) = &result {
                        if *inserted { let _ = events.send(DbEvent::CreditAccepted { balance: *balance, added: credits }); }
                        else { let _ = events.send(DbEvent::Balance(*balance)); }
                    }
                    let _ = reply.send(result.map(|_| ()));
                }
                DbCommand::PendingPixLogic { machine, reply } => {
                    let _ = reply.send(db.pending_pixlogic(&machine).map_err(|e| e.to_string()));
                }
                DbCommand::ConfirmPixLogic { machine, operation, reply } => {
                    let _ = reply.send(db.confirm_pixlogic(&machine, &operation).map_err(|e| e.to_string()));
                }
                DbCommand::RememberPix(item, reply) => {
                    let _ = reply.send(db.remember_pix(&item).map_err(|e| e.to_string()));
                }
                DbCommand::PendingPix(reply) => {
                    let _ = reply.send(db.pending_pix().map_err(|e| e.to_string()));
                }
                DbCommand::ForgetPix(machine_id, txid) => {
                    if let Err(e) = db.forget_pix(&machine_id, &txid) {
                        log::error!("Falha ao remover Pix expirado {txid}: {e}");
                    }
                }
                DbCommand::UnlockOperator => unlocked = true,
                DbCommand::LockOperator => unlocked = false,
                DbCommand::LoadSettings => match db.settings() {
                    Ok(settings) => { let _ = events.send(DbEvent::SettingsLoaded(settings)); }
                    Err(e) => toast(&events, format!("Falha ao carregar configurações: {e}"), 2),
                },
                DbCommand::SaveSettings(input) => {
                    if !unlocked { toast(&events, "Abra o menu do operador para alterar configurações", 2); continue; }
                    let result = db.settings().map_err(|e| e.to_string())
                        .and_then(|old| operator::parse(input, old))
                        .and_then(|settings| { db.save_settings(&settings).map_err(|e| e.to_string())?; Ok(settings) });
                    match result {
                        Ok(settings) => { let _ = events.send(DbEvent::SettingsSaved(settings)); toast(&events, "Configurações salvas", 1); }
                        Err(e) => toast(&events, e, 2),
                    }
                }
                DbCommand::CycleGenre => match db.get_albums() {
                    Ok(albums) => { let _ = events.send(DbEvent::CycleGenre(albums)); }
                    Err(e) => toast(&events, format!("Falha ao filtrar gêneros: {e}"), 2),
                },
                DbCommand::RequestPlay(track) => { let _ = events.send(DbEvent::Enqueue(track)); }
                DbCommand::SetVolume(volume) => {
                    if let Err(e) = db.set_config_i64(VOLUME_CONFIG_KEY, volume as i64) { log::error!("Falha ao persistir o volume: {e}"); }
                }
                DbCommand::QueryOperatorStats => {
                    let partial = db.get_partial_coins().unwrap_or(0);
                    let absolute = db.get_absolute_coins().unwrap_or(0);
                    let revenue = db.get_total_receipts_cents().map_err(|e| e.to_string());
                    let price = db.get_song_price().unwrap_or(1);
                    let recent_days = db.get_recent_days().unwrap_or(30);
                    let genres = db.get_genres().unwrap_or_default();
                    let _ = events.send(DbEvent::OperatorStats { partial, absolute, revenue, price, recent_days, genres });
                }
                DbCommand::SetSongPrice(price) => match db.set_song_price(price) {
                    Ok(()) => {
                        let _ = events.send(DbEvent::SongPrice(price));
                        toast(&events, format!("Preço atualizado: {price} crédito{}", if price == 1 { "" } else { "s" }), 1);
                    }
                    Err(e) => { log::error!("Falha ao gravar preço: {e}"); toast(&events, "Erro ao salvar o preço", 2); }
                },
                DbCommand::ToggleGenre(genre) => match db.toggle_genre_block(&genre) {
                    Ok(blocked) => {
                        log::info!("Gênero '{genre}' {}", if blocked { "BLOQUEADO" } else { "liberado" });
                        match db.get_albums() {
                            Ok(albums) => { let _ = events.send(DbEvent::Catalog(albums)); }
                            Err(e) => toast(&events, format!("Falha ao recarregar catálogo: {e}"), 2),
                        }
                    }
                    Err(e) => { log::error!("Falha ao alternar gênero: {e}"); toast(&events, "Erro ao bloquear gênero", 2); }
                },
                DbCommand::ResetPartial => match db.reset_partial_coins() {
                    Ok(()) => { let _ = events.send(DbEvent::PartialReset); toast(&events, "Caixa parcial zerado", 1); }
                    Err(e) => { log::error!("Falha ao zerar caixa: {e}"); toast(&events, "Erro ao zerar caixa", 2); }
                },
                DbCommand::ResetCredits => match db.reset_current_credits() {
                    Ok(()) => { let _ = events.send(DbEvent::Balance(0)); toast(&events, "Créditos atuais zerados", 1); }
                    Err(e) => { log::error!("Falha ao zerar créditos: {e}"); toast(&events, "Erro ao zerar créditos", 2); }
                },
                DbCommand::SetRecentDays(days) => match db.set_recent_days(days) {
                    Ok(()) => {
                        toast(&events, format!("Dias recentes: {days}"), 1);
                        match db.get_albums() {
                            Ok(albums) => { let _ = events.send(DbEvent::Catalog(albums)); }
                            Err(e) => toast(&events, format!("Falha ao recarregar catálogo: {e}"), 2),
                        }
                    }
                    Err(e) => { log::error!("Falha ao salvar dias recentes: {e}"); toast(&events, "Erro ao salvar dias recentes", 2); }
                },
            }
        }
    }).expect("storage thread");
    (DbHandle(tx), responses)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cash_is_committed_before_player_receives_selection() {
        let db = Database::in_memory();
        let (handle, rx) = spawn(db);
        let track = TrackInfo { id: 1, title: "Faixa".into(), artist: "Artista".into(),
            album: "Álbum".into(), file_path: "/test.mp3".into(), file_type: "mp3".into(), genre: "Rock".into() };
        handle.cash_pulse().unwrap();
        handle.request_play(track).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::CreditAccepted { balance: 1, added: 1 }));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Enqueue(t) if t.genre == "Rock"));
    }

    #[test]
    fn temporary_storage_rejects_every_paid_credit_source() {
        let (handle, events) = spawn_with_persistence(Database::in_memory(), false);
        handle.cash_pulse().unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Toast { kind: 2, .. }));
        handle.accept_pix("machine".into(), "payment".into(), 2).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Toast { kind: 2, .. }));
        assert!(handle.accept_pixlogic("machine".into(), "operation".into(), 2).is_err());
        handle.refresh_credits().unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Balance(0)));
    }

    #[test]
    fn settings_require_an_active_operator_session() {
        let db = Database::in_memory();
        let (handle, rx) = spawn(db);
        let input = || operator::SettingsInput {
            base_cents: "100".into(), base_credits: "1".into(),
            pack_cents: "500".into(), pack_credits: "6".into(),
            large_cents: "1000".into(), large_credits: "15".into(),
            coin_cents: "100".into(), attract_minutes: "5".into(),
            low_disk_mib: "500".into(), free_play: true,
        };
        handle.save_settings(input()).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Toast { kind: 2, .. }));
        handle.load_settings().unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::SettingsLoaded(s) if !s.free_play));
        handle.unlock_operator().unwrap();
        handle.save_settings(input()).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::SettingsSaved(s) if s.free_play));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Toast { kind: 1, .. }));
        handle.lock_operator().unwrap();
        handle.save_settings(input()).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), DbEvent::Toast { kind: 2, .. }));
    }
}
