//! Modelos de dados e máquina de estados da interface (Módulo 7).
//!
//! O catálogo é agrupado em álbuns (`AlbumInfo`), cada um com suas faixas
//! (`TrackInfo`) — estrutura aninhada que alimenta o carrossel de capas e o
//! painel de faixas do Slint. A navegação por controles arcade é regida por
//! `AppState`: uma máquina de estados de foco PURA (nenhum I/O, nenhum canal)
//! — as teclas entram, ações semânticas (`Action`) saem, e o `main.rs`
//! executa os efeitos colaterais (banco, player, USB, UI).

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
    /// Gênero musical da tag ID3 (MÓDULO 8 — filtro do catálogo público).
    /// String vazia no caminho RequestPlay (a UI não precisa do gênero).
    pub genre: String,
}

/// Um disco do catálogo: agrupamento de faixas por (artista, álbum).
/// Base da navegação em duas camadas do Módulo 7 — primeiro escolhe-se o
/// disco no carrossel de capas, depois a faixa dentro dele.
#[derive(Debug, Clone)]
pub struct AlbumInfo {
    /// Chave de agrupamento estável ("artista|álbum") — também indexa as capas
    pub key: String,
    /// Título do álbum
    pub title: String,
    /// Artista/banda
    pub artist: String,
    /// Gênero do disco (tag ID3 ou pasta do acervo) — ordenação do carrossel
    pub genre: String,
    /// Primeira letra do título (maiúscula) — usada no placeholder da capa
    /// quando o arquivo não traz arte embutida
    pub initial: String,
    /// 0..=5: índice da paleta de cores do placeholder (derivado da chave,
    /// estável entre reinicializações para a cor nunca "mudar sozinha")
    pub palette: u32,
    /// true se o disco contiver ao menos uma faixa recém-adicionada (*)
    pub is_recent: bool,
    /// Faixas do disco (já ordenadas por título pela consulta SQL)
    pub tracks: Vec<TrackInfo>,
}

/// Miniatura de capa de álbum, já decodificada e redimensionada para RGB
/// puro — pronta para virar textura do Slint no event loop (o buffer cru
/// atravessa os canais mpsc porque `slint::Image` não é Send).
#[derive(Debug, Clone)]
pub struct CoverArt {
    /// Pixels RGB intercalados (3 bytes por pixel)
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// MÓDULO 8 — Um gênero musical do acervo, como exibido no submenu
/// "Bloquear Gêneros" do operador: nome (tag ID3), contagem de faixas
/// e a situação de bloqueio no catálogo público.
#[derive(Debug, Clone)]
pub struct GenreInfo {
    pub name: String,
    /// Número de faixas do acervo com esse gênero (independente do bloqueio)
    pub track_count: i64,
    /// true = gênero bloqueado (invisível no catálogo público)
    pub blocked: bool,
}

// =============================================================================
// MÓDULO 7 — Máquina de estados de foco (controles arcade)
// -----------------------------------------------------------------------------
// Mapeamento da controladora "Zero Delay" do gabinete (vista pelo sistema
// como teclado USB comum — maiúscula ou minúscula, daí a normalização):
//
//   E = esquerda (capa anterior)      R = direita (próxima capa)
//   I = escolhe a capa (abre faixas)  W = cima (sobe lista/menu / pula linha)
//   Q = baixo (desce lista/menu)      O = seleciona música (1 crédito)
//   U = cancela música                P = barra de volume
//   Z = insere crédito (moedeiro)     X = menu do operador
//   A = zera créditos                 L = encerra o programa
//
// =============================================================================

/// Passo do volume por pressionamento de W/Q dentro do overlay (em %)
pub const VOLUME_STEP: u32 = 5;

/// Volume máximo exibido na barra (100% = playbin volume 1.0)
pub const VOLUME_MAX: u32 = 100;

/// Volume padrão na primeira execução (persistido no banco a partir daí)
pub const VOLUME_DEFAULT: u32 = 70;

// =============================================================================
// MÓDULO 8 — Preço dinâmico da música (créditos por reprodução)
// =============================================================================

/// Preço mínimo da música (1 crédito)
pub const SONG_PRICE_MIN: u32 = 1;

/// Preço máximo da música (10 créditos — limite de segurança do operador)
pub const SONG_PRICE_MAX: u32 = 10;

/// Régua da seleção rápida de letras alfabética e * para recém-adicionados
pub const ALPHABET_ITEMS: &[&str] = &[
    "*", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
    "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "#"
];

// =============================================================================
// MÓDULO 8 — Menu do operador profissional (7 opções + 3 submenus)
// =============================================================================

/// Total de linhas do menu principal do operador
pub const OPERATOR_MENU_ITEMS: usize = 7;

/// Índice da linha "Sincronizar Pendrive" no menu principal
pub const OPERATOR_MENU_SYNC: usize = 0;

/// Índice da linha "Preço da Música" (abre o submenu de preço)
pub const OPERATOR_MENU_PRICE: usize = 1;

/// Índice da linha "Dias Recém Adicionados (*)" (submenu de dias do recém adicionado)
pub const OPERATOR_MENU_RECENT_DAYS: usize = 2;

/// Índice da linha "Bloquear Gêneros" (abre o submenu de gêneros)
pub const OPERATOR_MENU_GENRES: usize = 3;

/// Índice da linha "Zerar Caixa Parcial"
pub const OPERATOR_MENU_RESET_PARTIAL: usize = 4;

/// Índice da linha "Zerar Créditos Atuais"
pub const OPERATOR_MENU_RESET_CREDITS: usize = 5;

/// Índice da linha "Desligar Máquina"
pub const OPERATOR_MENU_POWEROFF: usize = 6;

/// Estado de foco da interface — uma única fonte de verdade, espelhada
/// para a propriedade `ui-focus` do Slint (que decide qual camada desenhar).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusState {
    /// Camada 1 — carrossel de capas (E/R/W/Q navegam, I abre o disco)
    BrowsingAlbums,
    /// Camada 2 — faixas do álbum aberto (W/Q navegam, O toca, I volta)
    BrowsingTracks,
    /// Overlay de volume (P abre/fecha, W/Q/E/R ajustam)
    VolumeControl,
    /// MÓDULO 8 — Menu principal do operador (X abre, W/Q navegam, O entra, U fecha)
    OperatorMainMenu,
    /// MÓDULO 8 — Submenu de preço (W/Q alteram, O/U salvam e voltam)
    OperatorPriceMenu,
    /// MÓDULO 8 — Submenu de gêneros (W/Q navegam, O alterna bloqueio, U volta)
    OperatorGenreMenu,
    /// Régua de seleção rápida alfabética e recém-adicionados (*)
    AlphabetPicker,
    /// Submenu de dias recém-adicionados (W/Q alteram, O/U salvam)
    OperatorRecentDaysMenu,
}

impl FocusState {
    /// Espelha o estado para a propriedade `ui-focus` do Slint (0..=7)
    pub fn as_i32(self) -> i32 {
        match self {
            FocusState::BrowsingAlbums => 0,
            FocusState::BrowsingTracks => 1,
            FocusState::VolumeControl => 2,
            FocusState::OperatorMainMenu => 3,
            FocusState::OperatorPriceMenu => 4,
            FocusState::OperatorGenreMenu => 5,
            FocusState::AlphabetPicker => 6,
            FocusState::OperatorRecentDaysMenu => 7,
        }
    }

    /// true para qualquer tela do operador (menu principal ou submenus) —
    /// usado para fechar o conjunto inteiro quando a sincronização USB
    /// toma a tela e para decidir se o vídeo XVideo deve ficar oculto.
    pub fn is_operator(self) -> bool {
        matches!(
            self,
            FocusState::OperatorMainMenu
                | FocusState::OperatorPriceMenu
                | FocusState::OperatorGenreMenu
                | FocusState::OperatorRecentDaysMenu
        )
    }
}

/// Efeitos que a máquina de estados pede ao resto do sistema.
/// A máquina permanece pura: nenhum canal, banco ou UI é tocado aqui dentro.
#[derive(Debug, Clone)]
pub enum Action {
    /// Tecla reconhecida, porém sem efeito adicional (ex.: E já na 1ª capa)
    Noop,
    /// Moeda/noteiro (tecla Z — aceita em qualquer estado)
    AddCredit,
    /// Tecla O numa faixa: débito do preço vigente + enfileiramento no player
    /// (MÓDULO 8: o valor exato é lido do banco pela thread de persistência)
    PlayTrack(TrackInfo),
    /// W/Q no overlay de volume: aplicar o novo valor no playbin
    VolumeChanged(u32),
    /// U no overlay de volume: aplicar e persistir o volume no banco
    VolumeClosed(u32),
    /// X: abrir o menu do operador (stats + gêneros do banco + sync USB)
    OpenOperatorMenu,
    /// U no menu principal do operador: fechar (restaurar vídeo ocultado)
    CloseOperatorMenu,
    /// O em "Forçar Sincronização USB"
    ForceSync,
    /// MÓDULO 8 — O em "Preço da Música": abrir o submenu de edição
    OpenPriceMenu,
    /// MÓDULO 8 — O/U no submenu de preço: persistir o novo preço e voltar
    PriceSaved(u32),
    /// MÓDULO 8 — O em "Bloquear Gêneros": abrir o submenu de gêneros
    OpenGenreMenu,
    /// MÓDULO 8 — U no submenu de gêneros: voltar ao menu principal
    /// (o vídeo permanece oculto — ainda estamos dentro das telas do operador)
    CloseGenreMenu,
    /// MÓDULO 8 — O num gênero: alternar bloqueio e recarregar o catálogo
    ToggleGenreBlock(String),
    /// MÓDULO 8 — O em "Zerar Caixa Parcial"
    ResetPartialCoins,
    /// MÓDULO 8 — O em "Zerar Créditos Atuais"
    ResetCredits,
    /// MÓDULO 8 — O em "Desligar Máquina" (sudo systemctl poweroff)
    PowerOff,
    /// Tecla U enquanto está tocando: cancela a faixa atual e avança para a próxima
    SkipTrack,
    /// Tecla L: encerra a aplicação
    QuitApp,
    /// Abertura da régua alfabética
    OpenAlphabetPicker,
    /// Movimento na régua alfabética
    LetterMoved(usize),
    /// Confirmação da letra na régua alfabética
    ConfirmLetter(usize),
    /// Abertura do submenu de dias recém-adicionados
    OpenRecentDaysMenu,
    /// Salvamento do valor de dias recém-adicionados
    RecentDaysSaved(u32),
}

/// Estado global de navegação da interface. Vivem dentro de um
/// `Arc<Mutex<AppState>>` compartilhado entre os callbacks da UI (thread do
/// event loop) e as bridges que publicam catálogo/fecham overlays.
pub struct AppState {
    /// Estado de foco atual (decide qual camada da UI está ativa)
    pub focus: FocusState,
    /// Estado anterior à abertura de um overlay (U volta para ele)
    pub previous: FocusState,
    /// Catálogo agrupado por álbum (substituído a cada sincronização)
    pub albums: Vec<AlbumInfo>,
    /// Disco selecionado no carrossel
    pub album_index: usize,
    /// Faixa selecionada no painel do álbum aberto
    pub track_index: usize,
    /// Linha selecionada no menu principal do operador (0..OPERATOR_MENU_ITEMS)
    pub menu_index: usize,
    /// Volume atual (0..=100, persistido no banco ao fechar o overlay)
    pub volume: u32,
    /// MÓDULO 8 — Preço vigente da música (créditos), sincronizado com o banco
    pub song_price: u32,
    /// MÓDULO 8 — Valor em edição no submenu de preço (W/Q alteram, O/U salvam)
    pub price_value: u32,
    /// Dias para considerar um disco como recém-adicionado (*)
    pub recent_days: u32,
    /// Valor em edição no submenu de dias recém-adicionados
    pub recent_days_value: u32,
    /// Índice selecionado na régua de letras alfabética (*, A-Z, #)
    pub letter_index: usize,
    /// MÓDULO 8 — Gêneros do acervo para o submenu "Bloquear Gêneros"
    /// (pré-carregados do banco na abertura do menu do operador)
    pub genres: Vec<GenreInfo>,
    /// MÓDULO 8 — Gênero selecionado no submenu de gêneros
    pub genre_index: usize,
    /// Indica se há uma música sendo reproduzida no momento
    pub is_playing: bool,
}

impl AppState {
    pub fn new(volume: u32, song_price: u32) -> Self {
        Self {
            focus: FocusState::BrowsingAlbums,
            previous: FocusState::BrowsingAlbums,
            albums: Vec::new(),
            album_index: 0,
            track_index: 0,
            menu_index: 0,
            volume: volume.min(VOLUME_MAX),
            song_price: song_price.clamp(SONG_PRICE_MIN, SONG_PRICE_MAX),
            price_value: song_price.clamp(SONG_PRICE_MIN, SONG_PRICE_MAX),
            recent_days: 30,
            recent_days_value: 30,
            letter_index: 0,
            genres: Vec::new(),
            genre_index: 0,
            is_playing: false,
        }
    }

    /// Álbum atualmente selecionado no carrossel (None se catálogo vazio)
    pub fn current_album(&self) -> Option<&AlbumInfo> {
        self.albums.get(self.album_index)
    }

    /// Número de faixas do álbum aberto (0 se não houver álbum)
    pub fn track_count(&self) -> usize {
        self.current_album().map(|a| a.tracks.len()).unwrap_or(0)
    }

    /// Abre o álbum selecionado (tecla I ou clique na capa).
    /// Só é possível quando o disco tem ao menos uma faixa.
    pub fn open_current_album(&mut self) -> bool {
        if self.track_count() > 0 {
            self.focus = FocusState::BrowsingTracks;
            self.track_index = 0;
            true
        } else {
            false
        }
    }

    /// Substitui o catálogo completo (scanner inicial ou pós-sync USB) e
    /// reinicia a navegação: os índices antigos podem não existir mais.
    pub fn replace_catalog(&mut self, albums: Vec<AlbumInfo>) {
        self.albums = albums;
        self.album_index = 0;
        self.track_index = 0;
        self.focus = FocusState::BrowsingAlbums;
        self.previous = FocusState::BrowsingAlbums;
    }

    /// MÓDULO 8 — Substitui o catálogo PRESERVANDO o foco atual: usado após
    /// alternar o bloqueio de um gênero, quando o operador está dentro do
    /// submenu de gêneros e não pode ser expulso para o carrossel.
    pub fn replace_catalog_keep_focus(&mut self, albums: Vec<AlbumInfo>) {
        self.albums = albums;
        self.album_index = 0;
        self.track_index = 0;
    }

    /// Pula a seleção do carrossel para o primeiro álbum correspondente à letra selecionada
    pub fn jump_to_letter(&mut self, letter: &str) {
        if self.albums.is_empty() {
            self.focus = FocusState::BrowsingAlbums;
            return;
        }

        let target_index = match letter {
            "*" => self.albums.iter().position(|a| a.is_recent),
            "#" => self.albums.iter().position(|a| {
                let first = a.artist.chars().next().or_else(|| a.title.chars().next());
                first.map(|c| !c.is_alphabetic()).unwrap_or(false)
            }),
            char_str => {
                let target_char = char_str.chars().next().unwrap().to_ascii_lowercase();
                self.albums.iter().position(|a| {
                    let first = a.artist.chars().next().or_else(|| a.title.chars().next());
                    first.map(|c| c.to_ascii_lowercase() == target_char).unwrap_or(false)
                })
            }
        };

        if let Some(idx) = target_index {
            self.album_index = idx;
        }
        self.focus = FocusState::BrowsingAlbums;
    }

    /// Processa uma tecla crua (case-insensitive: a controladora arcade
    /// gera maiúsculas ou minúsculas conforme o modo do firmware).
    ///
    /// Retorna:
    ///   - `Some(action)` — tecla consumida pelo estado de foco atual;
    ///   - `None`         — tecla irrelevante aqui (o evento é rejeitado
    ///                      e segue para o resto do sistema, se houver).
    pub fn handle_key(&mut self, raw: &str) -> Option<Action> {
        let lowered = raw.trim().to_lowercase();

        // Pressionamento longo das teclas E ou R abre a régua de seleção alfabética
        if lowered.contains("long") || lowered.contains("hold") || lowered == "e_long" || lowered == "r_long" {
            if self.focus == FocusState::BrowsingAlbums {
                self.previous = FocusState::BrowsingAlbums;
                self.focus = FocusState::AlphabetPicker;
                self.letter_index = 0;
                return Some(Action::OpenAlphabetPicker);
            }
        }

        let mut chars = lowered.chars();
        let key = chars.next()?;
        if chars.next().is_some() {
            return None; // textos multi-caractere não são teclas arcade
        }

        // Teclas globais (disponíveis em qualquer estado)
        if key == 'z' {
            return Some(Action::AddCredit);
        }
        if key == 'a' {
            return Some(Action::ResetCredits);
        }
        if key == 'l' {
            return Some(Action::QuitApp);
        }

        match self.focus {
            FocusState::BrowsingAlbums => match key {
                'e' => {
                    if !self.albums.is_empty() {
                        self.album_index = self.album_index.saturating_sub(1);
                    }
                    Some(Action::Noop)
                }
                'r' => {
                    if !self.albums.is_empty() {
                        self.album_index = (self.album_index + 1).min(self.albums.len() - 1);
                    }
                    Some(Action::Noop)
                }
                'w' => {
                    if !self.albums.is_empty() {
                        self.album_index = (self.album_index + 4).min(self.albums.len() - 1);
                    }
                    Some(Action::Noop)
                }
                'q' => {
                    if !self.albums.is_empty() {
                        self.album_index = self.album_index.saturating_sub(4);
                    }
                    Some(Action::Noop)
                }
                'i' => {
                    self.open_current_album();
                    Some(Action::Noop)
                }
                'p' => {
                    self.previous = FocusState::BrowsingAlbums;
                    self.focus = FocusState::VolumeControl;
                    Some(Action::Noop)
                }
                'u' => {
                    if self.is_playing {
                        Some(Action::SkipTrack)
                    } else {
                        None
                    }
                }
                'x' => {
                    self.previous = FocusState::BrowsingAlbums;
                    self.focus = FocusState::OperatorMainMenu;
                    self.menu_index = 0;
                    Some(Action::OpenOperatorMenu)
                }
                _ => None,
            },

            FocusState::AlphabetPicker => match key {
                'e' | 'w' => {
                    self.letter_index = (self.letter_index + 1).min(ALPHABET_ITEMS.len() - 1);
                    Some(Action::LetterMoved(self.letter_index))
                }
                'r' | 'q' => {
                    self.letter_index = self.letter_index.saturating_sub(1);
                    Some(Action::LetterMoved(self.letter_index))
                }
                'i' => {
                    let idx = self.letter_index;
                    if let Some(&letter) = ALPHABET_ITEMS.get(idx) {
                        self.jump_to_letter(letter);
                        Some(Action::ConfirmLetter(idx))
                    } else {
                        self.focus = FocusState::BrowsingAlbums;
                        Some(Action::Noop)
                    }
                }
                'u' | 'x' => {
                    self.focus = FocusState::BrowsingAlbums;
                    Some(Action::Noop)
                }
                _ => None,
            },

            FocusState::BrowsingTracks => match key {
                'w' => {
                    if self.track_count() > 0 {
                        self.track_index = (self.track_index + 1).min(self.track_count() - 1);
                    }
                    Some(Action::Noop)
                }
                'q' => {
                    self.track_index = self.track_index.saturating_sub(1);
                    Some(Action::Noop)
                }
                'i' => {
                    self.focus = FocusState::BrowsingAlbums;
                    Some(Action::Noop)
                }
                'o' => {
                    let track = self
                        .current_album()
                        .and_then(|a| a.tracks.get(self.track_index))
                        .cloned();
                    match track {
                        Some(t) => Some(Action::PlayTrack(t)),
                        None => Some(Action::Noop),
                    }
                }
                'u' => {
                    if self.is_playing {
                        Some(Action::SkipTrack)
                    } else {
                        None
                    }
                }
                'p' => {
                    self.previous = FocusState::BrowsingTracks;
                    self.focus = FocusState::VolumeControl;
                    Some(Action::Noop)
                }
                'x' => {
                    self.previous = FocusState::BrowsingTracks;
                    self.focus = FocusState::OperatorMainMenu;
                    self.menu_index = 0;
                    Some(Action::OpenOperatorMenu)
                }
                _ => None,
            },

            FocusState::VolumeControl => match key {
                'w' | 'r' => {
                    self.volume = (self.volume + VOLUME_STEP).min(VOLUME_MAX);
                    Some(Action::VolumeChanged(self.volume))
                }
                'q' | 'e' => {
                    self.volume = self.volume.saturating_sub(VOLUME_STEP);
                    Some(Action::VolumeChanged(self.volume))
                }
                'p' => {
                    let volume = self.volume;
                    self.focus = self.previous;
                    Some(Action::VolumeClosed(volume))
                }
                'u' => {
                    if self.is_playing {
                        Some(Action::SkipTrack)
                    } else {
                        None
                    }
                }
                _ => None,
            },

            // MÓDULO 8 — Menu principal do operador (7 opções + 3 submenus).
            // W/Q navegam, O entra/confirma, U fecha e volta à tela anterior.
            FocusState::OperatorMainMenu => match key {
                'w' => {
                    self.menu_index = (self.menu_index + 1).min(OPERATOR_MENU_ITEMS - 1);
                    Some(Action::Noop)
                }
                'q' => {
                    self.menu_index = self.menu_index.saturating_sub(1);
                    Some(Action::Noop)
                }
                'o' => match self.menu_index {
                    OPERATOR_MENU_SYNC => {
                        let previous = self.previous;
                        self.focus = previous;
                        Some(Action::ForceSync)
                    }
                    OPERATOR_MENU_PRICE => {
                        self.price_value = self.song_price;
                        self.focus = FocusState::OperatorPriceMenu;
                        Some(Action::OpenPriceMenu)
                    }
                    OPERATOR_MENU_RECENT_DAYS => {
                        self.recent_days_value = self.recent_days;
                        self.focus = FocusState::OperatorRecentDaysMenu;
                        Some(Action::OpenRecentDaysMenu)
                    }
                    OPERATOR_MENU_GENRES => {
                        self.genre_index = 0;
                        self.focus = FocusState::OperatorGenreMenu;
                        Some(Action::OpenGenreMenu)
                    }
                    OPERATOR_MENU_RESET_PARTIAL => Some(Action::ResetPartialCoins),
                    OPERATOR_MENU_RESET_CREDITS => Some(Action::ResetCredits),
                    OPERATOR_MENU_POWEROFF => Some(Action::PowerOff),
                    _ => Some(Action::Noop),
                },
                'u' => {
                    self.focus = self.previous;
                    Some(Action::CloseOperatorMenu)
                }
                _ => None,
            },

            // MÓDULO 8 — Submenu de preço: W/Q alteram o valor em edição,
            // O ou U salvam e voltam ao menu principal.
            FocusState::OperatorPriceMenu => match key {
                'w' => {
                    self.price_value = self.price_value.saturating_sub(1).max(SONG_PRICE_MIN);
                    Some(Action::Noop)
                }
                'q' => {
                    self.price_value = (self.price_value + 1).min(SONG_PRICE_MAX);
                    Some(Action::Noop)
                }
                'o' | 'u' => {
                    let price = self.price_value;
                    self.song_price = price;
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::PriceSaved(price))
                }
                _ => None,
            },

            // Submenu de dias para recém-adicionados (*)
            FocusState::OperatorRecentDaysMenu => match key {
                'w' | 'r' => {
                    self.recent_days_value = self.recent_days_value.saturating_sub(5).max(1);
                    Some(Action::Noop)
                }
                'q' | 'e' => {
                    self.recent_days_value = (self.recent_days_value + 5).min(365);
                    Some(Action::Noop)
                }
                'o' | 'u' => {
                    let days = self.recent_days_value;
                    self.recent_days = days;
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::RecentDaysSaved(days))
                }
                _ => None,
            },

            // MÓDULO 8 — Submenu de gêneros: W/Q navegam na lista, O alterna
            // o bloqueio (feedback local instantâneo + gravação no banco),
            // U volta ao menu principal.
            FocusState::OperatorGenreMenu => match key {
                'w' => {
                    if !self.genres.is_empty() {
                        self.genre_index = (self.genre_index + 1).min(self.genres.len() - 1);
                    }
                    Some(Action::Noop)
                }
                'q' => {
                    if !self.genres.is_empty() {
                        self.genre_index = self.genre_index.saturating_sub(1);
                    }
                    Some(Action::Noop)
                }
                'o' => match self.genres.get_mut(self.genre_index) {
                    Some(genre) => {
                        genre.blocked = !genre.blocked;
                        let name = genre.name.clone();
                        Some(Action::ToggleGenreBlock(name))
                    }
                    None => Some(Action::Noop),
                },
                'u' => {
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::CloseGenreMenu)
                }
                _ => None,
            },
        }
    }
}

/// Hash FNV-1a 64-bit — determina a cor do placeholder da capa e o nome do
/// arquivo de cache de capas (estável entre execuções: a capa nunca "muda
/// de cor" nem é extraída duas vezes do mesmo disco).
pub fn fnv64(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Primeira letra alfanumérica do título (maiúscula) para o placeholder
/// da capa quando o arquivo não traz arte embutida no ID3.
pub fn album_initial(title: &str) -> String {
    title
        .chars()
        .find(|c| c.is_alphanumeric())
        .and_then(|c| c.to_uppercase().next())
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_toggle_with_p() {
        let mut state = AppState::new(50, 1);
        assert_eq!(state.focus, FocusState::BrowsingAlbums);

        // Apertar 'p' abre o volume
        state.handle_key("p");
        assert_eq!(state.focus, FocusState::VolumeControl);

        // Apertar 'p' novamente fecha o volume
        state.handle_key("p");
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
    }

    #[test]
    fn test_volume_up_down_keys() {
        let mut state = AppState::new(50, 1);
        state.handle_key("p"); // abre volume
        assert_eq!(state.volume, 50);

        // 'w' ou 'r' aumenta volume
        state.handle_key("w");
        assert_eq!(state.volume, 55);
        state.handle_key("r");
        assert_eq!(state.volume, 60);

        // 'q' ou 'e' diminui volume
        state.handle_key("q");
        assert_eq!(state.volume, 55);
        state.handle_key("e");
        assert_eq!(state.volume, 50);
    }

    #[test]
    fn test_album_open_close_with_i() {
        let mut state = AppState::new(50, 1);
        state.albums = vec![AlbumInfo {
            title: "Album Test".to_string(),
            artist: "Artist Test".to_string(),
            genre: "Rock".to_string(),
            key: "key".to_string(),
            initial: "A".to_string(),
            palette: 0,
            is_recent: false,
            tracks: vec![TrackInfo {
                id: 1,
                title: "Track 1".to_string(),
                artist: "Artist Test".to_string(),
                album: "Album Test".to_string(),
                genre: "Rock".to_string(),
                file_path: "/test.mp3".to_string(),
                file_type: "mp3".to_string(),
            }],
        }];
        assert_eq!(state.focus, FocusState::BrowsingAlbums);

        // 'i' abre as faixas do álbum
        state.handle_key("i");
        assert_eq!(state.focus, FocusState::BrowsingTracks);

        // 'i' fecha o álbum e volta ao carrossel
        state.handle_key("i");
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
    }

    #[test]
    fn test_global_keys_a_and_l() {
        let mut state = AppState::new(50, 1);
        
        // 'a' zera os créditos
        let action_a = state.handle_key("a");
        assert!(matches!(action_a, Some(Action::ResetCredits)));

        // 'l' encerra o programa
        let action_l = state.handle_key("l");
        assert!(matches!(action_l, Some(Action::QuitApp)));
    }

    #[test]
    fn test_skip_track_with_u() {
        let mut state = AppState::new(50, 1);
        state.is_playing = true;

        // Quando está tocando, 'u' cancela a faixa
        let action = state.handle_key("u");
        assert!(matches!(action, Some(Action::SkipTrack)));

        state.is_playing = false;
        // Quando não está tocando, 'u' não tem efeito
        let action_not_playing = state.handle_key("u");
        assert!(action_not_playing.is_none());
    }
}


