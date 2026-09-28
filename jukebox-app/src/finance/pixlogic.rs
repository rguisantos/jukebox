//! PixLogic machine protocol, as implemented by the ESP8266 v2.2 firmware.
use crate::{finance::pix::PixUiEvent, storage::service::DbHandle};
use serde::Deserialize;
use std::{sync::mpsc::Sender, thread, time::Duration};

const INTERVAL: Duration = Duration::from_secs(3);

pub struct Config { base: String, machine: String, token: String }

impl Config {
    /// None selects the legacy QR service. A partial PixLogic configuration is an error.
    pub fn from_env() -> Option<Result<Self,String>> {
        let base = std::env::var("JUKEBOX_PIXLOGIC_API").unwrap_or_default();
        let machine = std::env::var("JUKEBOX_PIXLOGIC_UUID").unwrap_or_default();
        let token = std::env::var("JUKEBOX_PIXLOGIC_TOKEN").unwrap_or_default();
        if base.is_empty() && machine.is_empty() && token.is_empty() { return None; }
        if !base.starts_with("https://") || !valid_uuid(&machine) || token.len() != 64 {
            return Some(Err("Configuração PixLogic incompleta ou inválida".into()));
        }
        Some(Ok(Self { base: base.trim_end_matches('/').into(), machine, token }))
    }
    fn endpoint(&self, action: &str) -> String {
        format!("{}/api/machine/{}/{}", self.base, self.machine, action)
    }
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36 && value.bytes().enumerate().all(|(i,b)| {
        if [8,13,18,23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() }
    })
}

#[derive(Deserialize)]
struct Credit { status: String, #[serde(rename="operacaoId")] operation: Option<String>, retorno: Option<String> }

fn parse_credit(body: &str) -> Result<Option<(String,u32)>,String> {
    let value: Credit = serde_json::from_str(body).map_err(|_| "Resposta PixLogic inválida")?;
    if value.status != "reserved" { return Ok(None); }
    let operation = value.operation.ok_or("PixLogic: operação ausente")?;
    let digits = value.retorno.ok_or("PixLogic: quantidade ausente")?;
    if !valid_uuid(&operation) || digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err("PixLogic: operação ou quantidade inválida".into());
    }
    let amount: u32 = digits.parse().map_err(|_| "PixLogic: quantidade inválida")?;
    if !(1..=100).contains(&amount) { return Err("PixLogic: quantidade fora do limite".into()); }
    Ok(Some((operation,amount)))
}

pub fn spawn(config: Config, storage: DbHandle, events: Sender<PixUiEvent>) {
    thread::Builder::new().name("pixlogic-client".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => { log::error!("PixLogic: falha no runtime: {e}"); return; }
        };
        runtime.block_on(run(config, storage, events));
    }).expect("PixLogic thread");
}

async fn run(config: Config, storage: DbHandle, events: Sender<PixUiEvent>) {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
        Ok(client) => client,
        Err(e) => { log::error!("PixLogic: falha no cliente HTTP: {e}"); return; }
    };
    loop {
        let result = cycle(&client, &config, &storage).await;
        match result {
            Ok(()) => { let _ = events.send(PixUiEvent::PixLogicStatus { connected: true, message: "Aguardando pagamento Pix".into() }); }
            Err(reason) => {
                log::warn!("PixLogic: {reason}");
                let _ = events.send(PixUiEvent::PixLogicStatus { connected: false, message: reason });
            }
        }
        tokio::time::sleep(INTERVAL).await;
    }
}

async fn confirm(client: &reqwest::Client, config: &Config, operation: &str, amount: u32) -> Result<(),String> {
    let response = client.post(config.endpoint("confirm"))
        .header("X-Device-Token", &config.token)
        .json(&serde_json::json!({"operacaoId":operation,"pulsosEmitidos":amount}))
        .send().await.map_err(|e| format!("Confirmação PixLogic: {e}"))?;
    if response.status().as_u16() != 200 { return Err(format!("Confirmação PixLogic HTTP {}", response.status())); }
    Ok(())
}

async fn cycle(client: &reqwest::Client, config: &Config, storage: &DbHandle) -> Result<(),String> {
    for (operation, amount) in storage.pending_pixlogic(config.machine.clone())? {
        confirm(client, config, &operation, amount).await?;
        storage.confirm_pixlogic(config.machine.clone(), operation)?;
    }
    let response = client.get(config.endpoint("credit"))
        .header("X-Device-Token", &config.token).send().await
        .map_err(|e| format!("Consulta PixLogic: {e}"))?;
    if response.status().as_u16() != 200 { return Err(format!("Consulta PixLogic HTTP {}", response.status())); }
    let body = response.text().await.map_err(|e| format!("Resposta PixLogic: {e}"))?;
    if let Some((operation, amount)) = parse_credit(&body)? {
        storage.accept_pixlogic(config.machine.clone(), operation.clone(), amount)?;
        confirm(client, config, &operation, amount).await?;
        storage.confirm_pixlogic(config.machine.clone(), operation)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::Database, storage::service};
    use std::io::{Read, Write};
    const ID: &str = "11111111-2222-3333-4444-555555555555";
    #[test]
    fn accepts_only_firmware_reserved_response() {
        let body = format!(r#"{{"status":"reserved","operacaoId":"{ID}","retorno":"0020"}}"#);
        assert_eq!(parse_credit(&body).unwrap(), Some((ID.into(),20)));
        for bad in ["0000","0101","20","00a1"] {
            let body = format!(r#"{{"status":"reserved","operacaoId":"{ID}","retorno":"{bad}"}}"#);
            assert!(parse_credit(&body).is_err());
        }
        assert!(parse_credit(r#"{"status":"empty"}"#).unwrap().is_none());
    }

    #[test]
    fn applies_reserved_credit_before_confirming_on_a_local_mock_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (storage, _events) = service::spawn(Database::in_memory());
        let server_storage = storage.clone();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let (mut socket, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("mock PixLogic não recebeu a requisição: {e}"),
                    }
                };
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 2048];
                    let count = socket.read(&mut chunk).unwrap();
                    if count == 0 { break; }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") { break; }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.to_ascii_lowercase().contains("x-device-token: test-token"));
                let body = match index {
                    0 => { assert!(request.starts_with("GET /api/machine/"));
                        format!(r#"{{"status":"reserved","operacaoId":"{ID}","retorno":"0020"}}"#) }
                    1 => { assert!(request.starts_with("POST /api/machine/"));
                        assert_eq!(server_storage.pending_pixlogic(ID.into()).unwrap(), vec![(ID.into(),20)]);
                        "{}".into() }
                    _ => { assert!(request.starts_with("GET /api/machine/")); r#"{"status":"empty"}"#.into() }
                };
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
                socket.write_all(response.as_bytes()).unwrap();
            }
        });
        let config = Config { base: format!("http://127.0.0.1:{port}"), machine: ID.into(), token: "test-token".into() };
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let client = reqwest::Client::new();
            cycle(&client,&config,&storage).await.unwrap();
            assert!(storage.pending_pixlogic(ID.into()).unwrap().is_empty());
            cycle(&client,&config,&storage).await.unwrap();
        });
        server.join().unwrap();
    }
}
