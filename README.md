# Jukebox Arcade OS

Jukebox Rust + Slint 1.8, SQLite e GStreamer, destinada a PCs x86-64 antigos.
Distro Debian 13 mínima com inicialização dedicada, ALSA, Wi-Fi e acervo offline.

- [Configuração, preços, controles e modos automáticos](OPERACAO.md)
- [Build e operação da distro](distro/README.md)
- [Contrato de atualização online do acervo](distro/ONLINE-CATALOG.md)
- Aplicativo em `jukebox-app/`; configuração da ISO em `distro/config/`.

Esta revisão inclui cache de capas por proximidade, fila persistente com débito
atômico, importação USB por pastas e sincronização incremental HTTPS. O acesso
Wi-Fi do operador usa nmtui em uma janela de manutenção. Pix permanece na
implementação anterior até a integração com o serviço de saldo existente.

Controles preservados: W/Q navegam, E/R capas, I álbum, O filtra gênero/seleciona faixa, U volta/pula,
P volume, Z crédito, X operador, A zera saldo, L sai do aplicativo.

Validações de boot BIOS/UEFI, Wi-Fi, áudio sob carga e desempenho com 5.000 álbuns
em hardware real são necessárias antes de instalar em produção.

### Estrutura do aplicativo

`jukebox-app/src/main.rs` inicializa os serviços e despacha ações.
`catalog_ui.rs` publica o catálogo no event loop do Slint, traduz faixas e
álbuns para os modelos visuais e limita o carregamento de capas à janela de
álbuns visível. Comandos entre a seleção e o player carregam `TrackInfo`,
preservando metadados do domínio fora dos tipos gerados pela interface.
`db/`, `media/` e `state/` mantêm respectivamente persistência, mídia e
transições de navegação. O próximo corte estrutural natural é mover o
despacho dos comandos de banco ainda presente em `main.rs` para um serviço
próprio, com uma API que também possa atender a futura integração Pix.
