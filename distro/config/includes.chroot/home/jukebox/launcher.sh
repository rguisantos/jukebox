#!/bin/bash
# ==============================================================================
# JUKEBOX OS — MÓDULO 6: Launcher + Watchdog
# ------------------------------------------------------------------------------
# Executado em background pelo autostart do Openbox. Garante que o Jukebox
# NUNCA fique fechado: se o aplicativo encerrar ou crashar, é reiniciado em
# 3 segundos. Se o próprio X cair, o ciclo getty → .bash_profile → startx
# reconstrói a sessão inteira automaticamente.
#
# A configuração local da máquina (chaves do PIX, ID, modo demo) mora na
# partição gravável /dados — editar /dados/jukebox.env e reiniciar basta.
# ==============================================================================

# Aguarda o X11 e o servidor de áudio respirarem
sleep 2

# --- Configuração persistente da máquina (partição /dados, leitura/escrita) ---
ENV_FILE="/dados/jukebox.env"
ENV_TEMPLATE="/etc/jukebox/jukebox.env"

# Máquina nova (ou env perdida): cria o arquivo a partir do template somente-
# leitura do sistema — o operador edita uma vez e pronto.
if [ ! -f "$ENV_FILE" ] && [ -f "$ENV_TEMPLATE" ]; then
    mkdir -p /dados 2>/dev/null || true
    cp "$ENV_TEMPLATE" "$ENV_FILE" 2>/dev/null || true
    echo "launcher: criado $ENV_FILE a partir do template do sistema."
fi

if [ -f "$ENV_FILE" ]; then
    # set -a exporta cada variável carregada — sem isso o jukebox-app
    # (processo filho) não enxergaria nada do que foi "sourceado"
    set -a
    source "$ENV_FILE"
    set +a
    echo "launcher: configuração carregada de $ENV_FILE"
else
    echo "launcher: AVISO — $ENV_FILE não encontrado; usando padrões do app."
fi

# --- Localiza o binário do Jukebox ---------------------------------------------
# Ordem: build local na máquina (workflow de desenvolvimento), instalação
# oficial da ISO (/opt/jukebox) e, por fim, /usr/local/bin.
APP=""
for CANDIDATE in \
    /home/jukebox/jukebox-app/target/release/jukebox-app \
    /opt/jukebox/jukebox-app \
    /usr/local/bin/jukebox-app; do
    if [ -x "$CANDIDATE" ]; then
        APP="$CANDIDATE"
        break
    fi
done

if [ -z "$APP" ]; then
    echo "launcher: ERRO — binário do jukebox não encontrado."
    xmessage -center -timeout 0 \
        "JUKEBOX OS\n\nBinario nao encontrado.\n\nEsperado em:\n/opt/jukebox/jukebox-app" &
    exit 1
fi

# --- Loop infinito de proteção (Watchdog) --------------------------------------
while true; do
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] launcher: iniciando o Jukebox OS ($APP)..."

    # Executa o binário em PRIMEIRO PLANO: quando ele sair (crash/fechamento),
    # o controle volta exatamente aqui.
    "$APP"
    EXIT_CODE=$?

    # Se chegou aqui, o app fechou ou crashou
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] launcher: aplicativo encerrado (código $EXIT_CODE). Reiniciando em 3 segundos..."
    sleep 3
done
