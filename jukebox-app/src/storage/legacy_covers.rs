//! Fonte de capas do perfil legacy: `disco.capa` (bytea) no PostgreSQL.
//!
//! O pipeline de capas do app (`media::covers`) procura no cache JBC e
//! depois em arquivos (capa de pasta / APIC do MP3). No perfil legacy a
//! capa canônica vive no banco — o importador Java gravava o JPEG na
//! coluna `capa` de `disco`. Este módulo atende o *miss* do cache com uma
//! conexão dedicada, sem alterar o desenho da thread de capas.
//!
//! As capas legacy **não são cacheadas em disco**: o catálogo de campo tem
//! ~3,5 mil discos e o cache JBC (RGB cru 256×256) consumiria ~700 MiB do
//! disco IDE da base antiga. Decodificar sob demanda (local socket + JPEG
//! 256px) custa dezenas de milissegundos por capa visível — imperceptível
//! na navegação.

use crate::db::legacy_pg::client::LegacyDb;
use crate::db::legacy_pg::PgConfig;
use crate::state::models::CoverArt;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

/// Lado máximo de uma capa decodificada (o mesmo do pipeline do app).
const COVER_MAX: u32 = 256;
/// Timeout do pedido: a thread de capas nunca fica presa num banco lento.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

type CoverRequest = (i32, mpsc::Sender<Result<CoverArt, String>>);

static SOURCE: OnceLock<mpsc::Sender<CoverRequest>> = OnceLock::new();

/// Conecta ao banco e sobe a thread servidora de capas. Falha aqui não
/// derruba o app: o pipeline cai para os arquivos da árvore (capa de
/// pasta/APIC), como no perfil modern.
pub fn spawn(config: &PgConfig) -> Result<(), String> {
    let mut db = LegacyDb::connect(config)?;
    let (tx, rx) = mpsc::channel::<CoverRequest>();
    SOURCE
        .set(tx)
        .map_err(|_| "fonte de capas já iniciada".to_string())?;
    thread::Builder::new()
        .name("legacy-covers".into())
        .spawn(move || {
            while let Ok((disco_id, reply)) = rx.recv() {
                let art = db
                    .fetch_cover(disco_id)
                    .and_then(|bytes| {
                        bytes.ok_or_else(|| format!("disco {disco_id} sem capa no banco"))
                    })
                    .and_then(|bytes| {
                        decode_cover(&bytes)
                            .ok_or_else(|| format!("capa ilegível: disco {disco_id}"))
                    });
                let _ = reply.send(art);
            }
        })
        .map_err(|e| format!("thread de capas legacy: {e}"))?;
    Ok(())
}

/// Pede a capa de um álbum pela chave do catálogo legacy (`"pg:{id}"`).
/// `None` = fonte não disponível ou sem capa — o pipeline segue para os
/// arquivos. Chamado pela thread de capas (blocking com timeout).
pub fn fetch(key: &str) -> Option<CoverArt> {
    let id = key.strip_prefix("pg:")?.parse::<i32>().ok()?;
    let tx = SOURCE.get()?;
    let (reply, rx) = mpsc::channel();
    tx.send((id, reply)).ok()?;
    rx.recv_timeout(FETCH_TIMEOUT).ok()?.ok()
}

/// Decodifica o bytea (JPEG/PNG) e reduz para o máximo 256×256 em RGB
/// intercalado — o mesmo formato que o cache JBC produz.
fn decode_cover(bytes: &[u8]) -> Option<CoverArt> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    let (out_w, out_h, pixels) = if width.max(height) > COVER_MAX {
        let scale = f64::from(COVER_MAX) / f64::from(width.max(height));
        let w = ((f64::from(width) * scale).round() as u32).max(1);
        let h = ((f64::from(height) * scale).round() as u32).max(1);
        (
            w,
            h,
            image::imageops::resize(&rgba, w, h, image::imageops::FilterType::Nearest),
        )
    } else {
        (width, height, rgba)
    };
    let mut rgb = Vec::with_capacity(out_w as usize * out_h as usize * 3);
    for pixel in pixels.pixels() {
        let alpha = u16::from(pixel[3]);
        // Composição sobre fundo escuro da UI (mesma regra do QR Pix):
        // itera o array interno (Rgba.0) — o tipo não indexa por range.
        for color in &pixel.0[..3] {
            rgb.push(((u16::from(*color) * alpha + 12 * (255 - alpha)) / 255) as u8);
        }
    }
    Some(CoverArt {
        rgb,
        width: out_w,
        height: out_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_resizes_and_flattens_alpha() {
        // Gera um PNG 512×512 vermelho — deve sair 256×256 RGB.
        let rgba = image::RgbaImage::from_pixel(512, 512, image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let art = decode_cover(png.get_ref()).expect("capa válida");
        assert_eq!((art.width, art.height), (256, 256));
        assert_eq!(art.rgb.len(), 256 * 256 * 3);
        assert!(art
            .rgb
            .chunks_exact(3)
            .all(|px| px[0] > 200 && px[1] == 0 && px[2] == 0));
    }

    #[test]
    fn decode_rejects_garbage_and_empty() {
        assert!(decode_cover(b"nao eh imagem").is_none());
        assert!(decode_cover(&[]).is_none());
    }

    #[test]
    fn fetch_requires_a_running_source_and_pg_key() {
        // Sem spawn não há fonte: None silencioso (o pipeline segue para
        // os arquivos). Chaves do perfil modern nunca são pedidas ao banco.
        assert!(fetch("artista|album").is_none());
        assert!(fetch("pg:nao-numero").is_none());
        assert!(fetch("pg:").is_none());
    }
}
