# Jukebox OS - Distribuição Debian Customizada e Ultra-Leve

Esta pasta contém o gerador oficial da imagem ISO (**Debian 12 Bookworm amd64**) personalizada para máquinas de Jukebox/Arcade de rua (Socket AM3 e LGA 775).

## Destaques da Arquitetura do Sistema
- **Base Ultra-Leve**: Debian Bookworm sem Desktop Environment (sem GNOME/KDE/XFCE).
- **Sem Overhead de Áudio**: Sem PulseAudio ou PipeWire. Apenas **ALSA nativo**, economizando memória e ciclos de CPU no AMD Sempron 145 e Core 2 Duo.
- **Boot Direto em Modo Quiosque**: Autologin no `tty1`, inicialização imediata do X11 com `openbox` e tela preta sem cursor.
- **Blindagem Completa contra Quedas de Energia**: `overlayroot` (OverlayFS em tmpfs) mantendo a partição raiz (`/`) 100% como Somente Leitura (Read-Only). Puxar da tomada não corrompe o sistema.
- **Persistência de Dados**: Partição `/dados` formatada em `ext4` separada do OverlayFS para armazenar arquivos MP3/MP4, logs e o banco SQLite (créditos persistentes).
- **Hardware Watchdog**: Monitora o processador contra superaquecimento e reinicia automaticamente em caso de congelamento.
- **Silent Boot (Arcade)**: Oculta totalmente mensagens do kernel e cursor piscante durante o boot.

---

## Como Gerar a Imagem ISO

Em qualquer computador rodando Debian, Ubuntu ou derivado, execute:

```bash
cd /home/bilhares/jukebox/distro
sudo chmod +x build_iso.sh
sudo ./build_iso.sh
```

O script instalará automaticamente as ferramentas necessárias (`live-build`, `debootstrap`, `xorriso`), baixará os pacotes limpos dos repositórios oficiais e gerará o arquivo `jukebox-os-bookworm-amd64.hybrid.iso`.

---

## Como Gravar no Pendrive

### Opção 1: Ventoy (Recomendado - Mais Rápido)
1. Instale o [Ventoy](https://www.ventoy.net/) no seu pendrive.
2. Copie o arquivo `.iso` gerado diretamente para o pendrive.

### Opção 2: Gravação Direta (Linux)
```bash
sudo dd if=jukebox-os-bookworm-amd64.hybrid.iso of=/dev/sdX bs=4M status=progress oflag=sync
```
*(Substitua `/dev/sdX` pelo dispositivo correto do seu pendrive)*.

### Opção 3: Rufus (Windows)
1. Abra o Rufus e selecione o arquivo `.iso`.
2. Quando solicitado, selecione o **Modo Imagem DD**.

---

## Como Instalar na Máquina Arcade (AM3 / 775)

1. Conecte o pendrive na máquina do Jukebox e configure a BIOS para inicializar pelo USB.
2. O sistema inicializará diretamente no ambiente Live silencioso.
3. Para instalar no disco interno (HD ou SSD), execute o instalador automatizado:
   ```bash
   sudo jukebox-install-to-disk.sh
   ```
4. O instalador irá:
   - Detectar o HD/SSD interno.
   - Criar o particionamento ideal (`/` com 12GB, `swap` com 2GB e `/dados` com o restante do disco).
   - Formatar as partições em `ext4`.
   - Clonar o sistema e instalar o bootloader GRUB.
   - Ativar a blindagem do `overlayroot`.
5. Ao concluir, remova o pendrive e reinicie a máquina.

---

## Cadeia de Boot do Appliance (Módulo 6) e Auto-Recuperação

```
getty@tty1 (autologin jukebox)
  └─ .bash_profile ─── tty1 sem X? → exec startx -- vt1 -keeptty -nocursor
       └─ .xinitrc ─── logs em /dados/logs/xsession.log → exec openbox-session
            └─ ~/.config/openbox/autostart ─── xset (sem DPMS/tela preta)
                 └─ launcher.sh (watchdog, loop infinito)
                      └─ jukebox-app ── crash? reinicia em 3s
```

- **App crasha** → `launcher.sh` sobe o Jukebox de novo em 3 segundos.
- **Servidor X cai** → `startx` termina, o `getty` respawna e a sessão inteira é reconstruída.
- **Kernel congela** → hardware watchdog reinicia a máquina.
- **Cursor do mouse** → oculto pelo próprio Xorg (`-nocursor`), sem software extra.

## Configuração por Máquina: /dados/jukebox.env

As chaves do PIX e o ID da máquina moram na partição gravável — configurar
uma máquina nova é editar um arquivo e reiniciar:

```bash
sudo nano /dados/jukebox.env
```

```ini
JUKEBOX_PIX_API=https://meu-backend.com/api/pix
JUKEBOX_MACHINE_ID=JBOX-001
JUKEBOX_PIX_DEMO=0    # 1 = QR + pagamento simulados, sem backend
RUST_LOG=info
```

Na primeira inicialização o `launcher.sh` cria o arquivo a partir do template
somente-leitura `/etc/jukebox/jukebox.env`. Com o `overlayroot` ativo, este é
o único ponto de configuração que sobrevive entre reinicializações.

---

## Como Fazer Manutenção no Sistema Blindado

Como o sistema de arquivos raiz (`/`) opera protegido pelo `overlayroot`, qualquer alteração feita no sistema tradicional é descartada ao reiniciar.

Quando você precisar atualizar pacotes ou alterar configurações permanentes do sistema operacional:

```bash
sudo jukebox-maintenance
```

Este comando abre um shell `chroot` diretamente na partição física em modo Leitura/Escrita (`rw`). Faça as alterações necessárias e digite `exit`. Ao reiniciar, as alterações estarão gravadas.
