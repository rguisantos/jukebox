/// Informações de uma faixa de mídia indexada no catálogo do Jukebox.
/// Utilizado como struct intermediária entre o banco de dados e a interface Slint.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    /// ID único da faixa no SQLite (PRIMARY KEY)
    pub id: i64,
    /// Título da música/vídeo (extraído da tag ID3 ou do nome do arquivo)
    pub title: String,
    /// Nome do artista/banda
    pub artist: String,
    /// Nome do álbum
    pub album: String,
    /// Caminho absoluto do arquivo no disco (ex: /dados/musicas/rock/acdc.mp3)
    pub file_path: String,
    /// Tipo de mídia: "mp3", "mp4", "wav", "wmv", "mpeg"
    pub file_type: String,
}
