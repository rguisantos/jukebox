#!/bin/sh
# =============================================================================
# audit-sysvinit.sh — Auditoria de manifestos do live-build (Debian 13 Trixie)
# =============================================================================
# Uso (a partir de qualquer diretório):
#
#     ./tools/audit-sysvinit.sh [caminho/para/config] [--chroot]
#
#   1º argumento opcional : diretório `config` do live-build
#                           (padrão: config/ ao lado de tools/, ou ./config)
#   --chroot              : além dos manifestos, inspeciona o banco dpkg de
#                           `chroot/` (build parcial) e relata resíduos
#                           instalados — útil quando um build falhou no meio.
#
# O que verifica:
#   [A] Referências diretas à cadeia SysVinit nos manifestos, hooks,
#       includes e arquivos APT do config/ (listas, pref, apt.conf, hooks,
#       includes.chroot*). Entradas DIRETAS em listas quebram o build quando
#       combinadas com a pinagem (pacote sem candidato) e precisam sair.
#       Linhas de comentário são ignoradas (referências históricas ok).
#   [A2] Enumera TODOS os arquivos de lista em config/package-lists/ (qualquer
#       sufixo: .list, .list.chroot, .list.chroot_install, .list.chroot_live...)
#       e marca os não-rastreados pelo git — o .gitignore típico do live-build
#       (distro/config/package-lists/*) os torna INVISÍVEIS ao git status, e um
#       *.list.chroot_live obsoleto envenena SÓ o passe "live" (o passe
#       "install" não o lê — por isso um build pode passar do install e falhar
#       no live com "no installation candidate").
#   [B] Presença e conteúdo das 3 camadas do kit de correção
#       (pref.chroot, listas, hook).
#   [C] Pré-condição de execução de hooks do live-build: é necessário ao
#       menos um hook em config/hooks/normal/*.chroot.
#   [E] chroot/root/packages.chroot (se existir): transação acumulada de
#       passes anteriores — o live-build usa APPEND (>>) e o arquivo só é
#       removido após um passe BEM-SUCEDIDO ou com lb clean --chroot/--all.
#       Um pedido positivo sysvinit aí quebra o próximo build.
#   [D] Com --chroot: pacotes sysvinit efetivamente instalados no chroot.
#
# Saída: relatório no stdout; exit 0 = limpo, exit 1 = achados que exigem
# ação. Avisos (WARN) não alteram o código de saída.
# =============================================================================

BAD_PKGS="live-config-sysvinit sysvinit-core initscripts insserv startpar sysv-rc"

# Padrão grep para os nomes completos (usado em [A] e [E])
PATTERN=$(printf '%s' "${BAD_PKGS}" | tr ' ' '|')

# Arquivos do kit de correção que LEGITIMAMENTE mencionam esses nomes
# (kit genérico + variação do repo jukebox)
WHITELIST="00-block-sysvinit.pref.chroot 00-init-systemd.list.chroot 99-remove-sysvinit.list.chroot 7000-purge-sysvinit-residue.hook.chroot 0091-ensure-live-hooks-run.hook.chroot live.list.chroot 01-setup-kiosk.hook.chroot README-SYSVINIT-FIX.md"

FAILURES=0

say()  { printf '%s\n' "$*"; }
ok()   { printf '  [ OK ] %s\n' "$*"; }
warn() { printf '  [WARN] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

# ---------------------------------------------------------------------------
# Localizar o diretório config
# ---------------------------------------------------------------------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CHECK_CHROOT=0

for ARG in "$@"; do
        case "${ARG}" in
                --chroot) CHECK_CHROOT=1 ;;
                *) CONFIG_DIR="${ARG}" ;;
        esac
done

if [ -z "${CONFIG_DIR}" ]; then
        if [ -d "${SCRIPT_DIR}/../config" ]; then
                CONFIG_DIR="${SCRIPT_DIR}/../config"
        elif [ -d "./config" ]; then
                CONFIG_DIR="./config"
        else
                say "uso: $0 [caminho/para/config] [--chroot]"
                exit 1
        fi
fi

CONFIG_DIR=$(CDPATH= cd -- "${CONFIG_DIR}" 2>/dev/null && pwd)
BUILD_ROOT=$(dirname -- "${CONFIG_DIR}")

say "==================================================================="
say " Auditoria SysVinit — live-build Debian 13 Trixie"
say " config : ${CONFIG_DIR}"
say " build  : ${BUILD_ROOT}"
say "==================================================================="
say ""

# ---------------------------------------------------------------------------
# [A] Referências diretas em manifestos/config (fora do whitelist)
# ---------------------------------------------------------------------------
say "[A] Manifestos e configurações — referências à cadeia SysVinit:"

FOUND=0

if [ ! -d "${CONFIG_DIR}" ]; then
        bad "diretório config não encontrado: ${CONFIG_DIR}"
else
        # grep -w: casa os nomes completos (não casa 'sysvinit-coreutils',
        # por exemplo); pega também formas com hífen final (remoção).
        # Linhas de comentário (iniciadas por #) são descartadas: referências
        # históricas/documentais são legítimas — só entradas ATIVAS quebram build.
        while IFS= read -r LINE; do
                FILE=${LINE%%:*}
                BASE=$(basename -- "${FILE}")
                IS_WHITELISTED=0
                for W in ${WHITELIST}; do
                        [ "${BASE}" = "${W}" ] && IS_WHITELISTED=1
                done
                if [ "${IS_WHITELISTED}" -eq 0 ]; then
                        say "  [FAIL] ${LINE}"
                        FOUND=1
                fi
        done <<EOF
$(grep -rnw -E "${PATTERN}" "${CONFIG_DIR}" 2>/dev/null | grep -vE '^[^:]+:[0-9]+:[[:space:]]*#')
EOF

        if [ "${FOUND}" -eq 1 ]; then
                FAILURES=$((FAILURES + 1))
                say ""
                say "  Ação: remova/ajuste as linhas acima."
                say "  - Nome solto em *.list.chroot  => apague a linha (a pinagem já bloqueia o pacote)."
                say "  - Hífen INICIAL (-pacote)      => troque para hífen FINAL (pacote-) ou apague."
                say "  - Referência legítima em comentário => pode ignorar (ou mova o comentário)."
        else
                ok "nenhuma referência direta nos manifestos (fora o kit de correção)"
        fi
fi
say ""

# ---------------------------------------------------------------------------
# [A2] Enumeração de listas + detecção de não-rastreadas (git)
# ---------------------------------------------------------------------------
say "[A2] Arquivos de lista em config/package-lists/ (rastreamento git):"
PL_DIR="${CONFIG_DIR}/package-lists"
if [ -d "${PL_DIR}" ]; then
        # estamos num repo git? (o config/ pode ser usado standalone)
        IN_GIT=0
        if command -v git >/dev/null 2>&1 && \
           git -C "${BUILD_ROOT}" rev-parse --git-dir >/dev/null 2>&1
        then
                IN_GIT=1
        fi

        UNTRACKED=0
        for LIST_FILE in "${PL_DIR}"/*; do
                [ -f "${LIST_FILE}" ] || continue
                LIST_BASE=$(basename -- "${LIST_FILE}")
                if [ "${IN_GIT}" -eq 1 ]; then
                        if git -C "${BUILD_ROOT}" ls-files --error-unmatch "config/package-lists/${LIST_BASE}" >/dev/null 2>&1 \
                           || git -C "${BUILD_ROOT}" ls-files --error-unmatch "distro/config/package-lists/${LIST_BASE}" >/dev/null 2>&1; then
                                say "  [ rastreado ] ${LIST_BASE}"
                        else
                                say "  [NÃO-RASTREADO] ${LIST_BASE}   <== invisível ao git status (gitignore); confira se é intencional"
                                UNTRACKED=$((UNTRACKED + 1))
                        fi
                else
                        say "  [   lista    ] ${LIST_BASE}"
                fi
        done

        if [ "${UNTRACKED}" -gt 0 ]; then
                warn "${UNTRACKED} lista(s) não-rastreada(s) — o .gitignore do live-build as esconde do git status."
                say "        Um *.list.chroot_live/leftover envenena SÓ o passe 'live' (o passe 'install'"
                say "        não o lê) — cenário clássico de: install pass OK + 'no installation candidate'"
                say "        no live pass. Revise/apague: git clean -fdn config/package-lists/"
        else
                [ "${IN_GIT}" -eq 1 ] && ok "todas as listas são rastreadas pelo git (nenhuma oculta)"
        fi
else
        warn "diretório config/package-lists/ não encontrado"
fi
say ""

# ---------------------------------------------------------------------------
# [B] Camadas do kit de correção
# ---------------------------------------------------------------------------
say "[B] Kit de correção (3 camadas):"

PREF="${CONFIG_DIR}/archives/00-block-sysvinit.pref.chroot"
LIST_INIT="${CONFIG_DIR}/package-lists/00-init-systemd.list.chroot"
LIST_RM="${CONFIG_DIR}/package-lists/99-remove-sysvinit.list.chroot"
HOOK="${CONFIG_DIR}/hooks/live/7000-purge-sysvinit-residue.hook.chroot"

if [ -f "${PREF}" ]; then
        if grep -q -- 'Pin-Priority: -1' "${PREF}" && \
           grep -q -- 'live-config-sysvinit' "${PREF}"; then
                ok "Camada 1 (pinagem): ${PREF}"
        else
                bad "Camada 1: ${PREF} existe mas sem 'Pin-Priority: -1' para live-config-sysvinit"
        fi
else
        warn "Camada 1 (pinagem) ausente: ${PREF}"
fi

LIST_CANONICAL="${CONFIG_DIR}/package-lists/live.list.chroot"

if [ -f "${LIST_INIT}" ] && grep -qw 'live-config-systemd' "${LIST_INIT}"; then
        ok "Camada 2a (backend concreto): ${LIST_INIT}"
elif [ -f "${LIST_CANONICAL}" ] && grep -qw 'live-config-systemd' "${LIST_CANONICAL}"; then
        # Variante canônica: lista gerada pelo lb config, commitada para impedir
        # que resquício local obsoleto (era sysvinit) envenene os builds.
        ok "Camada 2a (backend concreto): ${LIST_CANONICAL} (lista canônica do live-build, commitada)"
else
        warn "Camada 2a ausente ou sem live-config-systemd: ${LIST_INIT} (ou live.list.chroot)"
fi

if [ -f "${LIST_RM}" ]; then
        if grep -q -- '^-live-config-sysvinit' "${LIST_RM}"; then
                bad "Camada 2b: ${LIST_RM} usa hífen INICIAL (inválido p/ apt-get) — use live-config-sysvinit-"
        elif grep -q -- 'live-config-sysvinit-' "${LIST_RM}"; then
                ok "Camada 2b (remoção ativa): ${LIST_RM}"
        else
                warn "Camada 2b presente mas sem entradas de remoção: ${LIST_RM}"
        fi
else
        warn "Camada 2b ausente: ${LIST_RM}"
fi

if [ -f "${HOOK}" ]; then
        if [ -x "${HOOK}" ]; then
                ok "Camada 3 (hook de purga): ${HOOK}"
        else
                bad "Camada 3: ${HOOK} sem permissão de execução (chmod +x)"
        fi
else
        warn "Camada 3 (hook) ausente: ${HOOK}"
fi
say ""

# ---------------------------------------------------------------------------
# [C] Pré-condição de execução de hooks (live-build 1:20250505+deb13u1)
# ---------------------------------------------------------------------------
say "[C] Pré-condição de hooks do live-build:"
if ls "${CONFIG_DIR}"/hooks/normal/*.chroot >/dev/null 2>&1; then
        ok "config/hooks/normal/*.chroot existe (hooks em hooks/live/ serão executados)"
else
        bad "config/hooks/normal/ não tem hooks *.chroot — o live-build NÃO executará"
        say "        os hooks de hooks/live/ (condição do chroot_hooks). Rode 'lb config'"
        say "        novamente ou restaure os symlinks padrão em config/hooks/normal/."
fi
say ""

# ---------------------------------------------------------------------------
# [E] chroot/root/packages.chroot — transação acumulada (append-only)
# ---------------------------------------------------------------------------
PKGS_TX="${BUILD_ROOT}/chroot/root/packages.chroot"
if [ -f "${PKGS_TX}" ]; then
        say "[E] chroot/root/packages.chroot existe (transação de passe anterior acumulada por >>):"
        TX_POISON=$(grep -nw -E "${PATTERN}" "${PKGS_TX}" 2>/dev/null | grep -v -- '-$' || true)
        if [ -n "${TX_POISON}" ]; then
                # heredoc (não pipe): while no MESMO shell, para bad() incrementar FAILURES
                while IFS= read -r TX_LINE; do
                        [ -n "${TX_LINE}" ] || continue
                        bad "pedido positivo no arquivo de transação: ${TX_LINE}"
                done <<EOF
${TX_POISON}
EOF
                say "  Ação: este arquivo sobrevive a runs falhos (só é removido após um passe"
                say "  bem-sucedido ou com lb clean --chroot/--all). Rode 'lb clean --all' antes"
                say "  de refazer o build (ou remova o arquivo manualmente)."
        else
                warn "arquivo de transação presente mas sem pedidos sysvinit — será reaproveitado"
                warn "pelo próximo passe (append >>); se o build anterior falhou, rode 'lb clean --all'."
        fi
        say ""
fi

# ---------------------------------------------------------------------------
# [D] Chroot parcial (opcional, --chroot)
# ---------------------------------------------------------------------------
if [ "${CHECK_CHROOT}" -eq 1 ]; then
        say "[D] Chroot existente (${BUILD_ROOT}/chroot) — resíduos instalados:"
        STATUS="${BUILD_ROOT}/chroot/var/lib/dpkg/status"
        if [ -f "${STATUS}" ]; then
                CHROOT_RESIDUE=0
                for PKG in ${BAD_PKGS}; do
                        if awk -v pkg="${PKG}" '
                                $1 == "Package:" { name = $2 }
                                $1 == "Status:" && $2 == "install" && $4 == "installed" && name == pkg { found = 1 }
                                END { exit !found }
                        ' "${STATUS}" 2>/dev/null; then
                                bad "instalado no chroot: ${PKG}"
                                CHROOT_RESIDUE=1
                        fi
                done
                if [ "${CHROOT_RESIDUE}" -eq 0 ]; then
                        ok "nenhum pacote sysvinit instalado no chroot"
                else
                        say "  Ação: rode 'lb clean --all' e refaça o build com o kit aplicado"
                        say "  (a Camada 2 fará a troca e a Camada 3 purgará o que restar)."
                fi
        else
                warn "chroot/var/lib/dpkg/status não encontrado — nada a inspecionar"
        fi
        say ""
fi

# ---------------------------------------------------------------------------
# Resumo
# ---------------------------------------------------------------------------
say "==================================================================="
if [ "${FAILURES}" -eq 0 ]; then
        say " RESULTADO: LIMPO — nenhum bloqueio para o rebuild."
else
        say " RESULTADO: ${FAILURES} ponto(s) exigindo ação antes do rebuild."
fi
say "==================================================================="

exit $([ "${FAILURES}" -eq 0 ] && echo 0 || echo 1)
