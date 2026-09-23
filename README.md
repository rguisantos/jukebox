# Jukebox Arcade OS

Jukebox comercial (moeda/crédito) em **Rust + Slint 1.8**, projetado para
hardware de fliperama legado: AMD Sempron 145 / Core 2 Duo, gráficos Intel
GMA 3150 / Radeon HD onboard, BIOS legada, Debian Kiosk.

## Estrutura do repositório

| Caminho | O que é |
|---|---|
| `jukebox-app/` | Aplicativo: catálogo SQLite, player GStreamer, USB Sync, PIX dinâmico, controles arcade, menu do operador |
| `distro/` | Geração da ISO Kiosk (Debian 13 trixie via live-build) — veja `distro/README.md` |
| `build_iso.sh` | **Orquestrador de deployment**: compila o app, embute o binário na ISO e gera a imagem do appliance |

## Gerar a ISO do appliance (Módulo 6 — Deployment)

```bash
sudo apt install live-build debootstrap squashfs-tools xorriso isolinux syslinux-common
cd distro && lb config && cd ..
./build_iso.sh
# → distro/jukebox-os-trixie-amd64.hybrid.iso
```

Documentação completa (cadeia de boot, watchdog, configuração por máquina,
instalação em disco, segurança): **[`distro/README.md`](distro/README.md)**

## Controles arcade (padrão do aplicativo)

`W` cima · `Q` baixo · `O` confirma · `U` volta · `E`/`R` capas · `I` álbum ·
`P` volume · `Z` crédito · `X` menu do operador
