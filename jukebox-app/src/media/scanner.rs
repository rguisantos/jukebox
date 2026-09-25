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
            let track = extract_track_info(file_path, &media_dir);
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

    // Garante que todas as faixas (novas e existentes) utilizem artista (pasta) e álbum (pasta)
    if let Ok(all_tracks) = db.get_all_tracks() {
        for mut track in all_tracks {
            let path = Path::new(&track.file_path);
            let (file_artist, _) = parse_filename(path);
            let correct_artist = resolve_track_artist(&track.file_path, &media_dir, &file_artist, None);
            let correct_album = resolve_track_album(&track.file_path, &media_dir, None);
            if track.artist != correct_artist || track.album != correct_album {
                track.artist = correct_artist;
                track.album = correct_album;
                let _ = db.upsert_track(&track);
            }
        }
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

/// Resolve o nome do artista preferindo a pasta do artista no sistema de arquivos (não ID3).
pub fn resolve_track_artist(file_path: &str, media_root: &Path, file_artist: &str, id3_artist: Option<&str>) -> String {
    let path = Path::new(file_path);
    let from_dir = infer_from_folder(path, media_root);
    if let Some(folder_artist) = from_dir.artist.filter(|s| !s.trim().is_empty()) {
        return folder_artist;
    }
    if !file_artist.trim().is_empty() && file_artist != "Artista Desconhecido" {
        return file_artist.to_string();
    }
    if let Some(artist) = id3_artist.filter(|s| !s.trim().is_empty()) {
        return artist.to_string();
    }
    String::from("Artista Desconhecido")
}

/// Resolve o nome do álbum preferindo a pasta do álbum no sistema de arquivos (não ID3).
pub fn resolve_track_album(file_path: &str, media_root: &Path, id3_album: Option<&str>) -> String {
    let path = Path::new(file_path);
    let from_dir = infer_from_folder(path, media_root);
    if let Some(folder_album) = from_dir.album.filter(|s| !s.trim().is_empty()) {
        return folder_album;
    }
    if let Some(parent) = path.parent().filter(|p| *p != media_root) {
        if let Some(folder_name) = parent.file_name().and_then(|s| s.to_str()).filter(|s| !s.trim().is_empty()) {
            return folder_name.to_string();
        }
    }
    if let Some(album) = id3_album.filter(|s| !s.trim().is_empty()) {
        return album.to_string();
    }
    String::from("Sem Álbum")
}

/// Extrai metadados de uma faixa. Prefere a pasta para Artista e Álbum (para agrupar o carrossel);
/// tags ID3 preenchem título e gênero (quando não definidos na pasta).
fn extract_track_info(path: &Path, media_root: &Path) -> TrackInfo {
    let file_path = path.to_string_lossy().to_string();
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_lowercase();

    let from_dir = infer_from_folder(path, media_root);
    let (file_artist, file_title) = parse_filename(path);

    let mut title = file_title;
    let mut genre = from_dir
        .genre
        .clone()
        .unwrap_or_else(|| String::from("Desconhecido"));
    let mut id3_artist: Option<String> = None;
    let mut id3_album: Option<String> = None;

    if extension == "mp3" {
        if let Ok(tag) = id3::Tag::read_from_path(path) {
            if let Some(t) = tag.title().filter(|s| !s.trim().is_empty()) {
                title = t.to_string();
            }
            if let Some(a) = tag.artist().filter(|s| !s.trim().is_empty()) {
                id3_artist = Some(a.to_string());
            }
            if let Some(a) = tag.album().filter(|s| !s.trim().is_empty()) {
                id3_album = Some(a.to_string());
            }
            // Pasta do acervo (Gênero/Artista/Álbum) manda no estilo; ID3 só
            // preenche quando a faixa não está nessa árvore.
            if from_dir.genre.is_none() {
                if let Some(g) = tag.genre().filter(|s| !s.trim().is_empty()) {
                    genre = g.to_string();
                }
            }
        }
    }

    let artist = resolve_track_artist(&file_path, media_root, &file_artist, id3_artist.as_deref());
    let album = resolve_track_album(&file_path, media_root, id3_album.as_deref());

    TrackInfo {
        id: 0,
        title,
        artist,
        album,
        genre,
        file_path,
        file_type: extension,
    }
}

struct FolderTags {
    genre: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

/// Lê gênero / artista / álbum a partir de `media_root/Gênero/Artista/Álbum/faixa`.
fn infer_from_folder(path: &Path, media_root: &Path) -> FolderTags {
    let empty = FolderTags {
        genre: None,
        artist: None,
        album: None,
    };
    let rel = match path.strip_prefix(media_root) {
        Ok(rel) => rel,
        Err(_) => return empty,
    };
    let parent = match rel.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return empty,
    };
    let dirs: Vec<String> = parent
        .iter()
        .filter_map(|s| s.to_str().map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .collect();

    match dirs.len() {
        0 => empty,
        1 => FolderTags {
            album: Some(dirs[0].clone()),
            ..empty
        },
        2 => FolderTags {
            artist: Some(dirs[0].clone()),
            album: Some(dirs[1].clone()),
            ..empty
        },
        _ => FolderTags {
            genre: Some(dirs[0].clone()),
            artist: Some(dirs[dirs.len() - 2].clone()),
            album: Some(dirs[dirs.len() - 1].clone()),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_track_artist_uses_folder_name() {
        let media_root = Path::new("/dados/musicas");
        let track_path = "/dados/musicas/Rock/ACDC/BackInBlack/01-HellsBells.mp3";
        let resolved = resolve_track_artist(track_path, media_root, "Parsed Artist", Some("ID3 Artist Name"));
        
        // Deve priorizar a pasta do artista ("ACDC") em vez do ID3 tag ou filename
        assert_eq!(resolved, "ACDC");
    }

    #[test]
    fn test_resolve_track_album_uses_folder_name() {
        let media_root = Path::new("/dados/musicas");
        let track_path = "/dados/musicas/Rock/ACDC/BackInBlack/01-HellsBells.mp3";
        let resolved = resolve_track_album(track_path, media_root, Some("ID3 Album Name"));
        
        // Deve priorizar a pasta do álbum ("BackInBlack") em vez do ID3 tag
        assert_eq!(resolved, "BackInBlack");
    }

    #[test]
    fn test_resolve_track_album_fallback_to_id3_when_no_subfolder() {
        let media_root = Path::new("/dados/musicas");
        let track_path = "/dados/musicas/01-Track.mp3";
        let resolved = resolve_track_album(track_path, media_root, Some("ID3 Album Name"));
        
        // Se estiver diretamente na raiz de mídia, usa o ID3 se disponível
        assert_eq!(resolved, "ID3 Album Name");
    }
}


