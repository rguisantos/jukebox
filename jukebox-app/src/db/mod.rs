use crate::settings::Settings;
use crate::state::models::{
    album_initial, fnv64, AlbumInfo, GenreInfo, TrackInfo, SONG_PRICE_MAX, SONG_PRICE_MIN,
};
use rusqlite::{params, Connection, OptionalExtension, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Gerenciador do banco de dados persistente SQLite
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Inicializa a conexão SQLite, configura o modo WAL e cria as tabelas necessárias.
    /// Tenta prioritariamente o caminho de produção `/dados/jukebox.db`.
    /// Caso `/dados` não seja acessível (ambiente de dev local), faz fallback para `./dados/jukebox.db`.
    pub fn open() -> Result<Self> {
        let db_path = Self::resolve_db_path();

        if let Some(parent) = db_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        log::info!("Abrindo banco de dados em: {:?}", db_path);
        let conn = Connection::open(&db_path)?;

        // Otimizações críticas para performance e proteção contra quedas de energia:
        // 1. WAL (Write-Ahead Logging): Leituras e escritas simultâneas sem bloqueio de concorrência.
        // 2. FULL: sync WAL commits before acknowledging paid operations.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "cache_size", "-2000")?; // 2MB de cache em RAM

        let db = Self { conn };
        db.create_tables()?;
        Ok(db)
    }

    /// Determina o caminho ideal para o banco de dados
    fn resolve_db_path() -> PathBuf {
        let prod_dir = Path::new("/dados");
        if prod_dir.exists() && prod_dir.is_dir() {
            prod_dir.join("jukebox.db")
        } else {
            // Fallback para desenvolvimento local caso /dados não exista
            PathBuf::from("./dados/jukebox.db")
        }
    }

    /// Cria as tabelas iniciais se não existirem
    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS playback_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            track_json TEXT NOT NULL,
            price INTEGER NOT NULL CHECK(price > 0)
        );",
        )?;
        self.conn.execute_batch("CREATE TABLE IF NOT EXISTS operator_settings (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS cash_receipts (receipt TEXT PRIMARY KEY, cents INTEGER NOT NULL, credits INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS conversion_session (id INTEGER PRIMARY KEY CHECK(id=1), cents INTEGER NOT NULL, awarded INTEGER NOT NULL);
            INSERT OR IGNORE INTO conversion_session VALUES(1,0,0);")?;
        if self
            .conn
            .prepare("SELECT is_free FROM playback_queue LIMIT 0")
            .is_err()
        {
            self.conn.execute(
                "ALTER TABLE playback_queue ADD COLUMN is_free INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        // Tabela de chave-valor para estado global (créditos, configurações persistentes)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS system_state (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );",
            [],
        )?;

        // Tabela de auditoria de moedas inseridas (útil para fechamento de caixa do operador)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS credits_audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                amount INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;

        // Tabela de faixas de mídia indexadas pelo scanner. O gênero (Módulo 8)
        // alimenta o bloqueio de gêneros no catálogo público.
        // created_at (Unix epoch) é usado para o filtro de recém-adicionados (*).
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS tracks (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                title      TEXT    NOT NULL,
                artist     TEXT    NOT NULL DEFAULT 'Artista Desconhecido',
                album      TEXT    NOT NULL DEFAULT 'Sem Álbum',
                file_path  TEXT    NOT NULL UNIQUE,
                file_type  TEXT    NOT NULL,
                genre      TEXT    NOT NULL DEFAULT 'Desconhecido',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );",
            [],
        )?;

        // Índice para buscas rápidas por artista (usado na navegação do catálogo)
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks (artist);",
            [],
        )?;

        // MÓDULO 8 — Migração de bases criadas pelos Módulos 1–7: adiciona a
        // coluna `genre` e zera a tabela para forçar a re-indexação completa
        // (as tags ID3 de gênero só são lidas pelo scanner; sem o wipe, todo o
        // acervo antigo ficaria eternamente em "Desconhecido"). Os arquivos
        // em /dados/musicas continuam no disco — o próximo scan os recataloga.
        // SQLite não tem "ADD COLUMN IF NOT EXISTS": checa via PRAGMA.
        if !self.has_tracks_genre_column()? {
            match self.conn.execute(
                "ALTER TABLE tracks ADD COLUMN genre TEXT NOT NULL DEFAULT 'Desconhecido';",
                [],
            ) {
                Ok(_) => {
                    log::warn!(
                        "Migração Módulo 8: coluna `genre` criada — catálogo será re-indexado do zero."
                    );
                    self.conn.execute("DELETE FROM tracks;", [])?;
                }
                // Janela de corrida minúscula (outra conexão criou a coluna no
                // intervalo entre a checagem e o ALTER): segue se a coluna existe
                Err(_) if self.has_tracks_genre_column().unwrap_or(false) => {
                    log::info!("Migração Módulo 8: coluna `genre` já criada por outra conexão.");
                }
                Err(e) => return Err(e),
            }
        }

        // MÓDULO 8 — Gêneros bloqueados pelo operador (não aparecem no catálogo)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS blocked_genres (
                genre TEXT PRIMARY KEY
            );",
            [],
        )?;

        // Garante que a chave 'credits' exista com valor inicial 0
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value) VALUES ('credits', 0);",
            [],
        )?;

        // MÓDULO 8 — Caixa parcial (o operador zera ao recolher o dinheiro)
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value) VALUES ('partial_coins', 0);",
            [],
        )?;

        // MÓDULO 8 — Odômetro absoluto (NUNCA zera). Ao migrar uma base dos
        // Módulos 1–7, herda o histórico da auditoria de créditos para o
        // contador não nascer zerado numa máquina que já operou no bar.
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value)
             SELECT 'absolute_coins', COALESCE(SUM(amount), 0)
             FROM credits_audit WHERE amount > 0;",
            [],
        )?;

        // Preço da música em créditos (padrão 1)
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value) VALUES ('song_price', 1);",
            [],
        )?;

        // Dias para considerar álbum recém-adicionado (*) (padrão 30 dias)
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value) VALUES ('recent_days', 30);",
            [],
        )?;

        // Adiciona a coluna `created_at` na tabela `tracks` se não existir
        // (migração de bancos criados antes desta versão do schema).
        // NOTA: ALTER TABLE ADD COLUMN só aceita defaults constantes no SQLite —
        // usamos 0 (epoch Unix = 1970) para que faixas antigas não sejam "recentes".
        if !self.has_tracks_created_at_column()? {
            self.conn.execute(
                "ALTER TABLE tracks ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;",
                [],
            )?;
            log::info!("Migração: coluna `created_at` adicionada à tabela tracks (faixas existentes com created_at=0).");
        }

        // v9: catálogo ordenado por pasta Gênero/Artista/Álbum. Bases antigas
        // têm gênero só do ID3 (ou "Desconhecido") — wipe força o scanner a
        // reler as pastas. Os arquivos em /dados/musicas não são tocados.
        const CATALOG_SCHEMA: i64 = 9;
        let schema = self.read_counter("catalog_schema").unwrap_or(0);
        if schema < CATALOG_SCHEMA {
            log::warn!(
                "Migração catálogo v{}: re-indexando (gênero/artista/álbum pela pasta).",
                CATALOG_SCHEMA
            );
            self.conn.execute("DELETE FROM tracks;", [])?;
            self.conn.execute(
                "INSERT INTO system_state (key, value) VALUES ('catalog_schema', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
                params![CATALOG_SCHEMA],
            )?;
        }

        Ok(())
    }

    fn has_tracks_created_at_column(&self) -> Result<bool> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(tracks);")?;
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(names.contains(&"created_at".to_string()))
    }

    /// Verifica se a tabela `tracks` já possui a coluna `genre` (Módulo 8)
    fn has_tracks_genre_column(&self) -> Result<bool> {
        let mut stmt = self.conn.prepare("PRAGMA table_info(tracks);")?;
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(names.iter().any(|name| name == "genre"))
    }

    // =========================================================================
    // Créditos
    // =========================================================================

    /// Retorna a quantidade atual de créditos gravada no banco
    pub fn get_credits(&self) -> Result<u32> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM system_state WHERE key = 'credits';")?;
        let credits: i64 = stmt.query_row([], |row| row.get(0))?;
        Ok(credits.max(0) as u32)
    }

    /// Incrementa créditos de forma atômica e registra na auditoria.
    /// MÓDULO 8: toda entrada (moeda ou PIX) alimenta também os contadores
    /// antifraude — o caixa parcial (zerado pelo operador no recolhimento)
    /// e o odômetro absoluto (nunca zerado, para conferência patrimonial).
    pub fn increment_credits(&mut self, amount: u32) -> Result<u32> {
        let tx = self.conn.transaction()?;

        tx.execute(
            "UPDATE system_state SET value = value + ?1 WHERE key = 'credits';",
            params![amount as i64],
        )?;

        tx.execute(
            "UPDATE system_state SET value = value + ?1 WHERE key = 'partial_coins';",
            params![amount as i64],
        )?;

        tx.execute(
            "UPDATE system_state SET value = value + ?1 WHERE key = 'absolute_coins';",
            params![amount as i64],
        )?;

        tx.execute(
            "INSERT INTO credits_audit (amount) VALUES (?1);",
            params![amount as i64],
        )?;

        let new_credits: i64 = {
            let mut stmt = tx.prepare("SELECT value FROM system_state WHERE key = 'credits';")?;
            stmt.query_row([], |row| row.get(0))?
        };

        tx.commit()?;
        Ok(new_credits.max(0) as u32)
    }

    pub fn settings(&self) -> Result<Settings> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM operator_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .optional()?;
        match json {
            None => Ok(Settings::default()),
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            }),
        }
    }
    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        settings
            .validate()
            .map_err(|e| rusqlite::Error::InvalidParameterName(e))?;
        let old = self.settings()?;
        let tx = self.conn.transaction()?;
        if old.packages != settings.packages {
            let cents: u64 =
                tx.query_row("SELECT cents FROM conversion_session WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            tx.execute(
                "UPDATE conversion_session SET cents=?1, awarded=0 WHERE id=1",
                [old.convert(cents).1],
            )?;
        }
        tx.execute("INSERT INTO operator_settings(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json", [serde_json::to_string(settings).unwrap()])?;
        tx.commit()
    }
    /// Deduplicated money receipt; incremental deposits receive package bonus only once.
    pub fn accept_money(&mut self, cents: u32, receipt: &str) -> Result<(u32, u32)> {
        if cents == 0 || cents > 100_000 {
            return Err(rusqlite::Error::InvalidParameterName(
                "Valor inválido".into(),
            ));
        }
        let settings = self.settings()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM cash_receipts WHERE receipt=?1)",
            [receipt],
            |r| r.get(0),
        )?;
        let balance: u32 = tx.query_row(
            "SELECT value FROM system_state WHERE key='credits'",
            [],
            |r| r.get(0),
        )?;
        if seen {
            return Ok((balance, 0));
        }
        let (total, awarded): (u64, u64) = tx.query_row(
            "SELECT cents,awarded FROM conversion_session WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let total = total
            .checked_add(cents as u64)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        let converted = settings.convert(total).0;
        let delta = u32::try_from(converted.saturating_sub(awarded))
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let new_balance = balance
            .checked_add(delta)
            .filter(|b| *b <= i32::MAX as u32)
            .ok_or(rusqlite::Error::InvalidQuery)?;
        tx.execute(
            "UPDATE conversion_session SET cents=?1,awarded=?2 WHERE id=1",
            params![total, converted],
        )?;
        tx.execute(
            "INSERT INTO cash_receipts VALUES(?1,?2,?3)",
            params![receipt, cents, delta],
        )?;
        tx.execute("UPDATE system_state SET value=value+?1 WHERE key IN ('credits','partial_coins','absolute_coins')", [delta])?;
        tx.execute("INSERT INTO credits_audit(amount) VALUES(?1)", [delta])?;
        tx.commit()?;
        Ok((new_balance, delta))
    }
    pub fn random_track(&self, exclude: Option<&str>) -> Result<Option<TrackInfo>> {
        self.conn.query_row("SELECT id,title,artist,album,file_path,file_type,genre FROM tracks
            WHERE genre NOT IN (SELECT genre FROM blocked_genres) AND (?1 IS NULL OR file_path != ?1)
            ORDER BY RANDOM() LIMIT 1", [exclude], |r|Ok(TrackInfo{id:r.get(0)?,title:r.get(1)?,artist:r.get(2)?,album:r.get(3)?,file_path:r.get(4)?,file_type:r.get(5)?,genre:r.get(6)?})).optional()
    }

    /// Persist a purchase and debit in ONE transaction. Current song counts toward limit.
    pub fn reserve_track(&mut self, track: &TrackInfo) -> Result<Option<(i64, u32)>> {
        let json = serde_json::to_string(track)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let settings = self.settings()?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let last: Option<String> = tx
            .query_row(
                "SELECT track_json FROM playback_queue ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if last
            .and_then(|j| serde_json::from_str::<TrackInfo>(&j).ok())
            .map(|t| t.file_path == track.file_path)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM playback_queue", [], |r| r.get(0))?;
        let price: i64 = tx.query_row(
            "SELECT value FROM system_state WHERE key='song_price'",
            [],
            |r| r.get(0),
        )?;
        let balance: i64 = tx.query_row(
            "SELECT value FROM system_state WHERE key='credits'",
            [],
            |r| r.get(0),
        )?;
        if count >= 20 || price < 1 || (!settings.free_play && balance < price) {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO playback_queue(track_json,price,is_free) VALUES (?1,?2,?3)",
            params![json, price, settings.free_play],
        )?;
        let id = tx.last_insert_rowid();
        let price = if settings.free_play { 0 } else { price };
        tx.execute(
            "UPDATE system_state SET value=value-?1 WHERE key='credits'",
            [price],
        )?;
        tx.execute("INSERT INTO credits_audit(amount) VALUES (?1)", [-price])?;
        let total: u64 =
            tx.query_row("SELECT cents FROM conversion_session WHERE id=1", [], |r| {
                r.get(0)
            })?;
        tx.execute(
            "UPDATE conversion_session SET cents=?1,awarded=0 WHERE id=1",
            [settings.convert(total).1],
        )?;
        tx.commit()?;
        Ok(Some((id, (balance - price) as u32)))
    }

    pub fn pending_tracks(&self) -> Result<Vec<(i64, TrackInfo)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,track_json FROM playback_queue ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            let json: String = row.get(1)?;
            let track = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok((row.get(0)?, track))
        })?;
        rows.collect()
    }

    /// Refund failures without incrementing collection counters; duplicate completion is harmless.
    pub fn finish_track(&mut self, id: i64, refund: bool) -> Result<u32> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if refund {
            tx.execute("UPDATE system_state SET value=value+COALESCE((SELECT CASE WHEN is_free=1 THEN 0 ELSE price END FROM playback_queue WHERE id=?1),0) WHERE key='credits'", [id])?;
            tx.execute(
                "INSERT INTO credits_audit(amount) SELECT CASE WHEN is_free=1 THEN 0 ELSE price END FROM playback_queue WHERE id=?1",
                [id],
            )?;
        }
        tx.execute("DELETE FROM playback_queue WHERE id=?1", [id])?;
        let balance: i64 = tx.query_row(
            "SELECT value FROM system_state WHERE key='credits'",
            [],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(balance.max(0) as u32)
    }

    // =========================================================================
    // Catálogo de Faixas de Mídia
    // =========================================================================

    /// Insere ou atualiza uma faixa no banco. Usa file_path como chave de unicidade.
    /// Em caso de conflito (arquivo já indexado), atualiza os metadados.
    pub fn upsert_track(&mut self, track: &TrackInfo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tracks (title, artist, album, file_path, file_type, genre)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(file_path) DO UPDATE SET
                title     = excluded.title,
                artist    = excluded.artist,
                album     = excluded.album,
                file_type = excluded.file_type,
                genre     = excluded.genre;",
            params![
                track.title,
                track.artist,
                track.album,
                track.file_path,
                track.file_type,
                track.genre,
            ],
        )?;
        Ok(())
    }

    /// Batch catalog writes so FULL durability does not require one disk sync per track.
    pub fn upsert_tracks(&mut self, tracks: &[TrackInfo]) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO tracks (title,artist,album,file_path,file_type,genre)
                VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(file_path) DO UPDATE SET
                title=excluded.title, artist=excluded.artist, album=excluded.album,
                file_type=excluded.file_type, genre=excluded.genre",
            )?;
            for track in tracks {
                insert.execute(params![
                    track.title,
                    track.artist,
                    track.album,
                    track.file_path,
                    track.file_type,
                    track.genre
                ])?;
            }
        }
        tx.commit()
    }

    /// Retorna todos os file_paths já cadastrados no banco (para o scanner incremental)
    pub fn get_all_track_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT file_path FROM tracks;")?;
        let paths = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(paths)
    }

    /// Retorna todas as faixas do catálogo público, ordenadas por artista e
    /// título. MÓDULO 8: gêneros bloqueados ficam de fora da consulta.
    pub fn get_all_tracks(&self) -> Result<Vec<TrackInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, artist, album, file_path, file_type, genre
             FROM tracks
             WHERE genre NOT IN (SELECT genre FROM blocked_genres)
             ORDER BY artist ASC, title ASC;",
        )?;

        let tracks = stmt
            .query_map([], |row| {
                Ok(TrackInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    album: row.get(3)?,
                    file_path: row.get(4)?,
                    file_type: row.get(5)?,
                    genre: row.get(6)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    // =========================================================================
    // MÓDULO 7 — Catálogo agrupado por álbum (navegação por capas)
    // =========================================================================

    /// Catálogo agrupado por (artista, álbum), ordenado para o carrossel:
    /// gênero → artista → álbum → faixa.
    pub fn get_albums(&self) -> Result<Vec<AlbumInfo>> {
        let recent_days = self.get_recent_days().unwrap_or(30);
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cutoff_secs = now_secs - (recent_days as i64 * 86400);

        let mut stmt = self.conn.prepare(
            "SELECT id, title, artist, album, file_path, file_type, genre, COALESCE(created_at, 0)
             FROM tracks
             WHERE genre NOT IN (SELECT genre FROM blocked_genres)
             ORDER BY genre COLLATE NOCASE ASC,
                      artist COLLATE NOCASE ASC,
                      album COLLATE NOCASE ASC,
                      title COLLATE NOCASE ASC;",
        )?;

        let rows = stmt
            .query_map([], |row| {
                let track = TrackInfo {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    artist: row.get(2)?,
                    album: row.get(3)?,
                    file_path: row.get(4)?,
                    file_type: row.get(5)?,
                    genre: row.get(6)?,
                };
                let created_at: i64 = row.get(7)?;
                Ok((track, created_at))
            })?
            .filter_map(|r| r.ok());

        let mut albums: Vec<AlbumInfo> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();

        for (track, created_at) in rows {
            let key = format!("{}|{}", track.artist, track.album);
            let is_recent = created_at >= cutoff_secs;

            match index.get(&key) {
                Some(&position) => {
                    let alb = &mut albums[position];
                    alb.tracks.push(track);
                    if is_recent {
                        alb.is_recent = true;
                    }
                }
                None => {
                    let initial = album_initial(&track.album);
                    let palette = (fnv64(&key) % 6) as u32;
                    index.insert(key.clone(), albums.len());
                    albums.push(AlbumInfo {
                        title: track.album.clone(),
                        artist: track.artist.clone(),
                        genre: track.genre.clone(),
                        key,
                        initial,
                        palette,
                        is_recent,
                        tracks: vec![track],
                    });
                }
            }
        }

        Ok(albums)
    }

    pub fn get_recent_days(&self) -> Result<u32> {
        self.read_counter("recent_days").map(|v| (v as u32).max(1))
    }

    pub fn set_recent_days(&self, days: u32) -> Result<()> {
        self.conn.execute(
            "INSERT INTO system_state (key, value) VALUES ('recent_days', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            params![days as i64],
        )?;
        Ok(())
    }

    // =========================================================================
    // MÓDULO 8 — Contadores antifraude, preço dinâmico e bloqueio de gêneros
    // =========================================================================

    /// Lê um contador da tabela chave-valor (0 se a chave ainda não existir).
    /// Distingue "chave ausente" (Ok(0)) de erro real de I/O (Err).
    fn read_counter(&self, key: &str) -> Result<i64> {
        match self.conn.query_row(
            "SELECT value FROM system_state WHERE key = ?1;",
            params![key],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(value) => Ok(value.max(0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(e),
        }
    }

    /// Caixa parcial: créditos inseridos desde o último recolhimento do
    /// operador (zerado pela opção "Zerar Caixa Parcial" do menu).
    pub fn get_partial_coins(&self) -> Result<i64> {
        self.read_counter("partial_coins")
    }

    /// Odômetro absoluto: total histórico de créditos inseridos — NUNCA zera
    /// (proteção patrimonial: conferência com o caixa parcial de cada período).
    pub fn get_absolute_coins(&self) -> Result<i64> {
        self.read_counter("absolute_coins")
    }

    /// Odômetro patrimonial em reais: soma de todos os recebimentos da tabela
    /// `cash_receipts` (centavos). Diferente dos contadores legados em
    /// créditos, esta soma é idempotente — repetir um identificador de recibo
    /// não a infla — e serve de base para o futuro PIX em reais. NUNCA zera.
    pub fn get_total_receipts_cents(&self) -> Result<i64> {
        self.conn.query_row(
            "SELECT COALESCE(SUM(cents), 0) FROM cash_receipts;",
            [],
            |row| row.get::<_, i64>(0),
        )
    }

    /// Zera o caixa parcial (após o operador esvaziar o moedeiro/gaveta).
    /// O odômetro absoluto permanece intacto.
    pub fn reset_partial_coins(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE system_state SET value = 0 WHERE key = 'partial_coins';",
            [],
        )?;
        Ok(())
    }

    /// Zera os créditos inseridos e ainda não gastos na máquina (créditos
    /// "abandonados" pelo freguês). O caixa parcial e o odômetro não mudam:
    /// o dinheiro já foi contado na entrada.
    pub fn reset_current_credits(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE system_state SET value = 0 WHERE key = 'credits';",
            [],
        )?;
        Ok(())
    }

    /// Preço atual da música em créditos (padrão 1, limitado a 1..=10)
    pub fn get_song_price(&self) -> Result<u32> {
        let price = self.read_counter("song_price")?;
        Ok(price.clamp(SONG_PRICE_MIN as i64, SONG_PRICE_MAX as i64) as u32)
    }

    /// Define o preço da música em créditos (persistente entre reinícios)
    pub fn set_song_price(&self, price: u32) -> Result<()> {
        let clamped = price.clamp(SONG_PRICE_MIN, SONG_PRICE_MAX);
        self.conn.execute(
            "INSERT INTO system_state (key, value) VALUES ('song_price', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            params![clamped as i64],
        )?;
        Ok(())
    }

    /// Lista todos os gêneros do acervo com a contagem de faixas e a situação
    /// de bloqueio — alimenta o submenu "Bloquear Gêneros" do operador.
    pub fn get_genres(&self) -> Result<Vec<GenreInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT genre,
                    COUNT(*) AS total,
                    EXISTS(SELECT 1 FROM blocked_genres bg WHERE bg.genre = tracks.genre)
             FROM tracks
             GROUP BY genre
             ORDER BY genre COLLATE NOCASE ASC;",
        )?;

        let genres = stmt
            .query_map([], |row| {
                Ok(GenreInfo {
                    name: row.get(0)?,
                    track_count: row.get(1)?,
                    blocked: row.get(2)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(genres)
    }

    /// Alterna o bloqueio de um gênero. Retorna o novo estado
    /// (true = bloqueado, false = liberado no catálogo público).
    pub fn toggle_genre_block(&self, genre: &str) -> Result<bool> {
        let removed = self.conn.execute(
            "DELETE FROM blocked_genres WHERE genre = ?1;",
            params![genre],
        )?;

        if removed > 0 {
            Ok(false)
        } else {
            self.conn.execute(
                "INSERT INTO blocked_genres (genre) VALUES (?1);",
                params![genre],
            )?;
            Ok(true)
        }
    }

    /// Lê um valor inteiro da tabela chave-valor (None se a chave não existe)
    pub fn get_config_i64(&self, key: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM system_state WHERE key = ?1;")?;
        let mut rows = stmt.query(params![key])?;
        Ok(rows.next()?.map(|row| row.get::<_, i64>(0)).transpose()?)
    }

    /// Grava um valor inteiro na tabela chave-valor (upsert atômico)
    pub fn set_config_i64(&self, key: &str, value: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO system_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value;",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod queue_tests {
    use super::*;
    fn database() -> Database {
        let db = Database {
            conn: Connection::open_in_memory().unwrap(),
        };
        db.create_tables().unwrap();
        db
    }
    fn track() -> TrackInfo {
        TrackInfo {
            id: 1,
            title: "Música".into(),
            artist: "Artista".into(),
            album: "Álbum".into(),
            file_path: "/dados/musicas/a.mp3".into(),
            file_type: "mp3".into(),
            genre: "Rock".into(),
        }
    }
    #[test]
    fn duplicate_tail_does_not_debit_but_a_b_a_is_allowed() {
        let mut db = database();
        db.increment_credits(3).unwrap();
        assert!(db.reserve_track(&track()).unwrap().is_some());
        assert!(db.reserve_track(&track()).unwrap().is_none());
        assert_eq!(db.get_credits().unwrap(), 2);
        let mut b = track();
        b.file_path = "/b.mp3".into();
        db.reserve_track(&b).unwrap().unwrap();
        db.reserve_track(&track()).unwrap().unwrap();
        assert_eq!(db.get_credits().unwrap(), 0);
    }
    #[test]
    fn total_receipts_cents_is_idempotent_and_survives_legacy_counters() {
        let mut db = database();
        // Vazio: COALESCE devolve 0, não erro.
        assert_eq!(db.get_total_receipts_cents().unwrap(), 0);
        db.accept_money(100, "a").unwrap();
        // Recibo duplicado não soma duas vezes (idempotência patrimonial).
        db.accept_money(100, "a").unwrap();
        db.accept_money(500, "b").unwrap();
        assert_eq!(db.get_total_receipts_cents().unwrap(), 600);
        // Zerar o caixa parcial NUNCA toca no odômetro em reais.
        db.reset_partial_coins().unwrap();
        assert_eq!(db.get_total_receipts_cents().unwrap(), 600);
        assert_eq!(db.get_partial_coins().unwrap(), 0);
    }
    #[test]
    fn incremental_cash_has_same_bonus_as_single_receipt_and_deduplicates() {
        let mut a = database();
        let mut b = database();
        let mut s = Settings::default();
        s.packages[1].credits = 6;
        s.packages[2].credits = 14;
        a.save_settings(&s).unwrap();
        b.save_settings(&s).unwrap();
        for i in 0..5 {
            a.accept_money(100, &format!("receipt-{i}")).unwrap();
        }
        b.accept_money(500, "single").unwrap();
        assert_eq!(a.get_credits().unwrap(), 6);
        assert_eq!(a.get_credits().unwrap(), b.get_credits().unwrap());
        assert_eq!(a.accept_money(100, "receipt-4").unwrap(), (6, 0));
        a.reserve_track(&track()).unwrap().unwrap();
        assert_eq!(a.accept_money(100, "next-session").unwrap(), (6, 1));
    }
    #[test]
    fn fractional_remainder_survives_a_purchase() {
        let mut db = database();
        db.increment_credits(1).unwrap();
        assert_eq!(db.accept_money(50, "half").unwrap(), (1, 0));
        db.reserve_track(&track()).unwrap().unwrap();
        assert_eq!(db.accept_money(50, "other-half").unwrap(), (1, 1));
    }
    #[test]
    fn free_play_neither_debits_nor_refunds_unpaid_credit() {
        let mut db = database();
        let mut s = Settings::default();
        s.free_play = true;
        db.save_settings(&s).unwrap();
        let (id, balance) = db.reserve_track(&track()).unwrap().unwrap();
        assert_eq!(balance, 0);
        assert_eq!(db.finish_track(id, true).unwrap(), 0);
        assert_eq!(db.get_absolute_coins().unwrap(), 0);
    }
    #[test]
    fn settings_and_pin_persist_without_plaintext_password() {
        let mut db = database();
        let mut s = Settings::default();
        s.set_pin("123456").unwrap();
        s.attract_minutes = 5;
        db.save_settings(&s).unwrap();
        let loaded = db.settings().unwrap();
        assert!(loaded.verify_pin("123456"));
        assert_eq!(loaded.attract_minutes, 5);
        let json: String = db
            .conn
            .query_row("SELECT json FROM operator_settings", [], |r| r.get(0))
            .unwrap();
        assert!(!json.contains("123456"));
    }

    #[test]
    fn full_queue_cannot_consume_credit() {
        let mut db = database();
        db.increment_credits(25).unwrap();
        for i in 0..20 {
            let mut t = track();
            t.file_path = format!("/{i}.mp3");
            assert!(db.reserve_track(&t).unwrap().is_some());
        }
        assert!(db.reserve_track(&track()).unwrap().is_none());
        assert_eq!(db.get_credits().unwrap(), 5);
        assert_eq!(db.pending_tracks().unwrap().len(), 20);
    }
    #[test]
    fn refund_is_idempotent_and_does_not_inflate_collection() {
        let mut db = database();
        db.increment_credits(5).unwrap();
        let (id, balance) = db.reserve_track(&track()).unwrap().unwrap();
        assert_eq!(balance, 4);
        assert_eq!(db.finish_track(id, true).unwrap(), 5);
        assert_eq!(db.finish_track(id, true).unwrap(), 5);
        assert_eq!(db.get_absolute_coins().unwrap(), 5);
        assert!(db.pending_tracks().unwrap().is_empty());
    }
    #[test]
    fn insufficient_balance_does_not_enqueue() {
        let mut db = database();
        assert!(db.reserve_track(&track()).unwrap().is_none());
        assert!(db.pending_tracks().unwrap().is_empty());
    }
    #[test]
    fn committed_queue_survives_reopening_database() {
        let path = std::env::temp_dir().join(format!(
            "jukebox-queue-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let mut db = Database {
                conn: Connection::open(&path).unwrap(),
            };
            db.create_tables().unwrap();
            db.increment_credits(3).unwrap();
            db.reserve_track(&track()).unwrap().unwrap();
        }
        {
            let db = Database {
                conn: Connection::open(&path).unwrap(),
            };
            assert_eq!(db.get_credits().unwrap(), 2);
            assert_eq!(
                db.pending_tracks().unwrap()[0].1.file_path,
                track().file_path
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}
