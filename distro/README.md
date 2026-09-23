# JUKEBOX OS — ISO Kiosk (Debian 13 Trixie, live-build)

Gerador oficial da imagem ISO do appliance: Debian enxuto, autologin no tty1,
X11 sem cursor, aplicativo Rust **embutido na imagem** e rodando sob watchdog.

## Arquitetura do Módulo 6 (Deployment)

```
jukebox/                        raiz do projeto
├── build_iso.sh                ← ORQUESTRADOR (executa da raiz!)
├── jukebox-app/                aplicativo Rust (cargo build --release)
└── distro/                     árvore do live-build
    ├── auto/config             parâmetros do "lb config" (build reprodutível)
    ├── auto/clean              limpeza segura (desmonta chroot de build travado)
    └── config/
        ├── hooks/live/01-setup-kiosk.hook.chroot
        │                       ← usuário jukebox + autologin tty1 + sudoers
        │                         poweroff + /dados + systemd garantido
        ├── includes.chroot/
        │   ├── opt/jukebox/launcher.sh    WATCHDOG (loop infinito, sleep 3)
        │   ├── opt/jukebox/jukebox-app    binário (copiado pelo build_iso.sh)
        │   ├── etc/skel/                  a home do usuário nasce daqui:
        │   │   ├── .bash_profile          tty1 sem X? → startx -- -nocursor
        │   │   ├── .profile               fallback POSIX do autologin
        │   │   ├── .xinitrc               log em /dados/logs + openbox-session
        │   │   └── .config/openbox/autostart   xset (sem DPMS) + launcher &
        │   ├── etc/jukebox/jukebox.env    template da configuração da máquina
        │   ├── etc/udev/rules.d/99-jukebox-usb.rules   automontagem p/ USB Sync
        │   └── etc/{overlayroot,watchdog,modules-load.d,…}
        │                                     blindagens do appliance
        ├── package-lists/jukebox.list.chroot   pacotes da imagem
        └── bootloaders/                      syslinux/isolinux pinados
                                              (fix para hosts Ubuntu)
```

## Como gerar a ISO

No host de build (Debian, Ubuntu ou derivado):

```bash
# 1) Dependências do host (uma única vez)
sudo apt install live-build debootstrap squashfs-tools xorriso \
     isolinux syslinux-common
#    Para compilar o aplicativo, além do Rust (https://rustup.rs):
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
     pkg-config build-essential

# 2) Inicializa o live-build (uma única vez por clone)
cd distro
lb config          # executa o auto/config: trixie, amd64, boot silencioso…
cd ..

# 3) Gera a ISO — compila o Rust, embute o binário e roda o lb build
./build_iso.sh
```

Resultado: **`distro/jukebox-os-trixie-amd64.hybrid.iso`** (híbrida: boota em
BIOS legada — Sempron 145 / LGA 775 — e em UEFI).

O `build_iso.sh` também aceita `sudo ./build_iso.sh`: nesse caso o `cargo` é
rebaixado de volta ao usuário dono do projeto (nunca compila como root, para
não sujar o `target/`). Primeira build: 10 a 30 min (baixa o Debian inteiro).

### Nota de compatibilidade glibc (importante)

O binário é compilado **no seu host** e roda **dentro da ISO** (Debian 13
trixie = glibc 2.41). Regra de ouro: glibc do host ≤ glibc da imagem.
Debian 12/13 e Ubuntu 22.04/24.04 sempre funcionam; evite hosts Debian
testing/sid. (Para voltar ao Debian 12 na imagem, edite `auto/config` — e
use um host com glibc ≤ 2.36 para compilar.)

## Gravação no pendrive

| Método | Comando / procedimento |
|---|---|
| dd (Linux) | `sudo dd if=distro/jukebox-os-trixie-amd64.hybrid.iso of=/dev/sdX bs=4M status=progress oflag=sync` |
| Ventoy | Apenas copie a ISO para o pendrive (mais rápido) |
| Rufus (Windows) | Selecione a ISO e o **modo DD** |

## Cadeia de boot do appliance e auto-recuperação

```
BIOS → isolinux (silencioso)
  └─ systemd → getty@tty1 (autologin jukebox)
       └─ .bash_profile ─── tty1 sem X? → startx -- vt1 -keeptty -nocursor
            └─ .xinitrc ─── loga em /dados/logs/xsession.log → openbox-session
                 └─ autostart ─── xset (sem DPMS/screensaver)
                      └─ /opt/jukebox/launcher.sh (watchdog)
                           └─ /opt/jukebox/jukebox-app ── crash? reinicia em 3s
```

- **App crasha** → watchdog relança em 3 segundos.
- **Servidor X cai** → `startx` termina, o getty respawna, sessão inteira refaz.
- **Kernel congela** → watchdog de hardware reinicia a máquina (15 s).
- **Cursor do mouse** → oculto pelo próprio Xorg (`-nocursor`), zero software extra.

## Configuração por máquina: `/dados/jukebox.env`

```bash
sudo nano /dados/jukebox.env   # e reinicie a máquina
```

```ini
JUKEBOX_PIX_API=https://meu-backend.com/api/pix
JUKEBOX_MACHINE_ID=JBOX-001
JUKEBOX_PIX_DEMO=0    # 1 = QR + pagamento simulados, sem backend
RUST_LOG=info
```

Na sessão live o `/dados` é o overlay gravável (nasce com o arquivo, via hook);
no sistema instalado em disco o `/dados` é a 3ª partição (ext4) e o launcher
recria o arquivo a partir do template `/etc/jukebox/jukebox.env` na primeira
inicialização. Ali também moram o banco SQLite (`jukebox.db`), as músicas
(`musicas/`), o cache de capas (`capas/`) e os logs (`logs/`).

## Modelo de segurança

- **Usuário `jukebox` / senha `jukebox`** — acesso de manutenção (Alt+F2 no
  tty2, ou SSH).
- **sudo sem senha: SOMENTE `systemctl poweroff`** — usado pela opção
  "Desligar Máquina" do menu do operador. Todo o resto pede senha.
  O live-config é neutralizado duas vezes: parâmetro `noroot` na linha de
  boot + regra restrita escrita pelo hook em `/etc/sudoers.d/live` (o
  componente de sudo do live-config passa reto quando o arquivo já contém
  uma regra para o usuário).
- **root** — sem senha (travado).

## Live x instalado em disco

- **Live (pendrive)**: boot direto, `/` efêmero (qualquer alteração é
  descartada no reboot — à prova de tomada arrancada), `/dados` no overlay.
- **Disco (HD/SSD)**: rode `sudo jukebox-install-to-disk.sh` na máquina —
  particiona (12 GB sistema + 2 GB swap + resto para /dados), clona, instala
  o GRUB e ativa o overlayroot (sistema somente-leitura em produção).
  Manutenção permanente do sistema: `sudo jukebox-maintenance`.

## Solução de problemas

| Sintoma | Onde olhar |
|---|---|
| Tela preta após logo do boot | tty3 (`Alt+F3`) tem os logs do kernel; `/dados/logs/xsession.log` tem a sessão X |
| App não abre (caixa "Binário não encontrado") | O pendrive foi gravado com uma ISO antiga (sem binário embutido) — gere de novo com `./build_iso.sh` |
| Sem áudio | `amixer` no tty2; verifique se `gstreamer1.0-alsa` está na imagem (`gst-inspect-1.0 alsasink`) |
| GL quebrado na GMA 3150 | descomente `LIBGL_ALWAYS_SOFTWARE=1` no `.xinitrc` (via `jukebox-maintenance` no sistema instalado) |
| Diagnóstico do player | `gst-inspect-1.0` / `gst-launch-1.0` vêm instalados (`gstreamer1.0-tools`) |
