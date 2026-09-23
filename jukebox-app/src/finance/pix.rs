//! MÓDULO 5 — Integração PIX (QR Code Dinâmico + Polling de Status)
//!
//! Fluxo completo, projetado para operar em bar com internet instável
//! (3G/Wi-Fi fraco) sem nunca travar a interface nem causar panic:
//!
//!   1. No boot, a thread faz POST em `{PIX_API_URL}/api/pix/gerar`
//!      enviando o ID da máquina; o backend responde com o payload EMV
//!      dinâmico ("PIX Copia e Cola") + txid + URL de status.
//!   2. O payload EMV é convertido em pixels RGB (crates `qrcode`) e o
//!      buffer cru é enviado à UI, que o materializa em textura via
//!      `slint::Image::load_from_rgb8()` — zero I/O de disco.
//!   3. Um loop de polling faz GET em `/api/pix/status/{txid}` a cada 4s
//!      (sem IP público, sem webhook — a máquina só consulta).
//!   4. Quando o status volta "PAID": emite `PixUiEvent::Paid`, a thread
//!      principal credita via `DbCommand::AddCredit`, exibe o Toast
//!      "PIX Recebido! +N Créditos" e o serviço já solicita um NOVO
//!      QR Code automaticamente para a próxima venda.
//!
//! Resiliência de rede (regra de ouro do bar):
//!   - Timeout total de 3s e timeout de conexão de 2s por requisição;
//!   - Falha de DNS/roteador nunca derruba o serviço: o QR atual continua
//!     na tela e o polling simplesmente tenta de novo no próximo tick;
//!   - Falha ao gerar QR (backend fora do ar): estado "sem conexão" com
//!     retry automático a cada 15s + botão manual de tentar novamente;
//!   - Todo o serviço roda em um runtime Tokio `current_thread` isolado
//!     em uma std::thread própria — o runtime de single-core do Sempron
//!     não paga o preço de um pool multi-thread.
//!
//! Configuração por variáveis de ambiente (definidas no .xinitrc da distro):
//!   - `PIX_API_URL`    — base da API  (padrão: https://meu-backend.com)
//!   - `PIX_MAQUINA_ID` — ID da máquina (padrão: JUKEBOX-001)

use serde::Deserialize;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender as AsyncSender};

// =============================================================================
// Parâmetros comerciais e de rede (ajustáveis para o operador)
// =============================================================================

/// Créditos concedidos por pagamento PIX confirmado.
/// (O painel da UI exibe "R$ 2,00 = 1 CRÉDITO" de forma consistente.)
pub const CREDITOS_POR_PIX: u32 = 1;

/// Valor cobrado no QR Code gerado pelo backend (em reais)
pub const VALOR_PIX: f64 = 2.00;

/// Intervalo do polling de status (especificação: entre 3s e 5s)
const POLL_INTERVAL: Duration = Duration::from_secs(4);

/// Espera entre tentativas de gerar novo QR quando o backend está fora
const QR_RETRY_DELAY: Duration = Duration::from_secs(15);

/// Timeout total de cada requisição HTTP (rede de bar é lenta, mas o usuário
/// não pode esperar mais que isso pela responsividade do sistema)
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

/// Timeout apenas da fase de conexão/DNS
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Endpoint padrão do backend na nuvem
const DEFAULT_API_BASE: &str = "https://meu-backend.com";

/// Identificador padrão da máquina (sobrescível via PIX_MAQUINA_ID)
const DEFAULT_MACHINE_ID: &str = "JUKEBOX-001";

// =============================================================================
// Canais de eventos e comandos
// =============================================================================

/// Eventos enviados do serviço PIX para a thread principal (que os aplica
/// na UI via `slint::invoke_from_event_loop` e credita no banco quando pago).
pub enum PixUiEvent {
    /// Buscando QR Code no backend (estado de carregamento na UI)
    Loading,
    /// QR Code pronto — buffer RGB + dimensões + string Copia e Cola
    QrReady {
        rgb: Vec<u8>,
        width: u32,
        height: u32,
        copia_cola: String,
    },
    /// Backend inacessível (DNS fora, 3G caiu, servidor fora do ar).
    /// O serviço segue tentando sozinho em background.
    Offline { reason: String },
    /// Pagamento confirmado! A UI credita, mostra o Toast e este serviço
    /// já busca um QR novo para a próxima venda.
    Paid { credits: u32 },
}

/// Comandos aceitos pelo serviço (oriundos da interface)
pub enum PixCommand {
    /// Força a geração de um novo QR Code (botão "Tentar novamente")
    RefreshQr,
}

/// Handle do serviço PIX — barato de clonar, seguro para mover para callbacks
#[derive(Clone)]
pub struct PixService {
    cmd_tx: AsyncSender<PixCommand>,
}

impl PixService {
    /// Sobe o serviço PIX: cria a std::thread isolada com o runtime Tokio
    /// `current_thread` e retorna imediatamente o handle de controle.
    pub fn start(config: PixConfig, event_tx: Sender<PixUiEvent>) -> Self {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<PixCommand>(8);

        thread::Builder::new()
            .name("pix-service".to_string())
            .spawn(move || {
                log::info!(
                    "PIX: serviço iniciado — API: {} | Máquina: {}",
                    config.api_base,
                    config.machine_id
                );

                // Runtime de thread única: econômico para o Sempron 145.
                // `enable_all()` habilita o relógio (intervalos) e o driver
                // de I/O exigido pelo reqwest.
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        // Sem runtime não há o que fazer — mas o app segue
                        // vivo: só registramos e encerramos ESTA thread.
                        log::error!("PIX: falha ao criar runtime Tokio: {}", e);
                        let _ = event_tx.send(PixUiEvent::Offline {
                            reason: "falha interna do runtime".to_string(),
                        });
                        return;
                    }
                };

                rt.block_on(pix_main_loop(config, event_tx, cmd_rx));
            })
            .expect("Falha crítica ao criar a thread do serviço PIX");

        Self { cmd_tx }
    }

    /// Pede um novo QR Code imediatamente (chamado pela UI).
    /// `blocking_send` é seguro aqui: a UI não roda em contexto async.
    pub fn request_refresh(&self) {
        let _ = self.cmd_tx.blocking_send(PixCommand::RefreshQr);
    }
}

/// Configuração do serviço PIX, resolvida a partir do ambiente
#[derive(Debug, Clone)]
pub struct PixConfig {
    pub api_base: String,
    pub machine_id: String,
}

impl PixConfig {
    pub fn from_env() -> Self {
        Self {
            api_base: std::env::var("PIX_API_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
                .trim_end_matches('/')
                .to_string(),
            machine_id: std::env::var("PIX_MAQUINA_ID")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MACHINE_ID.to_string()),
        }
    }
}

// =============================================================================
// Loop principal do serviço (assíncrono, dentro do runtime isolado)
// =============================================================================

async fn pix_main_loop(
    config: PixConfig,
    event_tx: Sender<PixUiEvent>,
    mut cmd_rx: Receiver<PixCommand>,
) {
    // Cliente HTTP compartilhado por todas as requisições (pool de conexões
    // embutido). Se a construção falhar (sistema exausto), cai no retry.
    let client = loop {
        match build_client() {
            Ok(client) => break client,
            Err(e) => {
                log::error!("PIX: falha ao criar cliente HTTP: {}", e);
                let _ = event_tx.send(PixUiEvent::Offline {
                    reason: "falha ao iniciar cliente HTTP".to_string(),
                });
                tokio::time::sleep(QR_RETRY_DELAY).await;
            }
        }
    };

    loop {
        // -------------------------------------------------------------------
        // FASE 1 — Obter um QR Code dinâmico do backend (com retry infinito)
        // -------------------------------------------------------------------
        let _ = event_tx.send(PixUiEvent::Loading);

        let qr = loop {
            match fetch_qr_code(&client, &config).await {
                Ok(qr) => break qr,
                Err(reason) => {
                    // Rede caiu / backend fora: sinaliza a UI e espera
                    // (15s) OU um comando manual de refresh, o que vier 1º.
                    log::warn!("PIX: falha ao gerar QR Code: {}", reason);
                    let _ = event_tx.send(PixUiEvent::Offline { reason });

                    tokio::select! {
                        _ = tokio::time::sleep(QR_RETRY_DELAY) => continue,
                        cmd = cmd_rx.recv() => match cmd {
                            Some(PixCommand::RefreshQr) => continue,
                            // Canal fechado = aplicativo encerrando
                            None => return,
                        },
                    }
                }
            }
        };

        // Renderiza o payload EMV em buffer RGB e publica na UI
        match render_qr_to_rgb(&qr.payload) {
            Some((rgb, width, height)) => {
                log::info!(
                    "PIX: QR Code pronto ({}x{}px, txid={}).",
                    width,
                    height,
                    qr.txid
                );
                let _ = event_tx.send(PixUiEvent::QrReady {
                    rgb,
                    width,
                    height,
                    copia_cola: qr.payload.clone(),
                });
            }
            None => {
                // Payload EMV inválido/estourado — pede outro QR ao backend
                log::error!("PIX: payload EMV inválido para gerar QR: {:?}", qr.payload);
                let _ = event_tx.send(PixUiEvent::Offline {
                    reason: "payload PIX inválido".to_string(),
                });
                tokio::time::sleep(QR_RETRY_DELAY).await;
                continue;
            }
        }

        // -------------------------------------------------------------------
        // FASE 2 — Polling do status a cada 4s até PAGO / EXPIRADO
        // -------------------------------------------------------------------
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        // Após uma travada de rede, não dispara rajadas de requisições:
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // consome o tick imediato do interval()

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match check_status(&client, &config, &qr).await {
                        Ok(PixStatus::Paid) => {
                            log::info!("PIX: pagamento CONFIRMADO (txid={}).", qr.txid);
                            let _ = event_tx.send(PixUiEvent::Paid {
                                credits: CREDITOS_POR_PIX,
                            });
                            // Sai para a FASE 1: novo QR para a próxima venda
                            break;
                        }
                        Ok(PixStatus::Expired) => {
                            log::info!("PIX: QR expirado sem pagamento. Renovando...");
                            break;
                        }
                        Ok(PixStatus::Pending) => {
                            // Ainda aguardando pagamento — segue o baile
                        }
                        Err(reason) => {
                            // Rede oscilou (DNS, 3G): NÃO derruba o QR atual.
                            // A UI continua exibindo o QR; próximo tick tenta de novo.
                            log::debug!("PIX: poll de status falhou (rede): {}", reason);
                        }
                    }
                }
                cmd = cmd_rx.recv() => match cmd {
                    Some(PixCommand::RefreshQr) => {
                        log::info!("PIX: refresh manual solicitado pela UI.");
                        break;
                    }
                    None => return,
                }
            }
        }
    }
}

/// Cliente reqwest com rustls (TLS 100% Rust — sem OpenSSL na máquina legada)
fn build_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .user_agent("jukebox-os/1.0")
        .build()
}

// =============================================================================
// Camada HTTP — gerar QR e consultar status
// =============================================================================

/// Dados do QR dinâmivo obtido do backend
struct QrInfo {
    /// Payload EMV completo ("PIX Copia e Cola") — vira imagem QR
    payload: String,
    /// Identificador da transação para o polling
    txid: String,
    /// Caminho/URL de status (relativo à base ou absoluto)
    status_path: String,
}

/// Resposta esperada de POST /api/pix/gerar
#[derive(Debug, Deserialize)]
struct PixCreateResponse {
    /// Payload EMV — aceita variações de nome comuns entre backends
    #[serde(
        rename = "payload",
        alias = "pix_copia_e_cola",
        alias = "brcode",
        alias = "emv",
        alias = "qr_code"
    )]
    payload: String,
    #[serde(default, rename = "txid", alias = "tx_id", alias = "id")]
    txid: Option<String>,
    #[serde(
        default,
        rename = "status_url",
        alias = "url_status",
        alias = "statusUrl",
        alias = "status_path"
    )]
    status_url: Option<String>,
}

/// Resposta esperada de GET /api/pix/status/{txid}
#[derive(Debug, Deserialize)]
struct PixStatusResponse {
    status: String,
}

enum PixStatus {
    Pending,
    Paid,
    Expired,
}

/// POST /api/pix/gerar — solicita um QR Code dinâmico para esta máquina.
async fn fetch_qr_code(client: &reqwest::Client, config: &PixConfig) -> Result<QrInfo, String> {
    let url = format!("{}/api/pix/gerar", config.api_base);

    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "maquina_id": config.machine_id,
            "valor": VALOR_PIX,
        }))
        .send()
        .await
        .map_err(|e| describe_network_error(&e))?;

    let status_code = resp.status();
    if !status_code.is_success() {
        return Err(format!("HTTP {}", status_code.as_u16()));
    }

    let data: PixCreateResponse = resp
        .json()
        .await
        .map_err(|_| "resposta do backend não é JSON válido".to_string())?;

    // Validação básica do payload EMV (um PIX real tem 100+ caracteres)
    let payload = data.payload.trim().to_string();
    if payload.len() < 20 {
        return Err("payload EMV ausente ou truncado".to_string());
    }

    // txid: veio explícito? senão, extrai do próprio EMV (campo 62>05)
    let txid = data
        .txid
        .filter(|t| !t.trim().is_empty())
        .or_else(|| extract_txid_from_emv(&payload))
        .ok_or_else(|| "txid ausente na resposta".to_string())?;

    let status_path = data
        .status_url
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| format!("/api/pix/status/{}", txid));

    Ok(QrInfo {
        payload,
        txid,
        status_path,
    })
}

/// GET /api/pix/status/{txid} — consulta o andamento do pagamento.
async fn check_status(
    client: &reqwest::Client,
    config: &PixConfig,
    qr: &QrInfo,
) -> Result<PixStatus, String> {
    let url = if qr.status_path.starts_with("http") {
        qr.status_path.clone()
    } else {
        format!("{}{}", config.api_base, qr.status_path)
    };

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| describe_network_error(&e))?;

    let status_code = resp.status();
    if !status_code.is_success() {
        return Err(format!("HTTP {}", status_code.as_u16()));
    }

    let data: PixStatusResponse = resp
        .json()
        .await
        .map_err(|_| "resposta de status não é JSON válido".to_string())?;

    // Normaliza: backends usam "PAID", "CONCLUIDA" (padrão Bacen) etc.
    match data.status.trim().to_ascii_uppercase().as_str() {
        "PAID" | "CONCLUIDA" | "CONCLUÍDA" | "COMPLETED" => Ok(PixStatus::Paid),
        "EXPIRED" | "EXPIRADO" | "EXPIRADA" => Ok(PixStatus::Expired),
        _ => Ok(PixStatus::Pending),
    }
}

/// Traduz erros do reqwest para mensagens curtas de operador de bar
fn describe_network_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "tempo esgotado — rede lenta".to_string()
    } else if err.is_connect() {
        "sem conexão com o servidor".to_string()
    } else {
        err.to_string()
    }
}

// =============================================================================
// Geração da imagem do QR Code (payload EMV -> buffer RGB)
// =============================================================================

/// Escala de cada módulo do QR (4x4 px por módulo — legível em CRT/LCD 15")
const QR_SCALE: u32 = 4;

/// Zona de silêncio ao redor do QR (mínimo 4 módulos, conforme a especificação)
const QR_QUIET_ZONE: u32 = 4;

/// Converte o payload EMV em um buffer RGB branco/preto pronto para virar
/// textura do Slint. Retorna `(pixels, largura, altura)` ou None se o EMV
/// for grande demais para um QR Code (erro do backend).
fn render_qr_to_rgb(payload: &str) -> Option<(Vec<u8>, u32, u32)> {
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;

    let modules = code.width() as u32;
    let dim = (modules + QR_QUIET_ZONE * 2) * QR_SCALE;

    // Fundo branco puro: contraste máximo para leitores de celular
    let mut rgb = vec![0xFFu8; (dim * dim * 3) as usize];

    for my in 0..modules {
        for mx in 0..modules {
            if code[(mx as usize, my as usize)] == qrcode::Color::Dark {
                // Pinta o bloco escalado do módulo em azul-marinho profundo
                // (preto ~#12141C: combina com o tema escuro e ainda é
                // perfeitamente legível pelos leitores de QR)
                for sy in 0..QR_SCALE {
                    for sx in 0..QR_SCALE {
                        let px = (mx + QR_QUIET_ZONE) * QR_SCALE + sx;
                        let py = (my + QR_QUIET_ZONE) * QR_SCALE + sy;
                        let offset = ((py * dim + px) * 3) as usize;
                        rgb[offset] = 0x12;
                        rgb[offset + 1] = 0x14;
                        rgb[offset + 2] = 0x1C;
                    }
                }
            }
        }
    }

    Some((rgb, dim, dim))
}

// =============================================================================
// Parser EMV auxiliar — extrai o txid do próprio payload como fallback
// =============================================================================

/// Extrai o valor de uma tag TLV de nível superior do payload EMV-QRCPS.
/// Formato: ID (2 dígitos) + comprimento (2 dígitos) + valor (N chars).
fn emv_tlv_get(emv: &str, tag: &str) -> Option<String> {
    let bytes = emv.as_bytes();
    let mut i = 0usize;

    while i + 4 <= bytes.len() {
        let id = &emv[i..i + 2];
        let len: usize = emv[i + 2..i + 4].parse().ok()?;
        let start = i + 4;
        let end = start.checked_add(len)?;

        if end > emv.len() {
            return None; // TLV truncado/inválido
        }

        if id == tag {
            return Some(emv[start..end].to_string());
        }

        i = end;
    }

    None
}

/// O txid do PIX mora no template 62 (Dados Adicionais) > tag 05.
fn extract_txid_from_emv(emv: &str) -> Option<String> {
    let additional_data = emv_tlv_get(emv, "62")?;
    emv_tlv_get(&additional_data, "05")
}
