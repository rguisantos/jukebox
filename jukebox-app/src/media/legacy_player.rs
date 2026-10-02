//! Player do perfil legacy — **modo de validação** (sem mídia).
//!
//! Este player assume o canal `PlayerCommand`/`PlayerEvent` no lugar do
//! GStreamer (que não existe na base antiga — Ubuntu 11.04 i686) e executa
//! tudo o que independe de áudio/vídeo:
//!
//! - **débito + fila reais**: `Enqueue` debita 1 crédito e grava a linha
//!   em `filamidia` na mesma transação (`db::legacy_pg`). A fila do painel
//!   é o próprio banco — inclusive itens deixados pelo Java;
//! - **`SkipTrack`** descarta o início da fila (`dequeue`) sem registrar
//!   execução — igual a "pular" sem tocar;
//! - **persistência de volume** fica com o serviço (`VolumeClosed` grava
//!   `sistema.volume`): este player não recebe o valor linear por acaso
//!   nenhum, então não há round-trip com perda;
//! - `SetVolume` e efeitos visuais são no-op (sem áudio/vídeo).
//!
//! Hoje este é o **fallback** do player libVLC (`legacy_vlc`): quando a
//! biblioteca não carrega no sistema, o app segue nele — dinheiro e fila
//! reais, sem mídia. Na máquina em campo ele valida o caminho completo:
//! moeda → saldo → seleção → débito → `filamidia` (o Java em rollback
//! tocaria a fila intacta).

use super::player::{PlayerCommand, PlayerEvent};
use crate::db::legacy_pg::client::LegacyDb;
use crate::db::legacy_pg::PgConfig;
use std::sync::mpsc::{Receiver, Sender};

/// Preço por seleção: constante do sistema original (1 crédito).
const CUSTO_POR_MUSICA: f64 = 1.0;

/// Executa o loop de validação na thread atual (o `legacy_vlc::spawn`
/// chama quando a libVLC não está disponível — por isso `run`, sem
/// thread própria: quem decide é o chamador).
pub fn run(cmd_rx: Receiver<PlayerCommand>, event_tx: Sender<PlayerEvent>, config: PgConfig) {
    let mut db = match LegacyDb::connect(&config) {
        Ok(db) => db,
        Err(e) => {
            let _ = event_tx.send(PlayerEvent::Error {
                context: "Player legacy".into(),
                detail: format!("sem conexão com o jukeboxtvdb: {e}"),
            });
            // Drena os comandos para o app não travar no envio.
            while let Ok(_cmd) = cmd_rx.recv() {}
            return;
        }
    };

    // Fila residual (ex.: deixada pelo Java) já aparece no painel,
    // e o saldo da UI é lido na hora.
    emit_snapshot(&mut db, &event_tx);
    let _ = event_tx.send(PlayerEvent::CreditsChanged);
    let _ = event_tx.send(PlayerEvent::Notice(
        "Perfil legacy (validação): créditos e fila no banco; \
         mídia exige o libVLC"
            .into(),
    ));

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            PlayerCommand::Enqueue(track) => match db.enqueue(track.id, CUSTO_POR_MUSICA) {
                Ok(()) => {
                    log::info!(
                        "Fila legacy: mídia {} (+{CUSTO_POR_MUSICA} crédito)",
                        track.title
                    );
                    let _ = event_tx.send(PlayerEvent::Notice(format!(
                        "Música na fila: {} (modo validação)",
                        track.title
                    )));
                    let _ = event_tx.send(PlayerEvent::CreditsChanged);
                    emit_snapshot(&mut db, &event_tx);
                }
                Err(e) => {
                    let message = if e.contains("saldo insuficiente") {
                        "Créditos insuficientes. Insira saldo para escolher uma música."
                    } else {
                        e.as_str()
                    };
                    let _ = event_tx.send(PlayerEvent::Notice(message.into()));
                }
            },
            PlayerCommand::SkipTrack => match db.dequeue() {
                Ok(Some(_)) => {
                    let _ = event_tx.send(PlayerEvent::Notice(
                        "Início da fila descartado (validação sem mídia)".into(),
                    ));
                    emit_snapshot(&mut db, &event_tx);
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = event_tx.send(PlayerEvent::Error {
                        context: "Fila legacy".into(),
                        detail: e,
                    });
                }
            },
            PlayerCommand::Storage(_) => {
                // O menu do operador fica aguardando a resposta —
                // responder vazio libera o spinner (storage_busy).
                let _ = event_tx.send(PlayerEvent::Storage {
                    rows: Vec::new(),
                    catalog: None,
                    message: "Gerenciamento de armazenamento não se aplica \
                              ao perfil legacy"
                        .into(),
                    restore: false,
                });
            }
            // Sem áudio/vídeo nesta fase: nada a ajustar.
            PlayerCommand::SetVolume(_) => {}
            PlayerCommand::CreditFx => {}
            PlayerCommand::Activity => {}
            PlayerCommand::Operator(_) => {}
            PlayerCommand::HideVideo => {}
            PlayerCommand::RestoreVideo => {}
            PlayerCommand::ReloadSettings => {}
            PlayerCommand::RefreshBackgrounds => {}
        }
    }
}

/// Publica a fila atual do banco no painel da UI.
fn emit_snapshot(db: &mut LegacyDb, event_tx: &Sender<PlayerEvent>) {
    match db.queue_snapshot() {
        Ok(upcoming) => {
            let _ = event_tx.send(PlayerEvent::QueueUpdated { upcoming });
        }
        Err(e) => {
            let _ = event_tx.send(PlayerEvent::Error {
                context: "Fila legacy".into(),
                detail: e,
            });
        }
    }
}
