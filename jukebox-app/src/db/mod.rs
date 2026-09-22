use crate::state::models::TrackInfo;
use rusqlite::{params, Connection, Result};
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
        // 2. synchronous = NORMAL: Muito mais rápido que FULL, 100% seguro em sistemas de arquivos modernos.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
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

        // Tabela de faixas de mídia indexadas pelo scanner
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS tracks (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                title     TEXT    NOT NULL,
                artist    TEXT    NOT NULL DEFAULT 'Artista Desconhecido',
                album     TEXT    NOT NULL DEFAULT 'Sem Álbum',
                file_path TEXT    NOT NULL UNIQUE,
                file_type TEXT    NOT NULL
            );",
            [],
        )?;

        // Índice para buscas rápidas por artista (usado na navegação do catálogo)
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks (artist);",
            [],
        )?;

        // Garante que a chave 'credits' exista com valor inicial 0
        self.conn.execute(
            "INSERT OR IGNORE INTO system_state (key, value) VALUES ('credits', 0);",
            [],
        )?;

        Ok(())
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

    /// Incrementa créditos de forma atômica e registra na auditoria
    pub fn increment_credits(&mut self, amount: u32) -> Result<u32> {
        let tx = self.conn.transaction()?;

        tx.execute(
            "UPDATE system_state SET value = value + ?1 WHERE key = 'credits';",
            params![amount as i64],
        )?;

        tx.execute(
            "INSERT INTO credits_audit (amount) VALUES (?1);",
            params![amount as i64],
        )?;

        let mut stmt = tx.prepare("SELECT value FROM system_state WHERE key = 'credits';")?;
        let new_credits: i64 = stmt.query_row([], |row| row.get(0))?;

        tx.commit()?;
        Ok(new_credits.max(0) as u32)
    }

    // =========================================================================
    // Catálogo de Faixas de Mídia
    // =========================================================================

    /// Insere ou atualiza uma faixa no banco. Usa file_path como chave de unicidade.
    /// Em caso de conflito (arquivo já indexado), atualiza os metadados.
    pub fn upsert_track(&mut self, track: &TrackInfo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tracks (title, artist, album, file_path, file_type)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(file_path) DO UPDATE SET
                title     = excluded.title,
                artist    = excluded.artist,
                album     = excluded.album,
                file_type = excluded.file_type;",
            params![
                track.title,
                track.artist,
                track.album,
                track.file_path,
                track.file_type,
            ],
        )?;
        Ok(())
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

    /// Retorna todas as faixas cadastradas, ordenadas por artista e título
    pub fn get_all_tracks(&self) -> Result<Vec<TrackInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, artist, album, file_path, file_type
             FROM tracks
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
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }
}
