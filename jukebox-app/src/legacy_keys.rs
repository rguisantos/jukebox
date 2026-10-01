//! Tradução das teclas do Jukebox TV original para as teclas canônicas do app.
//!
//! O sistema Java guardava os códigos das teclas da interface de botões
//! (moedeiro + navegação) nas colunas `sistema.codtecla*`, usando códigos
//! **AWT** (`java.awt.event.KeyEvent`: VK_A=65…VK_Z=90, VK_LEFT=37…).
//! A interface é um teclado USB comum: o kernel entrega os eventos pelo X11
//! e o app os recebe em `arcade-key-pressed`. O hardlock embutido na placa
//! servia apenas à verificação de licença do Java — o Rust não precisa dele.
//!
//! Esta camada permite que cada máquina em campo continue com a sua
//! configuração de teclas, sem reprogramar a placa: no início da fase 1 o
//! app lê a linha `sistema` e constrói o mapa AWT → tecla canônica.
//!
//! Semântica original → tecla canônica do app:
//! - `codteclaesquerda/direita/cima/baixo` → e/r/q/w (navegação; W/Q
//!   saltam uma linha inteira da grade 5×2)
//! - `codtecladisco` / `codteclamusica` → i/o (abre álbum / confirma faixa)
//! - `codteclacancela` → u (cancela faixa/volta um nível)
//! - `codteclacredito` → z (pulso do moedeiro; **crédito em qualquer tela**)
//! - `codteclavolume` → p (abre barra de volume)
//! - `codteclaresetacreditos` → a (zeroing do saldo; 0 = desativada)
//! - `codteclafecharprograma` → l (encerra)
//! - `codteclasair` → sem equivalente direto (voltar à tela inicial):
//!   tratado como "u" quando um nível está aberto
//!
//! `codteclamaisvolume`/`codteclamenosvolume` ainda não têm tradução direta:
//! o overlay de volume atual ajusta com W/Q. Pendência registrada em
//! `JUKEBOXTV-LEGACY.md` (estender `FocusState::VolumeControl` para aceitar
//! os códigos dedicados de +/−).

/// Códigos AWT (`java.awt.event.KeyEvent`) usados pelas colunas `codtecla*`.
pub mod awt {
    pub const VK_LEFT: i32 = 37;
    pub const VK_UP: i32 = 38;
    pub const VK_RIGHT: i32 = 39;
    pub const VK_DOWN: i32 = 40;
    pub const VK_ENTER: i32 = 10;
    pub const VK_ESCAPE: i32 = 27;
    pub const VK_A: i32 = 65;
    pub const VK_E: i32 = 69;
    pub const VK_I: i32 = 73;
    pub const VK_L: i32 = 76;
    pub const VK_O: i32 = 79;
    pub const VK_P: i32 = 80;
    pub const VK_Q: i32 = 81;
    pub const VK_R: i32 = 82;
    pub const VK_U: i32 = 85;
    pub const VK_W: i32 = 87;
    pub const VK_Z: i32 = 90;
}

/// Mapa de teclas de uma máquina, lido da linha `sistema` do `jukeboxtvdb`.
/// Campos com `0` estão desativados (o original usava 0 para "sem tecla").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyKeys {
    pub esquerda: i32,
    pub direita: i32,
    pub cima: i32,
    pub baixo: i32,
    pub disco: i32,
    pub musica: i32,
    pub credito: i32,
    pub volume: i32,
    pub cancela: i32,
    pub sair: i32,
    pub resetacreditos: i32,
    pub fecharprograma: i32,
}

impl LegacyKeys {
    /// Layout típico relatado em campo: Z para crédito e letras da linha
    /// superior para navegação. Usado como fallback quando a leitura da
    /// linha `sistema` não está disponível (ex.: banco inacessível).
    pub fn typical() -> Self {
        Self {
            esquerda: awt::VK_A,
            direita: awt::VK_R,
            cima: awt::VK_W,
            baixo: awt::VK_Q,
            disco: awt::VK_I,
            musica: awt::VK_O,
            credito: awt::VK_Z,
            volume: awt::VK_P,
            cancela: awt::VK_U,
            sair: awt::VK_ESCAPE,
            resetacreditos: 0,
            fecharprograma: 0,
        }
    }

    /// Converte um código AWT na tecla canônica do app, segundo a
    /// configuração desta máquina. Retorna `None` para teclas não
    /// configuradas (0) ou sem papel no app.
    pub fn translate(&self, code: i32) -> Option<&'static str> {
        if code == 0 {
            return None;
        }
        Some(match code {
            c if c == self.esquerda => "e",
            c if c == self.direita => "r",
            c if c == self.baixo => "w",
            c if c == self.cima => "q",
            c if c == self.disco => "i",
            c if c == self.musica => "o",
            c if c == self.credito => "z",
            c if c == self.volume => "p",
            c if c == self.cancela => "u",
            // `sair` volta à tela inicial: cancela o nível aberto.
            c if c == self.sair => "u",
            c if c == self.resetacreditos => "a",
            c if c == self.fecharprograma => "l",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_layout_translates_every_semantic_button() {
        let keys = LegacyKeys::typical();
        assert_eq!(keys.translate(awt::VK_A), Some("e"));
        assert_eq!(keys.translate(awt::VK_R), Some("r"));
        assert_eq!(keys.translate(awt::VK_W), Some("q"));
        assert_eq!(keys.translate(awt::VK_Q), Some("w"));
        assert_eq!(keys.translate(awt::VK_I), Some("i"));
        assert_eq!(keys.translate(awt::VK_O), Some("o"));
        assert_eq!(keys.translate(awt::VK_Z), Some("z"));
        assert_eq!(keys.translate(awt::VK_P), Some("p"));
        assert_eq!(keys.translate(awt::VK_U), Some("u"));
        assert_eq!(keys.translate(awt::VK_ESCAPE), Some("u"));
        // Desativadas por padrão (0), como no original.
        assert_eq!(keys.translate(awt::VK_L), None);
    }

    #[test]
    fn credit_key_is_machine_configurable() {
        // Uma máquina com crédito no ENTER em vez de Z continua funcionando:
        // basta a coluna codteclacredito apontar para outro código AWT.
        let mut keys = LegacyKeys::typical();
        keys.credito = awt::VK_ENTER;
        assert_eq!(keys.translate(awt::VK_ENTER), Some("z"));
        assert_eq!(keys.translate(awt::VK_Z), None);
    }

    #[test]
    fn disabled_keys_and_unknown_codes_are_ignored() {
        let keys = LegacyKeys::typical();
        assert_eq!(keys.translate(0), None);
        assert_eq!(keys.translate(112), None); // F1 sem papel configurado
    }
}
