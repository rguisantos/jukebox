mod db;
mod media;
mod state;

use db::Database;
use media::scanner;
use slint::{ModelRc, SharedString, VecModel};
use std::sync::mpsc::{self, Sender};
use std::thread;

// Carrega as structs geradas a partir do arquivo ui/app_window.slint
slint::include_modules!();

/// Comandos despachados da UI ou do hardware para a thread do banco de dados
#[derive(Debug)]
enum DbCommand {
    AddCredit(u32),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Inicializa o sistema de logs (pode ser controlado via RUST_LOG=info)
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Iniciando Jukebox Arcade OS...");

    // 1. Inicializa o banco de dados SQLite persistente (conexão principal para créditos)
    let mut db = match Database::open() {
        Ok(database) => database,
        Err(err) => {
            log::error!("Erro crítico ao inicializar o banco de dados: {}", err);
            return Err(Box::new(err));
        }
    };

    // Lê os créditos já gravados no banco
    let initial_credits = db.get_credits().unwrap_or(0);
    log::info!("Créditos persistentes carregados do banco: {}", initial_credits);

    // 2. Instancia a janela principal da UI (Slint)
    let main_window = MainWindow::new()?;
    main_window.set_credits(initial_credits as i32);
    main_window.set_scanning(true);

    // 3. Canal de comunicação assíncrono para operações de I/O em disco
    // Garante que a escrita no SQLite NUNCA congele a thread da interface gráfica
    let (db_tx, db_rx) = mpsc::channel::<DbCommand>();

    // Obtém referências fracas da janela para permitir atualizações de threads externas
    let ui_handle_db = main_window.as_weak();
    let ui_handle_scanner = main_window.as_weak();

    // 4. Thread dedicada para I/O do Banco de Dados (Créditos)
    thread::spawn(move || {
        log::info!("Thread de persistência do SQLite iniciada com sucesso.");
        while let Ok(cmd) = db_rx.recv() {
            match cmd {
                DbCommand::AddCredit(amount) => {
                    log::info!("Processando inserção de crédito (+{})...", amount);
                    match db.increment_credits(amount) {
                        Ok(new_total) => {
                            log::info!("Créditos atualizados no SQLite: {}", new_total);
                            // Envia o novo saldo atômico para a UI de forma segura
                            let ui_clone = ui_handle_db.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = ui_clone.upgrade() {
                                    ui.set_credits(new_total as i32);
                                }
                            });
                        }
                        Err(e) => {
                            log::error!("Falha ao gravar crédito no SQLite: {}", e);
                        }
                    }
                }
            }
        }
    });

    // 5. Thread dedicada para o Scanner de Mídia (abre sua própria conexão SQLite)
    //    O SQLite em modo WAL permite que ambas as threads operem simultaneamente sem lock.
    thread::spawn(move || {
        log::info!("Thread do scanner de mídia iniciada.");

        // Abre conexão independente do SQLite para o scanner (WAL permite concorrência)
        let mut scanner_db = match Database::open() {
            Ok(database) => database,
            Err(err) => {
                log::error!("Scanner: Falha ao abrir conexão com o banco: {}", err);
                return;
            }
        };

        // Executa a varredura incremental do diretório de mídia
        let tracks = scanner::scan_media_directory(&mut scanner_db);

        // Converte as faixas do Rust para o modelo Slint e atualiza a UI
        let slint_tracks: Vec<TrackData> = tracks
            .iter()
            .map(|t| TrackData {
                id: t.id as i32,
                title: SharedString::from(&t.title),
                artist: SharedString::from(&t.artist),
                album: SharedString::from(&t.album),
                file_type: SharedString::from(&t.file_type),
            })
            .collect();

        let track_count = slint_tracks.len();

        // Despacha o modelo para a UI na thread principal do Slint
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_handle_scanner.upgrade() {
                let model = ModelRc::new(VecModel::from(slint_tracks));
                ui.set_tracks(model);
                ui.set_scanning(false);
                log::info!(
                    "Catálogo carregado na interface: {} faixas disponíveis.",
                    track_count
                );
            }
        });
    });

    // 6. Configura o callback para a tecla 'Z' (Moedeiro/Noteiro) emitido pela UI
    let tx_coin: Sender<DbCommand> = db_tx.clone();
    main_window.on_coin_inserted(move || {
        log::debug!("Evento de moeda/tecla 'Z' detectado pela interface.");
        // O envio via canal é instantâneo e não bloqueia a UI
        if let Err(e) = tx_coin.send(DbCommand::AddCredit(1)) {
            log::error!("Erro ao enviar comando de moeda para a fila: {}", e);
        }
    });

    // 7. Inicia o loop de eventos principal da interface gráfica (bloqueia até a janela fechar)
    log::info!("Interface Slint pronta. Entrando no loop principal de eventos X11.");
    main_window.run()?;

    log::info!("Jukebox OS finalizado com sucesso.");
    Ok(())
}
