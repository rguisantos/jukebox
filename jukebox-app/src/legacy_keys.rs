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
/// `Default` = tudo 0 (todas desativadas) — coerente com a leitura
/// tolerante a nulo da linha `sistema`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

    /// Mapa de remapeamento em tempo de execução desta máquina: texto
    /// bruto entregue pelo Slint → tecla canônica do app. Instalado uma
    /// vez no boot do perfil legacy (`OnceLock` em `main.rs`); vazio nos
    /// demais perfis (remap = identidade).
    ///
    /// Ordem importa na colisão de textos: navegação/seleção primeiro,
    /// crédito por último — diante de duas funções reclamando o mesmo
    /// texto, vence a que não movimenta dinheiro (nunca há crédito
    /// fantasma).
    pub fn runtime_keymap(&self) -> RuntimeKeyMap {
        let mut pairs: Vec<(&'static str, &'static str)> = Vec::new();
        for (code, canonical) in [
            (self.esquerda, "e"),
            (self.direita, "r"),
            (self.cima, "q"),
            (self.baixo, "w"),
            (self.disco, "i"),
            (self.musica, "o"),
            (self.volume, "p"),
            (self.cancela, "u"),
            // `sair` e `cancela` compartilham o canônico "u".
            (self.sair, "u"),
            (self.resetacreditos, "a"),
            (self.fecharprograma, "l"),
            // Crédito por último: colisão com navegação nunca gera crédito.
            (self.credito, "z"),
        ] {
            for raw in slint_texts(code) {
                pairs.push((raw, canonical));
            }
        }
        RuntimeKeyMap { pairs }
    }
}

/// Mapa tecla física → tecla canônica aplicado no callback de teclado.
///
/// O FocusScope do Slint canoniza setas/Esc (←/→/↑/↓ → e/r/q/w, Esc → u)
/// antes de chamar `arcade-key-pressed`. No perfil legacy, a propriedade
/// `pass-through-enter` faz o Enter chegar como "enter" literal — sem
/// colidir com as teclas I/O físicas que o Slint entregaria no lugar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeKeyMap {
    pairs: Vec<(&'static str, &'static str)>,
}

impl RuntimeKeyMap {
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Aplica o mapa a um texto de tecla bruto. Preserva o sufixo
    /// `_long` (pressão longa, usada pela régua alfabética); teclas fora
    /// do mapa passam inalteradas (identidade — teclado de serviço
    /// continua navegando). Sem `trim`: o espaço é uma tecla válida.
    pub fn remap(&self, raw: &str) -> String {
        let (base, suffix) = match raw.strip_suffix("_long") {
            Some(base) => (base, "_long"),
            None => (raw, ""),
        };
        let base = base.to_lowercase();
        if let Some((_, canonical)) = self.pairs.iter().find(|(raw, _)| *raw == base) {
            return format!("{canonical}{suffix}");
        }
        raw.to_string()
    }
}

/// Letras minúsculas para os códigos AWT VK_A..VK_Z.
const LETTERS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

/// Textos que o app recebe (pós-canonização do Slint) quando a máquina
/// pressiona a tecla física de um código AWT. No perfil legacy o Enter
/// chega como "enter" literal (propriedade `pass-through-enter` do
/// FocusScope — evita colidir com as teclas físicas I/O). Vetor vazio =
/// código sem texto correspondente (F-keys etc.).
fn slint_texts(awt: i32) -> Vec<&'static str> {
    match awt {
        awt::VK_LEFT => vec!["e"],
        awt::VK_UP => vec!["q"],
        awt::VK_RIGHT => vec!["r"],
        awt::VK_DOWN => vec!["w"],
        awt::VK_ESCAPE => vec!["u"],
        awt::VK_ENTER => vec!["enter"],
        32 => vec![" "], // VK_SPACE — event.text do espaço
        65..=90 => vec![LETTERS[(awt - 65) as usize]],
        _ => Vec::new(),
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

    #[test]
    fn typical_machine_runtime_keymap_translates_physical_buttons() {
        let map = LegacyKeys::typical().runtime_keymap();
        // Botões físicos: A=esquerda, R=direita, W=cima, Q=baixo, Z=crédito.
        assert_eq!(map.remap("a"), "e");
        assert_eq!(map.remap("r"), "r");
        assert_eq!(map.remap("w"), "q");
        assert_eq!(map.remap("q"), "w");
        assert_eq!(map.remap("z"), "z");
        assert_eq!(map.remap("i"), "i");
        assert_eq!(map.remap("o"), "o");
        assert_eq!(map.remap("p"), "p");
        // Máiusculas chegam pelo mesmo caminho (case-insensitive).
        assert_eq!(map.remap("A"), "e");
        // Esc chega canonizado como "u" e cancela/sair também mapeiam "u".
        assert_eq!(map.remap("u"), "u");
    }

    #[test]
    fn machine_with_credit_on_enter_keeps_letter_keys_intact() {
        let mut keys = LegacyKeys::typical();
        keys.credito = awt::VK_ENTER;
        let map = keys.runtime_keymap();
        // Com `pass-through-enter`, o Enter chega como "enter" literal —
        // sem colidir com o I físico do botão de disco.
        assert_eq!(map.remap("enter"), "z");
        assert_eq!(map.remap("i"), "i");
        assert_eq!(map.remap("o"), "o");
        // Z deixa de ser crédito nesta máquina (vira tecla solta).
        assert_eq!(map.remap("z"), "z");
    }

    #[test]
    fn navigation_wins_text_collisions_over_credit() {
        // Configuração degenerada: a MESMA tecla física (Z) reivindicada
        // por disco e por crédito — vence a navegação; nunca há crédito
        // fantasma por colisão de textos.
        let mut keys = LegacyKeys::typical();
        keys.disco = awt::VK_Z;
        let map = keys.runtime_keymap();
        assert_eq!(map.remap("z"), "i");
        // O crédito desta máquina continua no Z do AWT, mas perde a
        // disputa do texto "z" para o botão de disco.
        assert_eq!(map.remap("enter"), "enter");
    }

    #[test]
    fn long_press_suffix_survives_the_remap() {
        let map = LegacyKeys::typical().runtime_keymap();
        // Pressão longa do botão físico A (esquerda) abre a régua alfabética
        // como se fosse o canônico "e" segurado.
        assert_eq!(map.remap("a_long"), "e_long");
        assert_eq!(map.remap("r_long"), "r_long");
    }

    #[test]
    fn unlisted_keys_pass_through_unchanged() {
        let map = LegacyKeys::typical().runtime_keymap();
        // Teclado de serviço: teclas sem papel na placa não são traduzidas.
        assert_eq!(map.remap("x"), "x");
        assert_eq!(map.remap("F1"), "F1");
        // Mapa vazio (perfil modern) = identidade total.
        let empty = RuntimeKeyMap::default();
        assert!(empty.is_empty());
        assert_eq!(empty.remap("z"), "z");
        assert_eq!(empty.remap("e_long"), "e_long");
    }

    #[test]
    fn space_bar_can_be_a_machine_button() {
        let mut keys = LegacyKeys::typical();
        keys.credito = 32; // VK_SPACE
        let map = keys.runtime_keymap();
        assert_eq!(map.remap(" "), "z");
    }
}
