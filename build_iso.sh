#!/usr/bin/env bash
# ==============================================================================
# JUKEBOX OS — MÓDULO 6: build_iso.sh (Orquestrador da ISO Kiosk)
# ------------------------------------------------------------------------------
# Script mestre de deployment, executado NA RAIZ DO PROJETO. Produz a ISO
# completa do appliance com o aplicativo Rust EMBUTIDO dentro dela:
#
#   [1/6] Verifica/instala dependências do host (live-build e afins)
#   [2/6] Compila o aplicativo Rust          (cargo build --release)
#   [3/6] Garante a estrutura do live-build  (distro/config/includes.chroot/…)
#   [4/6] Embute o binário na imagem         (/opt/jukebox/jukebox-app)
#   [5/6] Permissões de execução             (hooks, launcher, scripts)
#   [6/6] Executa lb clean + lb build        (dentro de distro/)
#
# USO (na raiz do projeto):
#     ./build_iso.sh            # como usuário comum (usa sudo internamente)
#     sudo ./build_iso.sh       # também funciona (o cargo roda como o dono)
#
# PRIMEIRA VEZ NO HOST (uma única vez):
#     sudo apt install live-build debootstrap squashfs-tools xorriso \
#                      isolinux syslinux-common
#     cd distro && lb config && cd ..   # inicializa config/ via auto/config
#     ./build_iso.sh
#
# COMPATIBILIDADE GLIBC (importante!):
#     O binário é compilado NO SEU HOST e roda DENTRO da ISO (Debian 13
#     "trixie", glibc 2.41). Hosts com glibc mais antiga (Debian 12/13,
#     Ubuntu 22.04/24.04) sempre geram binários compatíveis. Evite hosts
#     testing/sid (glibc > 2.41).
# ==============================================================================
set -euo pipefail

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; CYAN='\033[0;36m'; NC='\033[0m'
info() { echo -e "${CYAN}[build_iso]${NC} $*"; }
ok()   { echo -e "${GREEN}[OK]${NC} $*"; }
erro() { echo -e "${RED}[ERRO]${NC} $*" >&2; exit 1; }

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${PROJECT_ROOT}/jukebox-app"
DISTRO_DIR="${PROJECT_ROOT}/distro"
APP_BIN="${APP_DIR}/target/release/jukebox-app"
APP_DEST_DIR="${DISTRO_DIR}/config/includes.chroot/opt/jukebox"
HOOK_SRC="${DISTRO_DIR}/config/hooks/live/01-setup-kiosk.hook.chroot"

# --- Root / sudo ---------------------------------------------------------------
# O cargo build roda como usuário COMUM (nunca root: evita um target/ do root).
# O lb config/clean/build precisa de root (debootstrap). Se o script foi
# chamado via sudo, o cargo é rebaixado de volta ao usuário original.
SUDO=""
if [[ ${EUID} -ne 0 ]]; then
    command -v sudo >/dev/null 2>&1 \
        || erro "Sem sudo disponível. Rode: sudo ./build_iso.sh"
    SUDO="sudo"
fi
AS_USER=()
if [[ ${EUID} -eq 0 && -n "${SUDO_USER:-}" ]]; then
    USER_HOME="$(getent passwd "${SUDO_USER}" | cut -d: -f6)"
    AS_USER=(sudo -u "${SUDO_USER}" env "PATH=${USER_HOME}/.cargo/bin:${PATH}" "CARGO_HOME=${USER_HOME}/.cargo")
fi

# --- [1/6] Dependências do host -------------------------------------------------
info "1/6 — Verificando dependências do host (live-build, xorriso, isolinux…)"
if ! command -v lb >/dev/null 2>&1; then
    info "live-build ausente — instalando (requer sudo; ~2 min na primeira vez)…"
    ${SUDO} apt-get update -qq
    ${SUDO} apt-get install -y --no-install-recommends \
        live-build debootstrap squashfs-tools xorriso isolinux syslinux-common \
        || erro "Falha ao instalar o live-build. Manualmente: sudo apt install live-build"
fi
command -v cargo >/dev/null 2>&1 \
    || erro "cargo não encontrado — instale o Rust: https://rustup.rs"

# Patches para o bug do live-build do UBUNTU ao gerar imagens Debian
# (bootlogo do gfxboot ausente e isohybrid procurado no pacote errado).
# Aplicados apenas se o bug estiver presente no host — no-op no Debian.
if [ -f /usr/lib/live/build/lb_binary_syslinux ] && ! grep -q 'if \[ -e "\${_TARGET}/bootlogo" \]; then' /usr/lib/live/build/lb_binary_syslinux; then
    ${SUDO} sed -i 's|tmpdir="$(mktemp -d)"|if [ -e "${_TARGET}/bootlogo" ]; then tmpdir="$(mktemp -d)"|g' /usr/lib/live/build/lb_binary_syslinux
    ${SUDO} sed -i 's|rm -rf "$tmpdir"|rm -rf "$tmpdir"; fi|g' /usr/lib/live/build/lb_binary_syslinux
fi
if [ -f /usr/lib/live/build/lb_binary_iso ]; then
    ${SUDO} sed -i 's|Check_package chroot/usr/bin/isohybrid syslinux$|Check_package chroot/usr/bin/isohybrid syslinux-utils|g' /usr/lib/live/build/lb_binary_iso
fi

# --- [2/6] Compilação do aplicativo (release) ------------------------------------
info "2/6 — Compilando o jukebox-app (cargo build --release)…"
[[ -f "${APP_DIR}/Cargo.toml" ]] || erro "Cargo.toml não encontrado em ${APP_DIR}"
cd "${APP_DIR}"
"${AS_USER[@]}" cargo build --release
[[ -f "${APP_BIN}" ]] || erro "Binário não gerado após o build: ${APP_BIN}"
ok "Binário pronto: ${APP_BIN}"

# --- [3/6] Estrutura do live-build -------------------------------------------------
info "3/6 — Garantindo a estrutura distro/config/…"
mkdir -p "${APP_DEST_DIR}"
mkdir -p "${DISTRO_DIR}/config/includes.chroot/etc/skel/.config/openbox"
mkdir -p "${DISTRO_DIR}/config/hooks/live"
mkdir -p "${DISTRO_DIR}/config/package-lists"

# --- [4/6] Binário para dentro da imagem -------------------------------------------
info "4/6 — Embutindo o binário na imagem: /opt/jukebox/jukebox-app"
${SUDO} install -m 0755 "${APP_BIN}" "${APP_DEST_DIR}/jukebox-app"

# --- [5/6] Permissões de execução ----------------------------------------------------
info "5/6 — Permissões de execução (hooks, launcher e scripts auxiliares)"
[[ -f "${HOOK_SRC}" ]] || erro "Hook não encontrado: ${HOOK_SRC}"

# Cópia de compatibilidade para hosts com live-build ANTIGO (Ubuntu 20.04/22.04),
# que só executam hooks em config/hooks/*.chroot (e não em config/hooks/live/).
# No live-build moderno a cópia é ignorada (o padrão de busca não é recursivo),
# então o hook nunca roda duas vezes. É gerada a cada build, não versionada.
cp -f "${HOOK_SRC}" "${DISTRO_DIR}/config/hooks/01-setup-kiosk.chroot"

find "${DISTRO_DIR}/config/hooks" -type f -name '*.chroot' -exec chmod 0755 {} +
chmod 0755 "${APP_DEST_DIR}/launcher.sh" 2>/dev/null || true
chmod 0755 "${DISTRO_DIR}/config/includes.chroot/usr/local/bin/"* 2>/dev/null || true
chmod 0755 "${DISTRO_DIR}/config/includes.chroot/usr/local/sbin/"* 2>/dev/null || true
chmod 0755 "${DISTRO_DIR}/auto/config" "${DISTRO_DIR}/auto/clean" 2>/dev/null || true

# --- [6/6] lb clean + lb build ---------------------------------------------------------
info "6/6 — Gerando a ISO (lb clean + lb build dentro de distro/)…"
cd "${DISTRO_DIR}"

# (Re)inicializa o config/ a partir de distro/auto/config — idempotente,
# obrigatório num clone novo e recria os symlinks dos hooks de exemplo que
# o live-build executa junto com os nossos.
${SUDO} lb config

# Desmonta montagens remanescentes de builds interrompidos (Ctrl+C no meio
# do debootstrap deixa /proc, /sys e /dev amarrados dentro do chroot).
clean_chroot_mounts() {
    local m
    for m in $(grep "$(pwd)/chroot" /proc/mounts 2>/dev/null | awk '{print $2}' | sort -r); do
        info "Desmontando restos de build anterior: $m"
        ${SUDO} umount -lf "$m" 2>/dev/null || true
    done
}
clean_chroot_mounts
${SUDO} lb clean
clean_chroot_mounts

info "Rodando lb build (baixa os pacotes oficiais do Debian e monta a ISO;"
info "na primeira vez leva 10 a 30 min dependendo da conexão)…"
${SUDO} lb build

# --- Resultado ----------------------------------------------------------------------------
ISO_FILE="$(ls -t *.iso 2>/dev/null | head -n 1 || true)"
[[ -n "${ISO_FILE}" ]] || erro "A ISO não foi gerada — revise o log do lb build acima."

# A ISO nasce como root (lb build roda como root): devolve ao usuário que
# chamou o script, para poder gravar no pendrive sem sudo.
if [[ ${EUID} -eq 0 && -n "${SUDO_USER:-}" ]]; then
    chown "${SUDO_USER}:" "${ISO_FILE}" 2>/dev/null || true
fi

echo ""
echo -e "${GREEN}================================================================${NC}"
echo -e "${GREEN} ISO GERADA COM SUCESSO${NC}"
echo -e "${GREEN}   Arquivo: ${DISTRO_DIR}/${ISO_FILE}${NC}"
echo -e "${GREEN}   Tamanho: $(du -h "${ISO_FILE}" | cut -f1)${NC}"
echo -e "${GREEN}================================================================${NC}"
echo ""
echo "Gravação no pendrive:"
echo "  Linux:   sudo dd if=${ISO_FILE} of=/dev/sdX bs=4M status=progress oflag=sync"
echo "  Windows: Rufus no modo DD — ou apenas copie a ISO para um pendrive Ventoy."
echo "  (substitua /dev/sdX pelo dispositivo do pendrive; TODOS os dados dele serão apagados)"
