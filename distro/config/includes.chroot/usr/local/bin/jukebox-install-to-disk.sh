#!/usr/bin/env bash
# ==============================================================================
# Jukebox OS - Instalador Automatizado para Disco Interno (HD / SSD)
# Alvo: Máquinas Arcade (Socket AM3 / LGA 775)
# ==============================================================================
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

clear
cat << "EOF"
  ____       _       _                  ___   ____  
 |  _ \     | |     | |                / _ \ / ___| 
 | | | | ___| |__   | | __ _ _   ___  | | | |\___ \ 
 | |_| |/ _ \ '_ \  | |/ _` | | | \ \ / / |_| |___) |
 |____/ \___|_.__/  |_|\__,_|_| |_|\_/\_/ \___/|____/ 
       JUKEBOX EMBEDDED OS - INSTALADOR DE DISCO
EOF

if [ "$(id -u)" -ne 0 ]; then
    echo -e "${RED}[ERRO] Este instalador deve ser executado como root (sudo).${NC}"
    exit 1
fi

echo -e "${CYAN}--> Localizando discos disponíveis no sistema...${NC}"
DISKS=($(lsblk -dn -o NAME,TYPE,TRAN,SIZE | awk '$2=="disk" && $3!="usb" {print "/dev/"$1}'))

if [ ${#DISKS[@]} -eq 0 ]; then
    echo -e "${YELLOW}[AVISO] Nenhum disco SATA/IDE interno foi filtrado diretamente.${NC}"
    echo "Discos detectados no sistema:"
    lsblk -d -o NAME,SIZE,MODEL,TRAN
    echo ""
    read -rp "Digite o caminho do disco alvo (ex: /dev/sda): " TARGET_DISK
else
    echo -e "Discos internos detectados:"
    for i in "${!DISKS[@]}"; do
        SIZE=$(lsblk -dn -o SIZE "${DISKS[$i]}")
        MODEL=$(lsblk -dn -o MODEL "${DISKS[$i]}" || echo "Generico")
        echo -e "  [$i] ${DISKS[$i]} ($SIZE - $MODEL)"
    done

    if [ ${#DISKS[@]} -eq 1 ]; then
        TARGET_DISK="${DISKS[0]}"
        echo -e "${GREEN}Selecionado automaticamente disco único: $TARGET_DISK${NC}"
    else
        read -rp "Selecione o número do disco de destino: " DISK_IDX
        TARGET_DISK="${DISKS[$DISK_IDX]}"
    fi
fi

if [ ! -b "$TARGET_DISK" ]; then
    echo -e "${RED}[ERRO] Disco inválido: $TARGET_DISK${NC}"
    exit 1
fi

echo ""
echo -e "${RED}!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!${NC}"
echo -e "${RED}  ATENÇÃO: TODOS OS DADOS EM $TARGET_DISK SERÃO DESTRUÍDOS!     ${NC}"
echo -e "${RED}!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!${NC}"
read -rp "Deseja prosseguir com a formatação e instalação? (digite 'sim'): " CONFIRM
if [ "$CONFIRM" != "sim" ]; then
    echo "Instalação cancelada pelo operador."
    exit 0
fi

echo -e "\n${CYAN}[1/6] Desmontando partições existentes em $TARGET_DISK...${NC}"
swapoff -a || true
umount "${TARGET_DISK}"* 2>/dev/null || true

echo -e "${CYAN}[2/6] Criando tabela de partições MBR (compatível AM3/775 BIOS)...${NC}"
# Particionamento:
# P1: Root (Sistema Jukebox) = 12 GB
# P2: Swap = 2 GB
# P3: Dados (/dados) = Restante do disco
parted -s "$TARGET_DISK" mklabel msdos
parted -s "$TARGET_DISK" mkpart primary ext4 1MiB 12GiB
parted -s "$TARGET_DISK" set 1 boot on
parted -s "$TARGET_DISK" mkpart primary linux-swap 12GiB 14GiB
parted -s "$TARGET_DISK" mkpart primary ext4 14GiB 100%

# Atualiza tabela de partições no kernel
partprobe "$TARGET_DISK" || true
sleep 2

# Suporte a formatos de partição padrão (/dev/sda1 ou /dev/nvme0n1p1)
if [[ "$TARGET_DISK" =~ [0-9]$ ]]; then
    PART_ROOT="${TARGET_DISK}p1"
    PART_SWAP="${TARGET_DISK}p2"
    PART_DATA="${TARGET_DISK}p3"
else
    PART_ROOT="${TARGET_DISK}1"
    PART_SWAP="${TARGET_DISK}2"
    PART_DATA="${TARGET_DISK}3"
fi

echo -e "${CYAN}[3/6] Formatando partições com sistemas de arquivos ext4...${NC}"
mkfs.ext4 -F -L "JUKEBOX_SYS" "$PART_ROOT"
mkswap -L "JUKEBOX_SWAP" "$PART_SWAP"
mkfs.ext4 -F -L "JUKEBOX_DATA" "$PART_DATA"

echo -e "${CYAN}[4/6] Montando e copiando sistema operacional para o disco...${NC}"
TARGET_MNT="/mnt/target_jukebox"
mkdir -p "$TARGET_MNT"
mount "$PART_ROOT" "$TARGET_MNT"
mkdir -p "$TARGET_MNT/dados"
mount "$PART_DATA" "$TARGET_MNT/dados"

# Copia da raiz do sistema live para a partição física
echo "Copiando arquivos do sistema (isso levará cerca de 1 a 2 minutos)..."
rsync -aHAXx --numeric-ids --info=progress2 \
    --exclude="/run/*" \
    --exclude="/proc/*" \
    --exclude="/sys/*" \
    --exclude="/dev/*" \
    --exclude="/tmp/*" \
    --exclude="/mnt/*" \
    --exclude="/media/*" \
    --exclude="/dados/*" \
    --exclude="/lost+found" \
    / "$TARGET_MNT/"

# Cria diretórios essenciais vazios
mkdir -p "$TARGET_MNT"/{run,proc,sys,dev,tmp,mnt,media,dados}
chmod 1777 "$TARGET_MNT/tmp"

echo -e "${CYAN}[5/6] Configurando /etc/fstab com UUIDs e montagens seguras...${NC}"
ROOT_UUID=$(blkid -s UUID -o value "$PART_ROOT")
SWAP_UUID=$(blkid -s UUID -o value "$PART_SWAP")
DATA_UUID=$(blkid -s UUID -o value "$PART_DATA")

cat << EOF > "$TARGET_MNT/etc/fstab"
# /etc/fstab - Gerado pelo Instalador Jukebox OS
# <file system>                          <mount point> <type>  <options>                          <dump> <pass>
UUID=$ROOT_UUID  /             ext4    errors=remount-ro                  0      1
UUID=$SWAP_UUID  none          swap    sw                                 0      0
UUID=$DATA_UUID  /dados        ext4    defaults,noatime,errors=remount-ro 0      2
EOF

# Garante permissões do usuário jukebox no diretório /dados
chown -R 1000:1000 "$TARGET_MNT/dados"
chmod 775 "$TARGET_MNT/dados"

# Ativa a blindagem OverlayFS para o sistema instalado
echo 'overlayroot="tmpfs"' > "$TARGET_MNT/etc/overlayroot.conf"

echo -e "${CYAN}[6/6] Instalando e configurando o bootloader GRUB em $TARGET_DISK...${NC}"
mount --bind /dev "$TARGET_MNT/dev"
mount --bind /dev/pts "$TARGET_MNT/dev/pts"
mount --bind /proc "$TARGET_MNT/proc"
mount --bind /sys "$TARGET_MNT/sys"

chroot "$TARGET_MNT" grub-install --target=i386-pc --recheck "$TARGET_DISK"
chroot "$TARGET_MNT" update-grub

# Desmontagem limpa
umount "$TARGET_MNT/sys"
umount "$TARGET_MNT/proc"
umount "$TARGET_MNT/dev/pts"
umount "$TARGET_MNT/dev"
umount "$TARGET_MNT/dados"
umount "$TARGET_MNT"

echo ""
echo -e "${GREEN}================================================================${NC}"
echo -e "${GREEN} INSTALAÇÃO CONCLUÍDA COM SUCESSO NO DISCO $TARGET_DISK!        ${NC}"
echo -e "${GREEN}================================================================${NC}"
echo "Remova o pendrive de instalação e reinicie a máquina."
read -rp "Deseja reiniciar a máquina agora? (s/N): " REBOOT_NOW
if [[ "$REBOOT_NOW" =~ ^[sS]$ ]]; then
    reboot
fi
