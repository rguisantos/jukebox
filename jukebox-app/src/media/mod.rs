pub mod catalog_lock;
pub mod covers;
// Perfil legacy (fase 1): player libVLC (mídia real na base antiga, com o
// modo de validação `legacy_player` como fallback sem a biblioteca) e a
// janela X11 do vídeo embutido.
#[cfg(feature = "legacy-pg")]
pub mod legacy_player;
#[cfg(feature = "legacy-pg")]
pub mod legacy_vlc;
#[cfg(feature = "legacy-pg")]
pub mod legacy_x11;
pub mod player;
pub mod scanner;
pub mod storage_management;
pub mod usb_sync;

pub mod online_sync;
