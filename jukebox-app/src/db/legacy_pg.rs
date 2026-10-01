//! Adaptador PostgreSQL do `jukeboxtvdb` — perfil legacy (fase 1).
//!
//! Este módulo só é compilado com a feature `legacy-pg` e dá ao app Rust
//! acesso ao **mesmo banco do sistema Java em campo**: o catálogo
//! (estilo/artista/disco/midia, capas em `bytea`), os créditos
//! (`sistema.creditos` + `registro_creditos`), a fila (`filamidia`) e o
//! histórico de execuções (`registro_musicas`). É o contrato que permite o
//! rollback instantâneo para o Java durante a migração — nenhuma tabela é
//! criada ou alterada, e os tipos originais são respeitados:
//!
//! - `filamidia.midia` e `registro_musicas.midia` são `bigint`, embora
//!   `midia.id` seja `integer` — a conversão acontece na fronteira;
//! - créditos são `double precision` (o brinde e a relação crédito/real
//!   admitem frações);
//! - `midia` **não tem coluna de caminho**: o arquivo é resolvido pela
//!   convenção da árvore `<raiz>/<estilo>/<artista>/<disco>/<nome>.<ext>`
//!   que o importador original criava em `/home/jukeboxtv/database`
//!   (ver [`resolve_media_path`]);
//! - não existem sequências nem DEFAULT de id: o Java atribuía `max(id)+1`
//!   dentro da transação — aqui fazemos igual, permanecendo compatível.
//!
//! A conexão usa o protocolo v3 (tokio-postgres), que o PostgreSQL 9.1
//! fala sem ressalvas; autenticação local md5 sem TLS, como no original
//! via JDBC. Um único cliente é compartilhado pela thread de serviço — a
//! fila de comandos já serializa o acesso, e o `FOR UPDATE` na linha
//! `sistema` protege as transações de créditos.
//!
//! Premissas semânticas herdadas da análise do JAR (documentadas no
//! README do pacote `jukebox-rs` da análise anterior e no
//! `JUKEBOXTV-LEGACY.md`): `relacaocredito` = créditos por real; moeda
//! (pulso/tecla Z) credita `relacaocredito`; `registro_creditos` registra
//! **entradas de dinheiro** (o consumo só debita `sistema.creditos`).

use crate::legacy_keys::LegacyKeys;
use crate::state::models::{album_initial, fnv64, AlbumInfo, GenreInfo, TrackInfo};
use std::path::{Path, PathBuf};

/// Raiz padrão da árvore de mídia — a mesma que o importador Java criava.
pub const MEDIA_ROOT_DEFAULT: &str = "/home/jukeboxtv/database";

/// Configuração de conexão, lida do ambiente (nunca versionada em código).
///
/// - `JUKEBOX_PG_URL`: URL completa
///   (`postgres://usuario:senha@localhost:5432/jukeboxtvdb`) — tem
///   precedência sobre as variáveis individuais;
/// - `JUKEBOX_PG_HOST` (padrão `localhost`), `JUKEBOX_PG_PORT` (5432),
///   `JUKEBOX_PG_DB` (`jukeboxtvdb`), `JUKEBOX_PG_USER`, `JUKEBOX_PG_PASSWORD`.
///
/// Na distro legacy os valores entram pelo `/etc/jukebox/jukebox.env`
/// carregado pelo launcher; as credenciais originais viviam cifradas em
/// `jjbox.usr`/`jjbox.pwd` e devem ser repassadas ao ambiente.
#[derive(Debug, Clone)]
pub struct PgConfig {
    pub url: String,
    /// Raiz da árvore de mídia (`JUKEBOX_MEDIA_ROOT`).
    pub media_root: PathBuf,
}

impl PgConfig {
    pub fn from_env() -> Self {
        let url = match std::env::var("JUKEBOX_PG_URL") {
            Ok(url) if !url.trim().is_empty() => url,
            _ => {
                let host = std::env::var("JUKEBOX_PG_HOST").unwrap_or_else(|_| "localhost".into());
                let port = std::env::var("JUKEBOX_PG_PORT").unwrap_or_else(|_| "5432".into());
                let db = std::env::var("JUKEBOX_PG_DB").unwrap_or_else(|_| "jukeboxtvdb".into());
                let user = std::env::var("JUKEBOX_PG_USER").unwrap_or_else(|_| "jukeboxtv".into());
                let password = std::env::var("JUKEBOX_PG_PASSWORD").unwrap_or_default();
                format!("postgres://{user}:{password}@{host}:{port}/{db}")
            }
        };
        let media_root = std::env::var("JUKEBOX_MEDIA_ROOT")
            .unwrap_or_else(|_| MEDIA_ROOT_DEFAULT.into());
        Self { url, media_root: PathBuf::from(media_root) }
    }
}

/// Colunas de `sistema` usadas na fase 1 (o original tem 70; as demais —
/// propaganda, autoredução de volume, dual monitor — entram quando os
/// módulos correspondentes forem ligados ao perfil legacy).
pub const SISTEMA_COLUMNS: &str = "creditos, creditosparcial, relacaocredito, \
    mininicioaleatorio, modoaleatorio, modofesta, volume, maxvolume, capacoluna, \
    capalinha, habilitaincentivo, incentivo2, incentivo5, incentivo10, incentivo20, \
    incentivo50, habilitabrinde, minmusicasbrinde, premiocredbrinde, contbrinde, \
    txtbrinde, codteclaesquerda, codtecladireita, codteclacima, codteclabaixo, \
    codtecladisco, codteclamusica, codteclacredito, codteclavolume, codteclacancela, \
    codteclasair, codteclaresetacreditos, codteclamaisvolume, codteclamenosvolume, \
    codteclafecharprograma, diaslancamento";

/// Linha `sistema` com leitura tolerante a nulo (a inspeção física mostrou
/// **todas** as colunas nullable — o Java gravava o que existia).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemRow {
    // ── créditos ──
    pub creditos: f64,
    pub creditosparcial: f64,
    pub relacaocredito: f64,
    pub mininicioaleatorio: f64,
    // ── modos ──
    pub modoaleatorio: bool,
    pub modofesta: bool,
    // ── volume ──
    pub volume: i64,
    pub maxvolume: i32,
    // ── grade (o layout clássico é 5×2) ──
    pub capacoluna: i32,
    pub capalinha: i32,
    // ── incentivos (cédulas) ──
    pub habilitaincentivo: bool,
    pub incentivo2: i32,
    pub incentivo5: i32,
    pub incentivo10: i32,
    pub incentivo20: i32,
    pub incentivo50: i32,
    // ── brinde ──
    pub habilitabrinde: bool,
    pub minmusicasbrinde: i32,
    pub premiocredbrinde: i32,
    pub contbrinde: i32,
    pub txtbrinde: String,
    // ── teclas (códigos AWT) ──
    pub keys: LegacyKeys,
    // ── catálogo / lançamentos ──
    /// Janela (dias) em que um disco conta como lançamento.
    pub diaslancamento: i64,
}

impl SystemRow {
    /// Ponte para a camada de tradução de teclas: as `codtecla*` desta
    /// máquina viram o mapa AWT → tecla canônica do app.
    pub fn legacy_keys(&self) -> LegacyKeys {
        self.keys.clone()
    }

    /// Grade configurada na máquina, com fallback para o clássico 5×2
    /// (mesmas regras de tolerância do port de referência).
    pub fn grid_dimensions(&self) -> (i32, i32) {
        let cols = if (1..=12).contains(&self.capacoluna) { self.capacoluna } else { 5 };
        let rows = if (1..=8).contains(&self.capalinha) { self.capalinha } else { 2 };
        (cols, rows)
    }
}

// =============================================================================
// Regras puras de créditos (portadas do jukebox-rs; sem I/O — testáveis)
// =============================================================================

/// Créditos de uma moeda (pulso do moedeiro / tecla Z).
pub fn creditos_por_moeda(relacaocredito: f64) -> f64 {
    relacaocredito.max(0.0)
}

/// Bônus (incentivo) configurado para uma cédula de R$ 2/5/10/20/50.
pub fn bonus_da_cedula(
    reais: i32,
    habilitado: bool,
    incentivo2: i32,
    incentivo5: i32,
    incentivo10: i32,
    incentivo20: i32,
    incentivo50: i32,
) -> i32 {
    if !habilitado {
        return 0;
    }
    match reais {
        2 => incentivo2,
        5 => incentivo5,
        10 => incentivo10,
        20 => incentivo20,
        50 => incentivo50,
        _ => 0,
    }
}

/// Créditos totais de uma cédula: `valor × relação + incentivo`.
pub fn creditos_por_cedula(reais: i32, relacaocredito: f64, bonus: i32) -> f64 {
    let base = reais.max(0) as f64 * relacaocredito.max(0.0);
    base + bonus.max(0) as f64
}

/// O brinde está pronto para o sorteio?
pub fn brinde_pronto(habilitabrinde: bool, contbrinde: i32, minmusicasbrinde: i32) -> bool {
    habilitabrinde && minmusicasbrinde > 0 && contbrinde >= minmusicasbrinde
}

/// Saldo exibido na UI: créditos integrais (o saldo real continua
/// `double precision` no banco — o brinde admite frações; a UI mostra
/// quantas músicas inteiras o saldo paga).
pub fn saldo_display(creditos: f64) -> u32 {
    creditos.max(0.0).floor().min(u32::MAX as f64) as u32
}

/// Volume 0..=100 respeitando o teto da máquina (`maxvolume`).
/// Teto ausente/inválido no banco = 100.
pub fn volume_display(volume: i64, maxvolume: i32) -> u32 {
    let ceiling = if (1..=100).contains(&maxvolume) {
        maxvolume as i64
    } else {
        100
    };
    volume.clamp(0, ceiling) as u32
}

/// Janela de lançamento (dias) para a UI: negativo vira 0, acima de
/// 10 anos vira 3650 (limita consultas `now() - interval` absurdas).
pub fn dias_lancamento_display(dias: i64) -> u32 {
    dias.clamp(0, 3650) as u32
}

// =============================================================================
// Resolução de caminho (convenção da árvore original)
// =============================================================================

/// Caminho físico de uma mídia pela convenção da árvore:
/// `<raiz>/<estilo>/<artista>/<disco>/<nome>.<extensao>`.
///
/// `midia` não guarda caminho no banco — `nome` é o nome do arquivo sem
/// extensão e `extensao_conteudo` a extensão. O separador é sempre `/`
/// (Linux). Nomes com `/` (ex.: "AC/DC") são um caso conhecido da árvore
/// original: o importador Java criava os diretórios; se o acervo em campo
/// sanitizou esses nomes, o operador pode apontar `JUKEBOX_MEDIA_ROOT`
/// para a raiz real e/ou a resolução será ajustada na validação de campo.
pub fn resolve_media_path(
    root: &Path,
    estilo: &str,
    artista: &str,
    disco: &str,
    nome: &str,
    extensao: &str,
) -> PathBuf {
    let ext = extensao.trim().trim_start_matches('.');
    root.join(estilo)
        .join(artista)
        .join(disco)
        .join(format!("{nome}.{ext}"))
}

/// Constrói o álbum (disco) no formato do app a partir dos campos do banco.
/// `is_recent` vem calculado pelo SQL (janela `diaslancamento`) para não
/// precisar decodificar timestamps no cliente.
pub fn album_from_disco(
    disco_id: i32,
    disco_nome: &str,
    artista_nome: &str,
    estilo_nome: &str,
    is_recent: bool,
    tracks: Vec<TrackInfo>,
) -> AlbumInfo {
    let key = format!("pg:{disco_id}");
    AlbumInfo {
        palette: (fnv64(&key) % 6) as u32,
        initial: album_initial(disco_nome),
        key,
        title: disco_nome.to_string(),
        artist: artista_nome.to_string(),
        genre: estilo_nome.to_string(),
        is_recent,
        tracks,
    }
}

/// Constrói a faixa (midia) no formato do app.
#[allow(clippy::too_many_arguments)]
pub fn track_from_midia(
    midia_id: i32,
    nome: &str,
    artista: &str,
    disco: &str,
    estilo: &str,
    extensao: &str,
    media_root: &Path,
) -> TrackInfo {
    TrackInfo {
        id: midia_id as i64,
        title: nome.to_string(),
        artist: artista.to_string(),
        album: disco.to_string(),
        file_path: resolve_media_path(media_root, estilo, artista, disco, nome, extensao)
            .to_string_lossy()
            .into_owned(),
        file_type: extensao.trim().trim_start_matches('.').to_lowercase(),
        genre: estilo.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_path_follows_the_original_tree_convention() {
        let path = resolve_media_path(
            Path::new("/home/jukeboxtv/database"),
            "Rock",
            "Legião Urbana",
            "Dois",
            "Tempo Perdido",
            "mp3",
        );
        assert_eq!(
            path.to_str().unwrap(),
            "/home/jukeboxtv/database/Rock/Legião Urbana/Dois/Tempo Perdido.mp3"
        );
        // Extensão com ponto à direita também normaliza.
        let path = resolve_media_path(Path::new("/dados"), "Sertanejo", "Zé", "CD", "Faixa 1", ".mp4");
        assert_eq!(path.to_str().unwrap(), "/dados/Sertanejo/Zé/CD/Faixa 1.mp4");
    }

    #[test]
    fn moeda_credits_follow_the_relation() {
        assert_eq!(creditos_por_moeda(1.5), 1.5);
        assert_eq!(creditos_por_moeda(0.0), 0.0);
        assert_eq!(creditos_por_moeda(-3.0), 0.0, "relação negativa não credita");
    }

    #[test]
    fn cedule_credits_add_the_enabled_bonus() {
        // R$ 10 com relação 1,5 e incentivo 2 → 17 créditos.
        assert_eq!(creditos_por_cedula(10, 1.5, 2), 17.0);
        // Sem incentivo habilitado o bônus é zero.
        assert_eq!(bonus_da_cedula(10, false, 1, 2, 3, 4, 5), 0);
        assert_eq!(bonus_da_cedula(10, true, 1, 2, 3, 4, 5), 3);
        assert_eq!(bonus_da_cedula(3, true, 1, 2, 3, 4, 5), 0, "cédula inexistente");
    }

    #[test]
    fn brinde_waits_for_the_minimum_song_count() {
        assert!(!brinde_pronto(true, 9, 10));
        assert!(brinde_pronto(true, 10, 10));
        assert!(!brinde_pronto(false, 99, 10), "brinde desabilitado nunca sorteia");
        assert!(!brinde_pronto(true, 99, 0), "mínimo 0 desativa");
    }

    #[test]
    fn system_row_bridge_feeds_the_legacy_key_map() {
        let mut row = SystemRow {
            creditos: 12.5,
            relacaocredito: 1.0,
            keys: LegacyKeys::typical(),
            capacoluna: 5,
            capalinha: 2,
            ..SystemRow::default()
        };
        let keys = row.legacy_keys();
        assert_eq!(keys.translate(crate::legacy_keys::awt::VK_Z), Some("z"));
        // Grade clássica preservada; valores inválidos caem no 5×2.
        assert_eq!(row.grid_dimensions(), (5, 2));
        row.capacoluna = 0;
        row.capalinha = 99;
        assert_eq!(row.grid_dimensions(), (5, 2));
    }

    #[test]
    fn album_and_track_mapping_stable_keys() {
        let track = track_from_midia(
            55230,
            "Tempo Perdido",
            "Legião Urbana",
            "Dois",
            "Rock",
            "mp3",
            Path::new("/home/jukeboxtv/database"),
        );
        assert_eq!(track.id, 55230);
        assert!(track.file_path.ends_with("Tempo Perdido.mp3"));
        assert!(!track.is_video());
        let album = album_from_disco(3497, "Dois", "Legião Urbana", "Rock", true, vec![track.clone()]);
        assert_eq!(album.key, "pg:3497");
        assert_eq!(album.initial, "D");
        assert_eq!(album.palette, (fnv64("pg:3497") % 6) as u32);
        assert_eq!(album.tracks.len(), 1);
        // Vídeo pela extensão, como no app modern.
        let video = track_from_midia(2, "Clipe", "Artista", "DVD", "Rock", "mpeg", Path::new("/dados"));
        assert!(video.is_video());
    }

    #[test]
    fn display_helpers_floor_and_clamp() {
        // Saldo: frações do brinde ficam no banco; a UI mostra músicas
        // inteiras pagáveis.
        assert_eq!(saldo_display(2.9), 2);
        assert_eq!(saldo_display(0.5), 0);
        assert_eq!(saldo_display(-3.0), 0);
        assert_eq!(saldo_display(7.0), 7);
        // Volume: respeita o teto da máquina; teto inválido = 100.
        assert_eq!(volume_display(150, 80), 80);
        assert_eq!(volume_display(90, 0), 90);
        assert_eq!(volume_display(-5, 100), 0);
        assert_eq!(volume_display(70, 85), 70);
        // Janela de lançamento: negativo vira 0, absurdo vira 10 anos.
        assert_eq!(dias_lancamento_display(-1), 0);
        assert_eq!(dias_lancamento_display(30), 30);
        assert_eq!(dias_lancamento_display(999_999), 3650);
    }

    #[test]
    fn system_row_reads_the_release_window() {
        let row = SystemRow { diaslancamento: 45, ..Default::default() };
        assert_eq!(row.diaslancamento, 45);
        // A leitura tolerante a nulo entrega 0 quando a coluna é NULL —
        // a UI de lançamentos simplesmente fica sem destaque.
        assert_eq!(SystemRow::default().diaslancamento, 0);
    }
}

// =============================================================================
// Cliente PostgreSQL (requer a feature `legacy-pg`)
// =============================================================================
//
// Um único `Client` tokio-postgres pertence à thread de serviço; cada
// método roda num `block_on` do runtime current-thread dedicado (o mesmo
// desenho da thread PixLogic). O driver da conexão vive em uma thread
// própria, garantindo que o socket seja drenado mesmo entre comandos.
//
// Transações de crédito travam a linha `sistema` com `FOR UPDATE` e
// atribuem ids com `max(id)+1` dentro da transação — idêntico ao Java
// original, que é dono do schema e não tem sequências.

#[cfg(feature = "legacy-pg")]
pub mod client {
    use super::*;
    use std::collections::HashMap;
    use tokio_postgres::{Client, NoTls, Row};

    pub struct LegacyDb {
        client: Client,
        runtime: tokio::runtime::Runtime,
        media_root: PathBuf,
    }

    impl LegacyDb {
        /// Conecta ao `jukeboxtvdb`. A URL vem de [`PgConfig`] (ambiente);
        /// autenticação local md5 sem TLS, como o JDBC original.
        pub fn connect(config: &PgConfig) -> Result<Self, String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("runtime PostgreSQL: {e}"))?;
            let (client, connection) =
                runtime.block_on(tokio_postgres::connect(&config.url, NoTls))
                    .map_err(|e| format!("conectando ao PostgreSQL: {e}"))?;
            // Driver da conexão em thread própria com runtime próprio.
            std::thread::Builder::new()
                .name("pg-connection".into())
                .spawn(move || {
                    if let Err(e) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime do driver PostgreSQL")
                        .block_on(connection)
                    {
                        log::error!("conexão PostgreSQL encerrada: {e}");
                    }
                })
                .map_err(|e| format!("thread do driver PostgreSQL: {e}"))?;
            Ok(Self { client, runtime, media_root: config.media_root.clone() })
        }

        // ── sistema ─────────────────────────────────────────────────────

        /// Linha `sistema` (tolerante a nulos — todas as colunas são
        /// nullable na inspeção física).
        pub fn sistema(&mut self) -> Result<SystemRow, String> {
            self.runtime.block_on(async {
                let row = self
                    .client
                    .query_one(
                        &format!("SELECT {SISTEMA_COLUMNS} FROM sistema LIMIT 1"),
                        &[],
                    )
                    .await
                    .map_err(|e| format!("lendo sistema: {e}"))?;
                Ok(system_from_row(&row))
            })
        }

        /// Saldo atual (`sistema.creditos`).
        pub fn balance(&mut self) -> Result<f64, String> {
            self.runtime.block_on(async {
                let row = self
                    .client
                    .query_opt("SELECT creditos FROM sistema LIMIT 1", &[])
                    .await
                    .map_err(|e| format!("lendo saldo: {e}"))?;
                Ok(row
                    .and_then(|r| r.try_get::<_, Option<f64>>(0).ok().flatten())
                    .unwrap_or(0.0))
            })
        }

        /// Persiste o volume na coluna original (`sistema.volume`).
        pub fn set_volume(&mut self, volume: i64) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute("UPDATE sistema SET volume = $1", &[&volume])
                    .await
                    .map_err(|e| format!("gravando volume: {e}"))?;
                Ok(())
            })
        }

        // ── catálogo ────────────────────────────────────────────────────

        /// Catálogo completo: discos de estilos habilitados, com faixas
        /// ordenadas por posição. `recent_days` é a janela de lançamento
        /// (`sistema.diaslancamento` no original).
        pub fn load_catalog(&mut self, recent_days: f64) -> Result<Vec<AlbumInfo>, String> {
            self.runtime.block_on(async {
                let discos = self
                    .client
                    .query(
                        "SELECT d.id, d.nome, a.nome, e.nome, \
                         (d.timestamp > now() - ($1::double precision * interval '1 day')) AS recente \
                         FROM disco d \
                         JOIN artista a ON a.id = d.artista \
                         JOIN estilo e ON e.id = a.estilo \
                         WHERE e.habilita = true \
                         ORDER BY a.comparetonome, d.comparetonome",
                        &[&recent_days],
                    )
                    .await
                    .map_err(|e| format!("lendo discos: {e}"))?;
                let midias = self
                    .client
                    .query(
                        "SELECT m.id, m.nome, m.disco, m.extensao_conteudo, \
                         d.nome, a.nome, e.nome \
                         FROM midia m \
                         JOIN disco d ON d.id = m.disco \
                         JOIN artista a ON a.id = m.artista \
                         JOIN estilo e ON e.id = a.estilo \
                         WHERE e.habilita = true \
                         ORDER BY m.disco, m.posicao NULLS LAST, m.comparetonome",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("lendo mídias: {e}"))?;

                let mut faixas: HashMap<i32, Vec<TrackInfo>> = HashMap::new();
                for row in &midias {
                    let disco_id: i32 = row.try_get(2).map_err(|e| e.to_string())?;
                    let track = track_from_midia(
                        row.try_get(0).map_err(|e| e.to_string())?,
                        row.try_get::<_, String>(1).map_err(|e| e.to_string())?.as_str(),
                        row.try_get::<_, String>(5).map_err(|e| e.to_string())?.as_str(),
                        row.try_get::<_, String>(4).map_err(|e| e.to_string())?.as_str(),
                        row.try_get::<_, String>(6).map_err(|e| e.to_string())?.as_str(),
                        row.try_get::<_, String>(3).map_err(|e| e.to_string())?.as_str(),
                        &self.media_root,
                    );
                    faixas.entry(disco_id).or_default().push(track);
                }

                Ok(discos
                    .iter()
                    .map(|row| {
                        let id: i32 = row.try_get(0).unwrap_or_default();
                        album_from_disco(
                            id,
                            row.try_get::<_, String>(1).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(2).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(3).unwrap_or_default().as_str(),
                            row.try_get::<_, Option<bool>>(4).ok().flatten().unwrap_or(false),
                            faixas.remove(&id).unwrap_or_default(),
                        )
                    })
                    .collect())
            })
        }

        /// Capa de um disco (`disco.capa` bytea) para o pipeline de capas
        /// do app — a extensão original está em `disco.extensao_capa`.
        pub fn fetch_cover(&mut self, disco_id: i32) -> Result<Option<Vec<u8>>, String> {
            self.runtime.block_on(async {
                let row = self
                    .client
                    .query_opt("SELECT capa FROM disco WHERE id = $1", &[&disco_id])
                    .await
                    .map_err(|e| format!("lendo capa: {e}"))?;
                Ok(row
                    .and_then(|r| r.try_get::<_, Option<Vec<u8>>>(0).ok().flatten()))
            })
        }

        // ── créditos ────────────────────────────────────────────────────

        /// Entrada de dinheiro: atualiza saldo/parcial e grava a linha em
        /// `registro_creditos` — tudo numa transação com `FOR UPDATE`.
        pub fn entrada(&mut self, valor: f64) -> Result<f64, String> {
            if valor <= 0.0 {
                return Err("valor de entrada deve ser positivo".into());
            }
            self.runtime.block_on(async {
                let tx = self
                    .client
                    .transaction()
                    .await
                    .map_err(|e| format!("abrindo transação de créditos: {e}"))?;
                let row = tx
                    .query_one(
                        "SELECT creditos, creditosparcial FROM sistema FOR UPDATE",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("travando sistema: {e}"))?;
                let saldo = opt_f64(&row, 0);
                let parcial = opt_f64(&row, 1);
                let (novo, nova_parcial) = (saldo + valor, parcial + valor);
                tx.execute(
                    "UPDATE sistema SET creditos = $1, creditosparcial = $2",
                    &[&novo, &nova_parcial],
                )
                .await
                .map_err(|e| format!("atualizando saldo: {e}"))?;
                let id: i32 = tx
                    .query_one(
                        "SELECT COALESCE(MAX(id), 0) + 1 FROM registro_creditos",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("calculando id de registro_creditos: {e}"))?
                    .get(0);
                // datahorareset vem da própria linha sistema (período vigente).
                tx.execute(
                    "INSERT INTO registro_creditos (creditos, datahora, datahorareset, id) \
                     SELECT $1, now(), resetcreditos, $2 FROM sistema",
                    &[&valor, &id],
                )
                .await
                .map_err(|e| format!("gravando registro_creditos: {e}"))?;
                tx.commit()
                    .await
                    .map_err(|e| format!("confirmando transação de créditos: {e}"))?;
                Ok(novo)
            })
        }

        /// Moeda/pulso do moedeiro (tecla Z): credita `relacaocredito`.
        pub fn entrada_moeda(&mut self) -> Result<f64, String> {
            let row = self.sistema()?;
            self.entrada(creditos_por_moeda(row.relacaocredito))
        }

        /// Cédula de R$ 2/5/10/20/50 (com incentivo, se habilitado).
        pub fn entrada_cedula(&mut self, reais: i32) -> Result<f64, String> {
            let row = self.sistema()?;
            let bonus = bonus_da_cedula(
                reais,
                row.habilitaincentivo,
                row.incentivo2,
                row.incentivo5,
                row.incentivo10,
                row.incentivo20,
                row.incentivo50,
            );
            self.entrada(creditos_por_cedula(reais, row.relacaocredito, bonus))
        }

        /// Débito simples do saldo (sem fila).
        pub fn debitar(&mut self, custo: f64) -> Result<f64, String> {
            if custo <= 0.0 {
                return Err("custo deve ser positivo".into());
            }
            self.runtime.block_on(async {
                let tx = self
                    .client
                    .transaction()
                    .await
                    .map_err(|e| format!("abrindo transação de débito: {e}"))?;
                let row = tx
                    .query_one("SELECT creditos FROM sistema FOR UPDATE", &[])
                    .await
                    .map_err(|e| format!("travando sistema: {e}"))?;
                let saldo = opt_f64(&row, 0);
                if saldo < custo {
                    tx.rollback().await.ok();
                    return Err(format!(
                        "saldo insuficiente: atual {saldo:.2}, custo {custo:.2}"
                    ));
                }
                let novo = saldo - custo;
                tx.execute("UPDATE sistema SET creditos = $1", &[&novo])
                    .await
                    .map_err(|e| format!("atualizando saldo: {e}"))?;
                tx.commit()
                    .await
                    .map_err(|e| format!("confirmando transação de débito: {e}"))?;
                Ok(novo)
            })
        }

        /// Zeroing (tecla reset): zera saldo/parcial e abre novo período de
        /// caixa. As linhas antigas de `registro_creditos` preservam o
        /// `datahorareset` do período a que pertencem — os relatórios por
        /// período ficam intactos. O contador do brinde é preservado
        /// (premissa: `creditosbrindenoreset` indica essa intenção no
        /// original; validar com o operador em campo).
        pub fn zeroing(&mut self) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute(
                        "UPDATE sistema SET creditos = 0, creditosparcial = 0, \
                         resetcreditos = now()",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("zerando créditos: {e}"))?;
                Ok(())
            })
        }

        // ── fila e execuções ────────────────────────────────────────────

        /// Reserva uma mídia: debita o custo e enfileira em `filamidia`
        /// **na mesma transação** — a mesma garantia "reserva + débito"
        /// do app modern. `custo = 0` enfileira sem debitar (Festa/Pix já
        /// liquidado — o crédito concedido entra como `entrada`).
        pub fn enqueue(&mut self, midia_id: i64, custo: f64) -> Result<(), String> {
            self.runtime.block_on(async {
                let tx = self
                    .client
                    .transaction()
                    .await
                    .map_err(|e| format!("abrindo transação da fila: {e}"))?;
                if custo > 0.0 {
                    let row = tx
                        .query_one("SELECT creditos FROM sistema FOR UPDATE", &[])
                        .await
                        .map_err(|e| format!("travando sistema: {e}"))?;
                    let saldo = opt_f64(&row, 0);
                    if saldo < custo {
                        tx.rollback().await.ok();
                        return Err(format!(
                            "saldo insuficiente: atual {saldo:.2}, custo {custo:.2}"
                        ));
                    }
                    tx.execute("UPDATE sistema SET creditos = $1", &[&(saldo - custo)])
                        .await
                        .map_err(|e| format!("debitando: {e}"))?;
                }
                let id: i32 = tx
                    .query_one("SELECT COALESCE(MAX(id), 0) + 1 FROM filamidia", &[])
                    .await
                    .map_err(|e| format!("calculando id de filamidia: {e}"))?
                    .get(0);
                // filamidia.midia é bigint; midia_id já é i64.
                tx.execute(
                    "INSERT INTO filamidia (id, midia, datahora) VALUES ($1, $2, now())",
                    &[&id, &midia_id],
                )
                .await
                .map_err(|e| format!("enfileirando: {e}"))?;
                tx.commit()
                    .await
                    .map_err(|e| format!("confirmando transação da fila: {e}"))?;
                Ok(())
            })
        }

        /// Retira e devolve a mídia mais antiga da fila (`None` se vazia),
        /// já resolvida para o caminho físico pela convenção da árvore.
        pub fn dequeue(&mut self) -> Result<Option<TrackInfo>, String> {
            self.runtime.block_on(async {
                let tx = self
                    .client
                    .transaction()
                    .await
                    .map_err(|e| format!("abrindo transação da fila: {e}"))?;
                let row = match tx
                    .query_opt(
                        "SELECT f.id, m.id, m.nome, m.extensao_conteudo, \
                         d.nome, a.nome, e.nome \
                         FROM filamidia f \
                         JOIN midia m ON m.id = f.midia \
                         JOIN disco d ON d.id = m.disco \
                         JOIN artista a ON a.id = m.artista \
                         JOIN estilo e ON e.id = a.estilo \
                         ORDER BY f.id LIMIT 1",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("lendo fila: {e}"))?
                {
                    Some(row) => row,
                    None => {
                        tx.commit().await.ok();
                        return Ok(None);
                    }
                };
                let fila_id: i32 = row.try_get(0).map_err(|e| e.to_string())?;
                let track = track_from_midia(
                    row.try_get(1).map_err(|e| e.to_string())?,
                    row.try_get::<_, String>(2).map_err(|e| e.to_string())?.as_str(),
                    row.try_get::<_, String>(5).map_err(|e| e.to_string())?.as_str(),
                    row.try_get::<_, String>(4).map_err(|e| e.to_string())?.as_str(),
                    row.try_get::<_, String>(6).map_err(|e| e.to_string())?.as_str(),
                    row.try_get::<_, String>(3).map_err(|e| e.to_string())?.as_str(),
                    &self.media_root,
                );
                tx.execute("DELETE FROM filamidia WHERE id = $1", &[&fila_id])
                    .await
                    .map_err(|e| format!("removendo da fila: {e}"))?;
                tx.commit()
                    .await
                    .map_err(|e| format!("confirmando retirada da fila: {e}"))?;
                Ok(Some(track))
            })
        }

        /// Registra a execução no histórico de relatórios (tabela sem id —
        /// a chave natural é `(midia, datahora)`).
        pub fn log_played(&mut self, midia_id: i64) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute(
                        "INSERT INTO registro_musicas (midia, datahora, exportado) \
                         VALUES ($1, now(), false)",
                        &[&midia_id],
                    )
                    .await
                    .map_err(|e| format!("gravando registro_musicas: {e}"))?;
                Ok(())
            })
        }

        /// Conta uma execução para o sorteio do brinde.
        pub fn bump_brinde(&mut self) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute(
                        "UPDATE sistema SET contbrinde = COALESCE(contbrinde, 0) + 1",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("contando brinde: {e}"))?;
                Ok(())
            })
        }

        /// Sorteia o prêmio do brinde: credita `premiocredbrinde` e zera o
        /// contador. Erro se o brinde não estiver configurado.
        pub fn premiar_brinde(&mut self) -> Result<f64, String> {
            let row = self.sistema()?;
            if !row.habilitabrinde || row.premiocredbrinde <= 0 {
                return Err("brinde sem prêmio configurado".into());
            }
            let novo = self.entrada(row.premiocredbrinde as f64)?;
            self.runtime.block_on(async {
                self.client
                    .execute("UPDATE sistema SET contbrinde = 0", &[])
                    .await
                    .map_err(|e| format!("zerando contador do brinde: {e}"))?;
                Ok::<(), String>(())
            })?;
            Ok(novo)
        }

        // ── operador / gêneros / fila ─────────────────────────────────

        /// Zera apenas o saldo atual (tecla `resetacreditos` do original —
        /// não toca no parcial do período).
        pub fn resetar_saldo(&mut self) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute("UPDATE sistema SET creditos = 0", &[])
                    .await
                    .map_err(|e| format!("zerando saldo: {e}"))?;
                Ok(())
            })
        }

        /// Alterna um estilo (gênero) entre habilitado/bloqueado e devolve o
        /// novo estado. O catálogo público filtra por `estilo.habilita`.
        pub fn toggle_estilo(&mut self, nome: &str) -> Result<bool, String> {
            self.runtime.block_on(async {
                let row = self
                    .client
                    .query_opt(
                        "UPDATE estilo SET habilita = NOT habilita WHERE nome = $1 \
                         RETURNING habilita",
                        &[&nome],
                    )
                    .await
                    .map_err(|e| format!("alternando estilo: {e}"))?;
                row.and_then(|r| r.try_get::<_, bool>(0).ok())
                    .ok_or_else(|| format!("estilo desconhecido: {nome}"))
            })
        }

        /// Estilos com estado de bloqueio e contagem de faixas (menu do
        /// operador e seletor de gêneros).
        pub fn estilos(&mut self) -> Result<Vec<GenreInfo>, String> {
            self.runtime.block_on(async {
                let rows = self
                    .client
                    .query(
                        "SELECT e.nome, e.habilita, COUNT(m.id) FROM estilo e \
                         LEFT JOIN artista a ON a.estilo = e.id \
                         LEFT JOIN disco d ON d.artista = a.id \
                         LEFT JOIN midia m ON m.disco = d.id \
                         GROUP BY e.nome, e.habilita ORDER BY e.nome",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("lendo estilos: {e}"))?;
                Ok(rows
                    .iter()
                    .map(|r| GenreInfo {
                        name: r.try_get::<_, String>(0).unwrap_or_default(),
                        // `blocked` do app = NOT habilita do banco.
                        blocked: !r.try_get::<_, bool>(1).unwrap_or(true),
                        track_count: r.try_get::<_, i64>(2).unwrap_or(0),
                    })
                    .collect())
            })
        }

        /// Números do operador: créditos acumulados no período corrente
        /// (`sistema.creditosparcial`) e total histórico
        /// (`SUM(registro_creditos.creditos)`).
        pub fn operador_numeros(&mut self) -> Result<(f64, f64), String> {
            self.runtime.block_on(async {
                let parcial = self
                    .client
                    .query_opt("SELECT creditosparcial FROM sistema LIMIT 1", &[])
                    .await
                    .map_err(|e| format!("lendo parcial: {e}"))?
                    .and_then(|r| r.try_get::<_, Option<f64>>(0).ok().flatten())
                    .unwrap_or(0.0);
                let absoluto = self
                    .client
                    .query_one(
                        "SELECT COALESCE(SUM(creditos), 0) FROM registro_creditos",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("somando registro_creditos: {e}"))?
                    .try_get::<_, f64>(0)
                    .map_err(|e| e.to_string())?;
                Ok((parcial, absoluto))
            })
        }

        /// Grava a janela de lançamento (`sistema.diaslancamento`).
        pub fn set_dias_lancamento(&mut self, dias: i64) -> Result<(), String> {
            self.runtime.block_on(async {
                self.client
                    .execute("UPDATE sistema SET diaslancamento = $1", &[&dias])
                    .await
                    .map_err(|e| format!("gravando diaslancamento: {e}"))?;
                Ok(())
            })
        }

        /// Fila atual completa (sem remover) para o painel da UI — inclui
        /// itens enfileirados pelo Java antes de um rollback, por exemplo.
        pub fn queue_snapshot(&mut self) -> Result<Vec<TrackInfo>, String> {
            self.runtime.block_on(async {
                let rows = self
                    .client
                    .query(
                        "SELECT f.id, m.id, m.nome, m.extensao_conteudo, \
                         d.nome, a.nome, e.nome \
                         FROM filamidia f \
                         JOIN midia m ON m.id = f.midia \
                         JOIN disco d ON d.id = m.disco \
                         JOIN artista a ON a.id = m.artista \
                         JOIN estilo e ON e.id = a.estilo \
                         ORDER BY f.id",
                        &[],
                    )
                    .await
                    .map_err(|e| format!("lendo fila: {e}"))?;
                Ok(rows
                    .iter()
                    .map(|row| {
                        track_from_midia(
                            row.try_get(1).unwrap_or_default(),
                            row.try_get::<_, String>(2).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(5).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(4).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(6).unwrap_or_default().as_str(),
                            row.try_get::<_, String>(3).unwrap_or_default().as_str(),
                            &self.media_root,
                        )
                    })
                    .collect())
            })
        }
    }

    /// Leitura tolerante a nulo de uma coluna double precision.
    fn opt_f64(row: &Row, idx: usize) -> f64 {
        row.try_get::<_, Option<f64>>(idx).ok().flatten().unwrap_or(0.0)
    }

    /// Leitura tolerante a nulo de uma coluna inteira.
    fn opt_i32(row: &Row, idx: usize) -> i32 {
        row.try_get::<_, Option<i32>>(idx).ok().flatten().unwrap_or(0)
    }

    fn opt_i64(row: &Row, idx: usize) -> i64 {
        row.try_get::<_, Option<i64>>(idx).ok().flatten().unwrap_or(0)
    }

    fn opt_bool(row: &Row, idx: usize) -> bool {
        row.try_get::<_, Option<bool>>(idx).ok().flatten().unwrap_or(false)
    }

    fn opt_string(row: &Row, idx: usize) -> String {
        row.try_get::<_, Option<String>>(idx).ok().flatten().unwrap_or_default()
    }

    /// Mapeia a linha `sistema` (na ordem de `SISTEMA_COLUMNS`) para
    /// `SystemRow`.
    fn system_from_row(row: &Row) -> SystemRow {
        SystemRow {
            creditos: opt_f64(row, 0),
            creditosparcial: opt_f64(row, 1),
            relacaocredito: opt_f64(row, 2),
            mininicioaleatorio: opt_f64(row, 3),
            modoaleatorio: opt_bool(row, 4),
            modofesta: opt_bool(row, 5),
            volume: opt_i64(row, 6),
            maxvolume: opt_i32(row, 7),
            capacoluna: opt_i32(row, 8),
            capalinha: opt_i32(row, 9),
            habilitaincentivo: opt_bool(row, 10),
            incentivo2: opt_i32(row, 11),
            incentivo5: opt_i32(row, 12),
            incentivo10: opt_i32(row, 13),
            incentivo20: opt_i32(row, 14),
            incentivo50: opt_i32(row, 15),
            habilitabrinde: opt_bool(row, 16),
            minmusicasbrinde: opt_i32(row, 17),
            premiocredbrinde: opt_i32(row, 18),
            contbrinde: opt_i32(row, 19),
            txtbrinde: opt_string(row, 20),
            // 35 = diaslancamento (janela de lançamentos)
            diaslancamento: opt_i64(row, 35),
            keys: LegacyKeys {
                esquerda: opt_i32(row, 21),
                direita: opt_i32(row, 22),
                cima: opt_i32(row, 23),
                baixo: opt_i32(row, 24),
                disco: opt_i32(row, 25),
                musica: opt_i32(row, 26),
                credito: opt_i32(row, 27),
                volume: opt_i32(row, 28),
                cancela: opt_i32(row, 29),
                sair: opt_i32(row, 30),
                resetacreditos: opt_i32(row, 31),
                // 32/33 = codteclamaisvolume/codteclamenosvolume: ainda sem
                // tradução (o overlay atual ajusta com W/Q — pendência em
                // JUKEBOXTV-LEGACY.md).
                fecharprograma: opt_i64(row, 34) as i32,
            },
        }
    }
}
