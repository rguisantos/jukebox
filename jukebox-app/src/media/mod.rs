pub mod catalog_lock;
pub mod covers;
// Perfil legacy (fase 1): player de validação sem mídia — o débito e a
// fila `filamidia` são reais; a reprodução chega com o libVLC (fase 2).
#[cfg(feature = "legacy-pg")]
pub mod legacy_player;
pub mod player;
pub mod scanner;
pub mod storage_management;
pub mod usb_sync;

pub mod online_sync;
