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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Gênero musical da tag ID3, preservado inclusive no comando de reprodução.
    pub genre: String,
}

impl TrackInfo {
    pub fn is_video(&self) -> bool {
        matches!(self.file_type.as_str(), "mp4" | "wmv" | "mpeg")
    }
}

/// Um disco do catálogo: uma pasta de mídia ou, na raiz, um álbum ID3.
/// Base da navegação em duas camadas do Módulo 7 — primeiro escolhe-se o
/// disco no carrossel de capas, depois a faixa dentro dele.
#[derive(Debug, Clone)]
pub struct AlbumInfo {
    /// Chave de agrupamento estável (caminho da pasta) — também indexa as capas
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
    "*", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
    "S", "T", "U", "V", "W", "X", "Y", "Z", "#",
];

// =============================================================================
// MÓDULO 8 — Menu do operador profissional (7 opções + 3 submenus)
// =============================================================================

/// Total de linhas do menu principal do operador
pub const OPERATOR_MENU_ITEMS: usize = 11;
pub const OPERATOR_MENU_BACK: usize = 10;
pub const SETTINGS_ITEMS: usize = 12;

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
    OperatorSettings,
    /// Camada 1 — carrossel de capas (E/R/W/Q navegam, I abre o disco)
    BrowsingAlbums,
    /// Camada 2 — faixas do álbum aberto (W/Q navegam, O toca, I volta)
    BrowsingTracks,
    /// Overlay de volume (P abre/fecha, W/Q/E/R ajustam)
    VolumeControl,
    /// Menu principal do operador (X abre, Q/W/E/R navegam, O confirma)
    OperatorMainMenu,
    /// Submenu de preço (W/Q alteram, E/R escolhem Salvar/Voltar)
    OperatorPriceMenu,
    /// Submenu de gêneros (direções navegam, O alterna ou volta)
    OperatorGenreMenu,
    /// Régua de seleção rápida alfabética e recém-adicionados (*)
    AlphabetPicker,
    GenrePicker,
    /// Submenu de dias recém-adicionados (W/Q alteram, O/U salvam)
    OperatorRecentDaysMenu,
}

impl FocusState {
    /// Espelha o estado para a propriedade `ui-focus` do Slint (0..=7)
    pub fn as_i32(self) -> i32 {
        match self {
            FocusState::OperatorSettings => 9,
            FocusState::BrowsingAlbums => 0,
            FocusState::BrowsingTracks => 1,
            FocusState::VolumeControl => 2,
            FocusState::OperatorMainMenu => 3,
            FocusState::OperatorPriceMenu => 4,
            FocusState::OperatorGenreMenu => 5,
            FocusState::AlphabetPicker => 6,
            FocusState::GenrePicker => 8,
            FocusState::OperatorRecentDaysMenu => 7,
        }
    }

    /// true para qualquer tela do operador (menu principal ou submenus) —
    /// usado para fechar o conjunto inteiro quando a sincronização USB
    /// toma a tela e para decidir se o vídeo XVideo deve ficar oculto.
    pub fn is_operator(self) -> bool {
        matches!(
            self,
            FocusState::OperatorSettings
                | FocusState::OperatorMainMenu
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
    OpenGenrePicker,
    SelectGenre,
    OpenSettings,
    SaveSettings,
    SettingsAdjusted { index: usize, direction: i32 },
    OpenWifi,
    SyncOnline,
    /// Tecla reconhecida, porém sem efeito adicional (ex.: E já na 1ª capa)
    Noop,
    /// Moeda/noteiro (tecla Z — aceita em qualquer estado)
    AddCredit,
    /// Tecla O numa faixa: débito do preço vigente + enfileiramento no player
    /// (MÓDULO 8: o valor exato é lido do banco pela thread de persistência)
    PlayTrack(TrackInfo),
    /// W/Q no overlay de volume: aplicar o novo valor no playbin
    VolumeChanged(u32),
    /// P ou timeout no overlay de volume: aplicar e persistir o volume no banco
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
    /// Estado anterior à abertura de um overlay
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
    /// Valor em edição no submenu de preço (W/Q alteram)
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
    pub settings_index: usize,
    pub submenu_action: usize,
    pub active_genre: String,
    pub available_genres: Vec<String>,
    pub selected_genre: usize,
    pub last_navigation: std::time::Instant,
    pub last_overlay_interaction: std::time::Instant,
    pub fullscreen: bool,
}

impl AppState {
    /// Os overlays públicos expiram após cinco segundos. O menu do operador
    /// nunca expira; sua saída depende da opção Voltar.
    pub fn expire_idle_overlay(&mut self, now: std::time::Instant) -> Option<Action> {
        if now.saturating_duration_since(self.last_overlay_interaction) < std::time::Duration::from_secs(5) {
            return None;
        }
        match self.focus {
            FocusState::VolumeControl => {
                self.focus = self.previous;
                Some(Action::VolumeClosed(self.volume))
            }
            FocusState::BrowsingTracks => {
                self.focus = FocusState::BrowsingAlbums;
                Some(Action::Noop)
            }
            _ => None,
        }
    }

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
            settings_index: 0,
            submenu_action: 0,
            active_genre: String::new(),
            available_genres: vec![String::new()],
            selected_genre: 0,
            last_navigation: std::time::Instant::now(),
            last_overlay_interaction: std::time::Instant::now(),
            fullscreen: false,
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
        let key = self.current_album().map(|a| a.key.clone());
        let track = self
            .current_album()
            .and_then(|a| a.tracks.get(self.track_index))
            .map(|t| t.file_path.clone());
        self.albums = albums;
        self.album_index = key
            .and_then(|key| self.albums.iter().position(|a| a.key == key))
            .unwrap_or(0);
        self.track_index = track
            .and_then(|path| {
                self.current_album()
                    .and_then(|a| a.tracks.iter().position(|t| t.file_path == path))
            })
            .unwrap_or(0);
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
                    first
                        .map(|c| c.to_ascii_lowercase() == target_char)
                        .unwrap_or(false)
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
        if lowered.contains("long")
            || lowered.contains("hold")
            || lowered == "e_long"
            || lowered == "r_long"
        {
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
            FocusState::OperatorSettings => match key {
                'w' => { self.settings_index = (self.settings_index + 1).min(SETTINGS_ITEMS - 1); Some(Action::Noop) }
                'q' => { self.settings_index = self.settings_index.saturating_sub(1); Some(Action::Noop) }
                'e' | 'r' if self.settings_index < SETTINGS_ITEMS - 2 => Some(Action::SettingsAdjusted {
                    index: self.settings_index, direction: if key == 'r' { 1 } else { -1 },
                }),
                'o' => match self.settings_index {
                    9 => Some(Action::SettingsAdjusted { index: 9, direction: 1 }),
                    10 => Some(Action::SaveSettings),
                    11 => { self.focus = FocusState::OperatorMainMenu; Some(Action::Noop) }
                    _ => Some(Action::Noop),
                },
                _ => None,
            },
            FocusState::BrowsingAlbums => match key {
                'o' => {
                    self.focus = FocusState::GenrePicker;
                    self.selected_genre = self.available_genres.iter()
                        .position(|genre| genre == &self.active_genre).unwrap_or(0);
                    Some(Action::OpenGenrePicker)
                },
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
                    self.last_overlay_interaction = std::time::Instant::now();
                    Some(Action::Noop)
                }
                'p' => {
                    self.previous = FocusState::BrowsingAlbums;
                    self.focus = FocusState::VolumeControl;
                    self.last_overlay_interaction = std::time::Instant::now();
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

            FocusState::GenrePicker => match key {
                'w' | 'r' => {
                    self.selected_genre = (self.selected_genre + 1).min(self.available_genres.len());
                    Some(Action::Noop)
                }
                'q' | 'e' => {
                    self.selected_genre = self.selected_genre.saturating_sub(1);
                    Some(Action::Noop)
                }
                'o' => {
                    self.focus = FocusState::BrowsingAlbums;
                    if let Some(genre) = self.available_genres.get(self.selected_genre).cloned() {
                        self.active_genre = genre;
                        Some(Action::SelectGenre)
                    } else { Some(Action::Noop) }
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
                'w' | 'r' => {
                    if self.track_count() > 0 {
                        self.track_index = (self.track_index + 1).min(self.track_count() - 1);
                    }
                    self.last_overlay_interaction = std::time::Instant::now();
                    Some(Action::Noop)
                }
                'q' | 'e' => {
                    self.track_index = self.track_index.saturating_sub(1);
                    self.last_overlay_interaction = std::time::Instant::now();
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
                    self.last_overlay_interaction = std::time::Instant::now();
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
                    self.last_overlay_interaction = std::time::Instant::now();
                    Some(Action::VolumeChanged(self.volume))
                }
                'q' | 'e' => {
                    self.volume = self.volume.saturating_sub(VOLUME_STEP);
                    self.last_overlay_interaction = std::time::Instant::now();
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

            // Menu principal do operador: quatro direções navegam, O confirma.
            FocusState::OperatorMainMenu => match key {
                'w' | 'r' => {
                    self.menu_index = (self.menu_index + 1).min(OPERATOR_MENU_ITEMS - 1);
                    Some(Action::Noop)
                }
                'q' | 'e' => {
                    self.menu_index = self.menu_index.saturating_sub(1);
                    Some(Action::Noop)
                }
                'o' => match self.menu_index {
                    OPERATOR_MENU_SYNC => Some(Action::ForceSync),
                    OPERATOR_MENU_PRICE => {
                        self.price_value = self.song_price;
                        self.submenu_action = 0;
                        self.focus = FocusState::OperatorPriceMenu;
                        Some(Action::OpenPriceMenu)
                    }
                    OPERATOR_MENU_RECENT_DAYS => {
                        self.recent_days_value = self.recent_days;
                        self.submenu_action = 0;
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
                    7 => Some(Action::OpenWifi),
                    8 => Some(Action::SyncOnline),
                    9 => {
                        self.settings_index = 0;
                        self.focus = FocusState::OperatorSettings;
                        Some(Action::OpenSettings)
                    }
                    OPERATOR_MENU_BACK => {
                        self.focus = self.previous;
                        Some(Action::CloseOperatorMenu)
                    }
                    _ => Some(Action::Noop),
                },
                _ => None,
            },

            // Preço: W/Q alteram o valor, E/R escolhem Salvar ou Voltar.
            FocusState::OperatorPriceMenu => match key {
                'e' => { self.submenu_action = 0; Some(Action::Noop) }
                'r' => { self.submenu_action = 1; Some(Action::Noop) }
                'w' => {
                    self.price_value = self.price_value.saturating_sub(1).max(SONG_PRICE_MIN);
                    Some(Action::Noop)
                }
                'q' => {
                    self.price_value = (self.price_value + 1).min(SONG_PRICE_MAX);
                    Some(Action::Noop)
                }
                'o' => {
                    if self.submenu_action == 1 {
                        self.focus = FocusState::OperatorMainMenu;
                        return Some(Action::Noop);
                    }
                    let price = self.price_value;
                    self.song_price = price;
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::PriceSaved(price))
                }
                _ => None,
            },

            // Submenu de dias para recém-adicionados (*)
            FocusState::OperatorRecentDaysMenu => match key {
                'e' => { self.submenu_action = 0; Some(Action::Noop) }
                'r' => { self.submenu_action = 1; Some(Action::Noop) }
                'w' => {
                    self.recent_days_value = self.recent_days_value.saturating_sub(5).max(1);
                    Some(Action::Noop)
                }
                'q' => {
                    self.recent_days_value = (self.recent_days_value + 5).min(365);
                    Some(Action::Noop)
                }
                'o' => {
                    if self.submenu_action == 1 {
                        self.focus = FocusState::OperatorMainMenu;
                        return Some(Action::Noop);
                    }
                    let days = self.recent_days_value;
                    self.recent_days = days;
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::RecentDaysSaved(days))
                }
                _ => None,
            },

            // Gêneros: quatro direções navegam; O alterna ou seleciona Voltar.
            FocusState::OperatorGenreMenu => match key {
                'w' | 'r' => {
                    self.genre_index = (self.genre_index + 1).min(self.genres.len());
                    Some(Action::Noop)
                }
                'q' | 'e' => {
                    if !self.genres.is_empty() {
                        self.genre_index = self.genre_index.saturating_sub(1);
                    }
                    Some(Action::Noop)
                }
                'o' if self.genre_index == self.genres.len() => {
                    self.focus = FocusState::OperatorMainMenu;
                    Some(Action::CloseGenreMenu)
                }
                'o' => match self.genres.get_mut(self.genre_index) {
                    Some(genre) => {
                        genre.blocked = !genre.blocked;
                        let name = genre.name.clone();
                        Some(Action::ToggleGenreBlock(name))
                    }
                    None => Some(Action::Noop),
                },
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
    fn genre_picker_selects_and_goes_back_with_o() {
        let mut state = AppState::new(50, 1);
        state.available_genres = vec![String::new(), "Pop".into(), "Rock".into()];
        assert!(matches!(state.handle_key("o"), Some(Action::OpenGenrePicker)));
        assert_eq!(state.focus, FocusState::GenrePicker);
        state.handle_key("w");
        assert!(matches!(state.handle_key("o"), Some(Action::SelectGenre)));
        assert_eq!(state.active_genre, "Pop");
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
        state.handle_key("o");
        state.handle_key("w");
        state.handle_key("w");
        assert!(matches!(state.handle_key("o"), Some(Action::Noop)));
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
        assert_eq!(state.active_genre, "Pop");
    }

    #[test]
    fn alphabet_picker_jumps_past_first_carousel_page() {
        let mut state = AppState::new(50, 1);
        state.albums = (0..5000).map(|i| AlbumInfo {
            key: format!("cd-{i}"), title: format!("CD {i}"),
            artist: if i < 4999 { "Ana" } else { "Zeca" }.into(),
            genre: "Rock".into(), initial: "C".into(), palette: 0,
            is_recent: false, tracks: vec![],
        }).collect();
        assert!(matches!(state.handle_key("e_long"), Some(Action::OpenAlphabetPicker)));
        state.jump_to_letter("Z");
        assert_eq!(state.album_index, 4999);
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
    }

    #[test]
    fn catalog_refresh_preserves_selection_in_large_library() {
        let mut state = AppState::new(50, 1);
        let albums: Vec<_> = (0..5000)
            .map(|i| AlbumInfo {
                key: format!("album-{i}"),
                title: format!("Álbum {i}"),
                artist: "Artista".into(),
                genre: "Rock".into(),
                initial: "A".into(),
                palette: 0,
                is_recent: false,
                tracks: vec![],
            })
            .collect();
        state.replace_catalog(albums.clone());
        state.album_index = 4200;
        state.focus = FocusState::BrowsingTracks;
        let mut updated = albums;
        updated.reverse();
        state.replace_catalog_keep_focus(updated);
        assert_eq!(state.current_album().unwrap().key, "album-4200");
        assert_eq!(state.focus, FocusState::BrowsingTracks);
    }

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

    #[test]
    fn public_overlays_expire_and_direction_resets_album_clock() {
        use std::time::{Duration, Instant};
        let mut state = AppState::new(50, 1);
        state.albums = vec![AlbumInfo {
            title: "Disco".into(), artist: "Artista".into(), genre: "Rock".into(),
            key: "disco".into(), initial: "D".into(), palette: 0, is_recent: false,
            tracks: vec![TrackInfo { id: 1, title: "Faixa".into(), artist: "Artista".into(),
                album: "Disco".into(), genre: "Rock".into(), file_path: "/f.mp3".into(), file_type: "mp3".into() }],
        }];
        state.handle_key("i");
        state.last_overlay_interaction = Instant::now() - Duration::from_secs(6);
        state.handle_key("w");
        assert!(state.expire_idle_overlay(Instant::now()).is_none());
        assert!(matches!(state.expire_idle_overlay(Instant::now() + Duration::from_secs(6)), Some(Action::Noop)));
        assert_eq!(state.focus, FocusState::BrowsingAlbums);

        state.handle_key("p");
        state.last_overlay_interaction = Instant::now() - Duration::from_secs(6);
        assert!(matches!(state.expire_idle_overlay(Instant::now()), Some(Action::VolumeClosed(50))));
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
    }

    #[test]
    fn operator_exits_with_selected_back_and_never_times_out() {
        use std::time::{Duration, Instant};
        let mut state = AppState::new(50, 1);
        assert!(matches!(state.handle_key("x"), Some(Action::OpenOperatorMenu)));
        state.last_overlay_interaction = Instant::now() - Duration::from_secs(60);
        assert!(state.expire_idle_overlay(Instant::now()).is_none());
        assert!(state.handle_key("u").is_none());
        state.menu_index = OPERATOR_MENU_PRICE;
        assert!(matches!(state.handle_key("o"), Some(Action::OpenPriceMenu)));
        state.handle_key("w");
        state.handle_key("r");
        state.handle_key("o");
        assert_eq!(state.focus, FocusState::OperatorMainMenu);
        assert_eq!(state.song_price, 1); // Voltar descarta a edição.
        state.menu_index = OPERATOR_MENU_BACK;
        assert!(matches!(state.handle_key("o"), Some(Action::CloseOperatorMenu)));
        assert_eq!(state.focus, FocusState::BrowsingAlbums);
    }
}
