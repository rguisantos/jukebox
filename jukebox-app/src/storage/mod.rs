pub mod service;
// Perfil legacy (fase 1 — Jukebox TV na base antiga): serviço de
// persistência sobre o PostgreSQL `jukeboxtvdb`. Ver JUKEBOXTV-LEGACY.md.
#[cfg(feature = "legacy-pg")]
pub mod legacy_covers;
#[cfg(feature = "legacy-pg")]
pub mod legacy_service;
