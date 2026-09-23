use crate::db::Database;
use crate::state::models::TrackInfo;
use id3::TagLike;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Extensões de arquivo reconhecidas pelo scanner como mídia válida
const SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "mp4", "wav", "wmv", "mpeg"];

/// Diretório de produção onde os arquivos de mídia ficam na máquina Arcade
const PROD_MEDIA_DIR: &str = "/dados/musicas";

/// Diretório de fallback para desenvolvimento local
const DEV_MEDIA_DIR: &str = "./dados/musicas";

/// Resolve o diretório correto de mídia (produção ou desenvolvimento)
pub fn resolve_media_dir() -> PathBuf {
    let prod = Path::new(PROD_MEDIA_DIR);
    if prod.exists() && prod.is_dir() {
        prod.to_path_buf()
    } else {
        PathBuf::from(DEV_MEDIA_DIR)
    }
}

/// Scanner de Mídia: Varre o diretório de músicas e indexa no banco de dados.
///
/// Otimizações para HD mecânico lento (Sempron 145):
/// 1. Primeiro consulta quais file_paths já estão cadastrados no banco.
/// 2. Apenas processa (lê tags ID3) dos arquivos NOVOS que ainda não foram indexados.
/// 3. Não realiza nenhuma operação se a pasta não existir ou estiver vazia.
pub fn scan_media_directory(db: &mut Database) -> Vec<TrackInfo> {
    let media_dir = resolve_media_dir();

    log::info!("Scanner: Iniciando varredura em {:?}...", media_dir);

    // Garante que o diretório de mídia exista
    if !media_dir.exists() {
        log::warn!("Scanner: Diretório de mídia {:?} não encontrado. Criando...", media_dir);
        let _ = fs::create_dir_all(&media_dir);
    }

    // Otimização: Obtém os caminhos já indexados no banco para evitar releitura de tags
    let known_paths: HashSet<String> = db
        .get_all_track_paths()
        .unwrap_or_default()
        .into_iter()
        .collect();

    log::info!(
        "Scanner: {} faixas já indexadas no banco. Buscando novos arquivos...",
        known_paths.len()
    );

    // Percorre recursivamente o diretório de mídia
    let new_files = walk_directory(&media_dir)
        .into_iter()
        .filter(|path| {
            let path_str = path.to_string_lossy().to_string();
            !known_paths.contains(&path_str)
        })
        .collect::<Vec<_>>();

    if new_files.is_empty() {
        log::info!("Scanner: Nenhum arquivo novo encontrado. Catálogo atualizado.");
    } else {
        log::info!(
            "Scanner: {} novos arquivos encontrados. Indexando...",
            new_files.len()
        );

        for file_path in &new_files {
            let track = extract_track_info(file_path);
            if let Err(e) = db.upsert_track(&track) {
                log::error!(
                    "Scanner: Erro ao indexar {:?}: {}",
                    file_path.file_name().unwrap_or_default(),
                    e
                );
            }
        }

        log::info!("Scanner: Indexação de novos arquivos concluída.");
    }

    // Retorna o catálogo completo atualizado para enviar à UI
    match db.get_all_tracks() {
        Ok(tracks) => {
            log::info!(
                "Scanner: Catálogo carregado com {} faixas no total.",
                tracks.len()
            );
            tracks
        }
        Err(e) => {
            log::error!("Scanner: Falha ao carregar catálogo do banco: {}", e);
            Vec::new()
        }
    }
}

/// Percorre recursivamente um diretório coletando todos os arquivos de mídia suportados
fn walk_directory(dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("Scanner: Não foi possível ler {:?}: {}", dir, e);
            return results;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Recursão em subpastas (ex: /dados/musicas/rock/, /dados/musicas/sertanejo/)
            results.extend(walk_directory(&path));
        } else if is_supported_media(&path) {
            results.push(path);
        }
    }

    results
}

/// Verifica se um arquivo possui extensão de mídia suportada
fn is_supported_media(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Extrai metadados de uma faixa de mídia (tags ID3 para MP3, nome do arquivo como fallback)
fn extract_track_info(path: &Path) -> TrackInfo {
    let file_path = path.to_string_lossy().to_string();
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_lowercase();

    // Tenta ler tags ID3 para arquivos MP3
    if extension == "mp3" {
        if let Ok(tag) = id3::Tag::read_from_path(path) {
            return TrackInfo {
                id: 0, // Será atribuído pelo SQLite
                title: tag
                    .title()
                    .unwrap_or_else(|| filename_without_ext(path))
                    .to_string(),
                artist: tag.artist().unwrap_or("Artista Desconhecido").to_string(),
                album: tag.album().unwrap_or("Álbum Desconhecido").to_string(),
                // MÓDULO 8: gênero da tag ID3 alimenta o bloqueio de gêneros
                genre: tag.genre().unwrap_or("Desconhecido").to_string(),
                file_path,
                file_type: extension.clone(),
            };
        }
    }

    // Fallback para todos os formatos: extrai do padrão "Artista - Titulo.ext" no nome do arquivo
    let (artist, title) = parse_filename(path);

    TrackInfo {
        id: 0,
        title,
        artist,
        album: String::from("Sem Álbum"),
        genre: String::from("Desconhecido"),
        file_path,
        file_type: extension,
    }
}

/// Extrai artista e título do nome do arquivo no padrão "Artista - Titulo.ext"
/// Caso não encontre o separador " - ", retorna "Artista Desconhecido" e o nome completo como título.
fn parse_filename(path: &Path) -> (String, String) {
    let name = filename_without_ext(path).to_string();

    if let Some(pos) = name.find(" - ") {
        let artist = name[..pos].trim().to_string();
        let title = name[pos + 3..].trim().to_string();
        if !artist.is_empty() && !title.is_empty() {
            return (artist, title);
        }
    }

    (String::from("Artista Desconhecido"), name)
}

/// Retorna o nome do arquivo sem a extensão
fn filename_without_ext(path: &Path) -> &str {
    path.file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("Sem Nome")
}
