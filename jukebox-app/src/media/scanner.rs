use crate::db::Database;
use crate::state::models::TrackInfo;
use id3::TagLike;
use std::os::unix::fs::MetadataExt;
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
/// Lê tags apenas de arquivos novos/modificados; usa stat sem reler todo o áudio.
pub fn scan_media_directory(db: &mut Database) -> Vec<TrackInfo> {
    scan_directory(db, &resolve_media_dir())
}

fn scan_directory(db: &mut Database, media_dir: &Path) -> Vec<TrackInfo> {
    log::info!("Scanner: Iniciando varredura em {:?}...", media_dir);

    // Garante que o diretório de mídia exista
    if !media_dir.exists() {
        log::warn!(
            "Scanner: Diretório de mídia {:?} não encontrado. Criando...",
            media_dir
        );
        let _ = fs::create_dir_all(&media_dir);
    }

    let known = db.media_fingerprints().unwrap_or_default();
    let mut pending = Vec::new();
    for path in walk_directory(media_dir) {
        let Ok(before) = fingerprint(&path) else { continue; };
        if known.get(path.to_string_lossy().as_ref()) == Some(&before) { continue; }
        let track = extract_track_info(&path, media_dir);
        // Do not mark files replaced/edited while their tags were being read as indexed.
        if fingerprint(&path).ok().as_ref() != Some(&before) { continue; }
        pending.push((track, before));
        if pending.len() == 128 {
            if let Err(e) = db.index_tracks(&pending) { log::error!("Scanner: {e}"); }
            pending.clear();
        }
    }
    if !pending.is_empty() {
        if let Err(e) = db.index_tracks(&pending) { log::error!("Scanner: {e}"); }
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

fn fingerprint(path: &Path) -> std::io::Result<String> {
    let m = fs::metadata(path)?;
    Ok(format!("{}:{}:{}:{}:{}:{}:{}", m.dev(), m.ino(), m.len(),
        m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()))
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
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true)
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
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
pub fn resolve_track_artist(
    file_path: &str,
    media_root: &Path,
    file_artist: &str,
    id3_artist: Option<&str>,
) -> String {
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
        if let Some(folder_name) = parent
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.trim().is_empty())
        {
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
    fn reindexes_replaced_file_without_changing_track_id() {
        let dir = std::env::temp_dir().join(format!("jukebox-scan-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.mp3");
        let write = |path: &Path, title: &str| {
            fs::write(path, b"").unwrap();
            let mut tag = id3::Tag::new(); tag.set_title(title);
            tag.write_to_path(path, id3::Version::Id3v24).unwrap();
        };
        write(&path, "First");
        let mut db = Database::in_memory();
        let first = scan_directory(&mut db, &dir);
        assert_eq!(first[0].title, "First");
        let signatures = db.media_fingerprints().unwrap();
        scan_directory(&mut db, &dir);
        assert_eq!(db.media_fingerprints().unwrap(), signatures);
        let replacement = dir.join("replacement.mp3");
        write(&replacement, "Other");
        fs::rename(replacement, &path).unwrap();
        let updated = scan_directory(&mut db, &dir);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].id, first[0].id);
        assert_eq!(updated[0].title, "Other");
        assert_ne!(db.media_fingerprints().unwrap(), signatures);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn test_resolve_track_artist_uses_folder_name() {
        let media_root = Path::new("/dados/musicas");
        let track_path = "/dados/musicas/Rock/ACDC/BackInBlack/01-HellsBells.mp3";
        let resolved = resolve_track_artist(
            track_path,
            media_root,
            "Parsed Artist",
            Some("ID3 Artist Name"),
        );

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
