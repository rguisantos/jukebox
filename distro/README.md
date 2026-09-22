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

## Como Fazer Manutenção no Sistema Blindado

Como o sistema de arquivos raiz (`/`) opera protegido pelo `overlayroot`, qualquer alteração feita no sistema tradicional é descartada ao reiniciar.

Quando você precisar atualizar pacotes ou alterar configurações permanentes do sistema operacional:

```bash
sudo jukebox-maintenance
```

Este comando abre um shell `chroot` diretamente na partição física em modo Leitura/Escrita (`rw`). Faça as alterações necessárias e digite `exit`. Ao reiniciar, as alterações estarão gravadas.
