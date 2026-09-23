#!/usr/bin/env bash
# ==============================================================================
# Script Mestre de Compilação - Jukebox OS ISO (Debian 12 Live-Build)
# ==============================================================================
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

echo -e "${CYAN}================================================================${NC}"
echo -e "${CYAN}    GERADOR DE IMAGEM ISO - JUKEBOX OS (DEBIAN 12 BOOKWORM)    ${NC}"
echo -e "${CYAN}================================================================${NC}"

if [ "$(id -u)" -ne 0 ]; then
    echo -e "${RED}[ERRO] Este script precisa ser executado como root (sudo).${NC}"
    echo "Exemplo: sudo ./build_iso.sh"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo -e "\n${YELLOW}[1/4] Verificando e instalando dependências do host...${NC}"
apt-get update -qq
apt-get install -y --no-install-recommends \
    live-build \
    debootstrap \
    squashfs-tools \
    xorriso \
    isolinux \
    syslinux-common

echo -e "\n${YELLOW}[2/4] Ajustando permissões dos scripts e hooks...${NC}"
chmod +x auto/clean auto/config
chmod +x config/hooks/*.chroot config/hooks/live/*.chroot 2>/dev/null || true
chmod +x config/includes.chroot/usr/local/bin/* || true
chmod +x config/includes.chroot/home/jukebox/.xinitrc || true
chmod +x config/includes.chroot/home/jukebox/launcher.sh || true

# Corrige bug específico do pacote live-build do Ubuntu ao compilar Debian (ausência de bootlogo do gfxboot)
if [ -f /usr/lib/live/build/lb_binary_syslinux ] && ! grep -q 'if \[ -e "\${_TARGET}/bootlogo" \]; then' /usr/lib/live/build/lb_binary_syslinux; then
    sed -i 's|tmpdir="\$(mktemp -d)"|if [ -e "${_TARGET}/bootlogo" ]; then tmpdir="$(mktemp -d)"|g' /usr/lib/live/build/lb_binary_syslinux
    sed -i 's|rm -rf "\$tmpdir"|rm -rf "$tmpdir"; fi|g' /usr/lib/live/build/lb_binary_syslinux
fi

# Garante que o lb_binary_iso procure o isohybrid no pacote correto do Debian 12 (syslinux-utils)
if [ -f /usr/lib/live/build/lb_binary_iso ]; then
    sed -i 's|Check_package chroot/usr/bin/isohybrid syslinux$|Check_package chroot/usr/bin/isohybrid syslinux-utils|g' /usr/lib/live/build/lb_binary_iso
fi

# Função para desmontar com segurança qualquer ponto de montagem remanescente
clean_chroot_mounts() {
    echo "Verificando se há montagens ativas no chroot..."
    for m in $(grep "$SCRIPT_DIR/chroot" /proc/mounts 2>/dev/null | awk '{print $2}' | sort -r); do
        echo "Desmontando: $m"
        umount -lf "$m" 2>/dev/null || true
    done
}

echo -e "\n${YELLOW}[3/4] Limpando compilações anteriores e desmontando sistemas de arquivos...${NC}"
clean_chroot_mounts
lb clean noauto --purge || true
clean_chroot_mounts

# Remove chroot se ainda existir após desmonte
if [ -d "$SCRIPT_DIR/chroot" ]; then
    rm -rf "$SCRIPT_DIR/chroot" || true
fi

echo -e "\n${YELLOW}[4/4] Executando 'lb config' e compilando a ISO com 'lb build'...${NC}"
echo "Isso baixará os pacotes oficiais do Debian Bookworm e gerará a ISO híbrida."
echo "Tempo estimado: 5 a 10 minutos dependendo da sua conexão com a internet."
echo ""

lb config
lb build

ISO_FILE=$(ls -t *.iso 2>/dev/null | head -n 1 || true)

if [ -n "$ISO_FILE" ] && [ -f "$ISO_FILE" ]; then
    echo -e "\n${GREEN}================================================================${NC}"
    echo -e "${GREEN} SUCESSO! IMAGEM ISO GERADA COM SUCESSO:                         ${NC}"
    echo -e "${GREEN} Arquivo: $SCRIPT_DIR/$ISO_FILE                                  ${NC}"
    echo -e "${GREEN} Tamanho: $(du -h "$ISO_FILE" | cut -f1)                         ${NC}"
    echo -e "${GREEN}================================================================${NC}"
    echo -e "\nInstruções para gravação no pendrive:"
    echo "  - Linux: sudo dd if=$ISO_FILE of=/dev/sdX bs=4M status=progress oflag=sync"
    echo "  - Windows: Use Rufus (modo DD) ou basta copiar para um pendrive com Ventoy."
else
    echo -e "\n${RED}[ERRO] O arquivo ISO não foi gerado. Verifique os logs acima.${NC}"
    exit 1
fi
