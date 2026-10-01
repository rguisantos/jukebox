//! Serviço de persistência do perfil legacy (fase 1).
//!
//! Atende **o mesmo protocolo `DbCommand`/`DbEvent`** do serviço SQLite —
//! `main.rs` não muda de forma: só a origem dos dados. Atrás dos comandos,
//! este serviço fala com o PostgreSQL `jukeboxtvdb` que o sistema Java
//! usava em campo (`db::legacy_pg`):
//!
//! - créditos: `sistema.creditos` + `registro_creditos` (transações
//!   `FOR UPDATE`, ids `max+1` como no original);
//! - catálogo: `estilo`/`artista`/`disco`/`midia` — publicado no boot em
//!   vez da varredura de arquivos do perfil modern;
//! - preços: **não existem no banco original** — o Java debitava 1 crédito
//!   por música (constante [`CUSTO_POR_MUSICA`]);
//! - débito e fila: ficam com o player (dono da fila `filamidia`, como o
//!   GStreamer é dono da fila SQLite no perfil modern); este serviço
//!   apenas repassa o `RequestPlay`.
//!
//! O que não tem equivalente no schema original (pacotes de créditos,
//! preço configurável, dias recentes em tabela própria) responde com toast
//! informativo — nada quebra, nada é gravado em tabelas que o Java não
//! conhece.

use super::service::{DbCommand, DbEvent, DbHandle};
use crate::db::legacy_pg::client::LegacyDb;
use crate::db::legacy_pg::{dias_lancamento_display, saldo_display, volume_display};
use crate::db::PendingPix;
use crate::legacy_keys::LegacyKeys;
use crate::settings::Settings;
use crate::state::models::VOLUME_DEFAULT;
use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Preço por música no perfil legacy: constante do sistema original (a
/// tabela `sistema` não tem coluna de preço — o Java debitava sempre 1).
pub const CUSTO_POR_MUSICA: f64 = 1.0;

/// Valores de inicialização lidos da linha `sistema` — o equivalente
/// legacy das leituras que o perfil modern faz no SQLite antes de abrir
/// a janela.
pub struct BootState {
    /// O menu de configurações do app modern não se aplica ao schema
    /// original; serve apenas para renderizar o formulário (leitura).
    pub settings: Settings,
    pub credits: u32,
    pub price: u32,
    pub volume: u32,
    pub recent_days: u32,
    pub keys: LegacyKeys,
}

/// Lê a linha `sistema` e deriva os valores de boot do perfil legacy.
/// Chamado antes da janela abrir — um erro aqui derruba o app (o banco
/// local do jukebox é pré-requisito, como era para o Java).
pub fn boot_state(db: &mut LegacyDb) -> Result<BootState, String> {
    let row = db.sistema()?;
    let volume = volume_display(row.volume, row.maxvolume);
    Ok(BootState {
        settings: Settings::default(),
        credits: saldo_display(row.creditos),
        price: CUSTO_POR_MUSICA as u32,
        // volume 0/nulo no banco vira o padrão do app: um jukebox mudo
        // em campo parece máquina quebrada (o Java nunca deixou 0).
        volume: if volume > 0 { volume } else { VOLUME_DEFAULT },
        recent_days: dias_lancamento_display(row.diaslancamento),
        keys: row.legacy_keys(),
    })
}

fn toast(events: &Sender<DbEvent>, message: impl Into<String>, kind: i32) {
    let _ = events.send(DbEvent::Toast {
        message: message.into(),
        kind,
    });
}

/// Sobe a thread de serviço. Publica saldo, catálogo e gêneros logo no
/// início (substitui a varredura do scanner) e então atende comandos na
/// ordem, como o serviço SQLite.
pub fn spawn(mut db: LegacyDb) -> (DbHandle, Receiver<DbEvent>) {
    let (tx, rx) = mpsc::channel();
    let (events, responses) = mpsc::channel();
    thread::Builder::new()
        .name("legacy-storage".into())
        .spawn(move || {
            let mut unlocked = false;
            // Idempotência PixLogic da sessão: o ESP pode reentregar a
            // mesma operação após reconexão. O perfil modern persiste a
            // pendência no SQLite; aqui a janela de retomada vale apenas
            // enquanto o processo vive (o PixLogic re-sincroniza sozinho
            // ao reconectar — perda aceitável na fase 1, sem tocar o schema).
            let mut pixlogic_seen: HashSet<(String, String)> = HashSet::new();

            // ── estado inicial (no lugar do scanner) ──
            match db.balance() {
                Ok(balance) => {
                    let _ = events.send(DbEvent::Balance(saldo_display(balance)));
                }
                Err(e) => toast(&events, format!("Falha ao ler saldo: {e}"), 2),
            }
            let recent = db
                .sistema()
                .map(|row| row.diaslancamento.max(0) as f64)
                .unwrap_or(30.0);
            match db.load_catalog(recent) {
                Ok(albums) => {
                    log::info!(
                        "Perfil legacy: catálogo publicado ({} discos).",
                        albums.len()
                    );
                    let _ = events.send(DbEvent::Catalog(albums));
                }
                Err(e) => toast(&events, format!("Falha ao carregar catálogo: {e}"), 2),
            }
            match db.estilos() {
                Ok(genres) => {
                    let mut names = vec![String::new()];
                    names.extend(genres.iter().filter(|g| !g.blocked).map(|g| g.name.clone()));
                    let _ = events.send(DbEvent::GenreOptions(names));
                }
                Err(e) => toast(&events, format!("Falha ao listar gêneros: {e}"), 2),
            }

            while let Ok(cmd) = rx.recv() {
                match cmd {
                    DbCommand::RefreshCredits => match db.balance() {
                        Ok(balance) => {
                            let _ = events.send(DbEvent::Balance(saldo_display(balance)));
                        }
                        Err(e) => toast(&events, format!("Falha ao ler saldo: {e}"), 2),
                    },
                    DbCommand::CashPulse => {
                        // O PostgreSQL local É a persistência — não existe
                        // a condição de armazenamento temporário do /dados.
                        let before = db.balance().unwrap_or(0.0);
                        match db.entrada_moeda() {
                            Ok(balance) => {
                                let added = saldo_display((balance - before).max(0.0));
                                let _ = events.send(DbEvent::CreditAccepted {
                                    balance: saldo_display(balance),
                                    added,
                                });
                            }
                            Err(e) => toast(&events, format!("Falha ao registrar moeda: {e}"), 2),
                        }
                    }
                    DbCommand::AcceptPix {
                        machine_id,
                        txid,
                        credits,
                    } => match db.entrada(credits as f64) {
                        Ok(balance) => {
                            log::info!(
                                "PIX aceito (máquina {machine_id}, txid {txid}): +{credits}"
                            );
                            let _ = events.send(DbEvent::CreditAccepted {
                                balance: saldo_display(balance),
                                added: credits,
                            });
                        }
                        Err(e) => toast(&events, format!("Falha ao registrar PIX: {e}"), 2),
                    },
                    DbCommand::AcceptPixLogic {
                        machine,
                        operation,
                        credits,
                        reply,
                    } => {
                        let result =
                            if pixlogic_seen.contains(&(machine.clone(), operation.clone())) {
                                log::info!(
                                "PixLogic repetido ignorado: {machine}/{operation} (+{credits})"
                            );
                                db.balance().map(|balance| {
                                    let _ = events.send(DbEvent::Balance(saldo_display(balance)));
                                })
                            } else {
                                db.entrada(credits as f64).map(|balance| {
                                    pixlogic_seen.insert((machine.clone(), operation.clone()));
                                    let _ = events.send(DbEvent::CreditAccepted {
                                        balance: saldo_display(balance),
                                        added: credits,
                                    });
                                })
                            };
                        let _ = reply.send(result);
                    }
                    DbCommand::PendingPixLogic { machine: _, reply } => {
                        // Sem persistência própria no schema original: a
                        // retomada pós-restart fica a cargo do PixLogic.
                        let _ = reply.send(Ok(Vec::new()));
                    }
                    DbCommand::ConfirmPixLogic {
                        machine,
                        operation,
                        reply,
                    } => {
                        pixlogic_seen.remove(&(machine, operation));
                        let _ = reply.send(Ok(()));
                    }
                    DbCommand::RememberPix(_item, reply) => {
                        let _ = reply.send(Ok(()));
                    }
                    DbCommand::PendingPix(reply) => {
                        let _ = reply.send(Ok(Vec::<PendingPix>::new()));
                    }
                    DbCommand::ForgetPix(_machine_id, _txid) => {}
                    DbCommand::UnlockOperator => unlocked = true,
                    DbCommand::LockOperator => unlocked = false,
                    DbCommand::LoadSettings => {
                        // O formulário renderiza com valores neutros: os
                        // campos de pacotes não existem no schema original.
                        let _ = events.send(DbEvent::SettingsLoaded(Settings::default()));
                    }
                    DbCommand::SaveSettings(_input) => {
                        if !unlocked {
                            toast(
                                &events,
                                "Abra o menu do operador para alterar configurações",
                                2,
                            );
                            continue;
                        }
                        toast(
                            &events,
                            "Perfil legacy: pacotes não se aplicam ao sistema original \
                             (preço = 1 crédito por música)",
                            2,
                        );
                    }
                    DbCommand::LoadGenres => match db.estilos() {
                        Ok(genres) => {
                            let mut names = vec![String::new()];
                            names.extend(genres.into_iter().filter(|g| !g.blocked).map(|g| g.name));
                            let _ = events.send(DbEvent::GenreOptions(names));
                        }
                        Err(e) => toast(&events, format!("Falha ao listar gêneros: {e}"), 2),
                    },
                    DbCommand::RefreshCatalog => {
                        let recent = db
                            .sistema()
                            .map(|row| row.diaslancamento.max(0) as f64)
                            .unwrap_or(30.0);
                        match db.load_catalog(recent) {
                            Ok(albums) => {
                                let _ = events.send(DbEvent::Catalog(albums));
                            }
                            Err(e) => toast(&events, format!("Falha ao filtrar gêneros: {e}"), 2),
                        }
                    }
                    DbCommand::RequestPlay(track) => {
                        // O débito + a fila `filamidia` ficam com o player
                        // (dono da fila, como no perfil modern).
                        let _ = events.send(DbEvent::Enqueue(track));
                    }
                    DbCommand::SetVolume(volume) => {
                        if let Err(e) = db.set_volume(volume as i64) {
                            log::error!("Falha ao persistir o volume: {e}");
                        }
                    }
                    DbCommand::QueryOperatorStats => {
                        let (partial, absolute) = db.operador_numeros().unwrap_or((0.0, 0.0));
                        let revenue = db.sistema().and_then(|row| {
                            if row.relacaocredito > 0.0 {
                                // Aproximação: créditos históricos ÷ relação
                                // crédito/real (incentivos de cédula inflam o
                                // valor — o relatório fiel vem do Java).
                                Ok((absolute / row.relacaocredito * 100.0) as i64)
                            } else {
                                Err("relação crédito/real não configurada".to_string())
                            }
                        });
                        let price = CUSTO_POR_MUSICA as u32;
                        let recent_days = db
                            .sistema()
                            .map(|row| dias_lancamento_display(row.diaslancamento))
                            .unwrap_or(30);
                        let genres = db.estilos().unwrap_or_default();
                        let _ = events.send(DbEvent::OperatorStats {
                            partial: partial as i64,
                            absolute: absolute as i64,
                            revenue,
                            price,
                            recent_days,
                            genres,
                        });
                    }
                    DbCommand::SetSongPrice(_price) => {
                        toast(
                            &events,
                            "Perfil legacy: preço fixo de 1 crédito por música \
                             (comportamento do sistema original)",
                            2,
                        );
                    }
                    DbCommand::ToggleGenre(genre) => match db.toggle_estilo(&genre) {
                        Ok(blocked) => {
                            log::info!(
                                "Gênero '{genre}' {}",
                                if blocked { "BLOQUEADO" } else { "liberado" }
                            );
                            let recent = db
                                .sistema()
                                .map(|row| row.diaslancamento.max(0) as f64)
                                .unwrap_or(30.0);
                            match db.load_catalog(recent) {
                                Ok(albums) => {
                                    let _ = events.send(DbEvent::Catalog(albums));
                                }
                                Err(e) => {
                                    toast(&events, format!("Falha ao recarregar catálogo: {e}"), 2)
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Falha ao alternar gênero: {e}");
                            toast(&events, "Erro ao bloquear gênero", 2);
                        }
                    },
                    DbCommand::ResetPartial => match db.zeroing() {
                        Ok(()) => {
                            let _ = events.send(DbEvent::PartialReset);
                            toast(&events, "Caixa zerado — novo período aberto", 1);
                        }
                        Err(e) => {
                            log::error!("Falha ao zerar caixa: {e}");
                            toast(&events, "Erro ao zerar caixa", 2);
                        }
                    },
                    DbCommand::ResetCredits => match db.resetar_saldo() {
                        Ok(()) => {
                            let _ = events.send(DbEvent::Balance(0));
                            toast(&events, "Créditos atuais zerados", 1);
                        }
                        Err(e) => {
                            log::error!("Falha ao zerar créditos: {e}");
                            toast(&events, "Erro ao zerar créditos", 2);
                        }
                    },
                    DbCommand::SetRecentDays(days) => match db.set_dias_lancamento(days as i64) {
                        Ok(()) => {
                            toast(&events, format!("Dias de lançamento: {days}"), 1);
                            match db.load_catalog(days as f64) {
                                Ok(albums) => {
                                    let _ = events.send(DbEvent::Catalog(albums));
                                }
                                Err(e) => {
                                    toast(&events, format!("Falha ao recarregar catálogo: {e}"), 2)
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Falha ao salvar dias de lançamento: {e}");
                            toast(&events, "Erro ao salvar dias de lançamento", 2);
                        }
                    },
                }
            }
        })
        .expect("thread do serviço legacy");
    (DbHandle(tx), responses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_price_is_the_original_constant() {
        // O Java debitava 1 crédito por seleção — sem coluna de preço no
        // banco. O operador que tentar mudar o preço recebe um toast.
        assert_eq!(CUSTO_POR_MUSICA, 1.0);
        assert_eq!(CUSTO_POR_MUSICA as u32, 1);
    }
}
