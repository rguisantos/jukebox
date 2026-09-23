#!/bin/bash
# ==============================================================================
# JUKEBOX OS — MÓDULO 6: /opt/jukebox/launcher.sh (Launcher + Watchdog)
# ------------------------------------------------------------------------------
# Executado em background pelo autostart do Openbox, como usuário jukebox.
# Contrato:
#   1. Garante o /dados/jukebox.env (recria a partir do template do sistema)
#   2. Carrega as variáveis de ambiente com "set -a" (exporta tudo que ler)
#   3. Executa /opt/jukebox/jukebox-app em PRIMEIRO PLANO, num loop infinito:
#      se o app fechar ou crashar, espera 3 segundos e sobe de novo.
#
# Observações de engenharia:
#   - NÃO usar "set -e" aqui: o propósito do script é justamente sobreviver
#     às falhas do aplicativo.
#   - Tudo que roda abaixo da sessão X (openbox, este launcher, o app) herda
#     o redirecionamento do ~/.xinitrc: /dados/logs/xsession.log — um único
#     arquivo para depurar a máquina no bar.
#   - Se o próprio X cair, o startx termina, o getty@tty1 respawna e a
#     cadeia inteira (login → X → openbox → launcher → app) se refaz sozinha.
# ==============================================================================

# Respira: deixa o X11 e o ALSA terminarem de subir
sleep 2

ENV_FILE="/dados/jukebox.env"
ENV_TEMPLATE="/etc/jukebox/jukebox.env"
APP="/opt/jukebox/jukebox-app"

# --- Configuração persistente da máquina ---------------------------------------
# Máquina nova (ou /dados recém-formatado pelo instalador): recria o
# jukebox.env a partir do template somente-leitura do sistema — o operador
# edita uma vez e pronto.
if [ ! -f "$ENV_FILE" ] && [ -f "$ENV_TEMPLATE" ]; then
    mkdir -p /dados 2>/dev/null || true
    cp "$ENV_TEMPLATE" "$ENV_FILE" 2>/dev/null || true
    echo "launcher: criado $ENV_FILE a partir do template do sistema."
fi

# set -a exporta automaticamente cada variável lida — sem isso o jukebox-app
# (processo filho) não enxergaria nada do que foi "sourceado" aqui.
if [ -f "$ENV_FILE" ]; then
    set -a
    source "$ENV_FILE"
    set +a
    echo "launcher: configuração carregada de $ENV_FILE"
else
    echo "launcher: AVISO — $ENV_FILE não encontrado; usando padrões internos do app."
fi

# --- Placa de som: garante Master e PCM ligados (algumas BIOS mutam no boot) ---
# O volume fino da sessão é controlado pelo próprio aplicativo; aqui só
# garantimos que o canal de hardware não nasça mudo.
amixer -q sset Master unmute 2>/dev/null || true
amixer -q sset Master 100%   2>/dev/null || true
amixer -q sset PCM unmute    2>/dev/null || true
amixer -q sset PCM 100%      2>/dev/null || true

# --- Binário ---------------------------------------------------------------------
if [ ! -x "$APP" ]; then
    echo "launcher: ERRO — binário não encontrado em $APP."
    # Última caixa de diálogo possível num kiosk sem toolkit: o xmessage
    xmessage -center -timeout 0 \
        "JUKEBOX OS

Binario nao encontrado em:
$APP

Regrave o pendrive com a ISO gerada pelo build_iso.sh." &
    exit 1
fi

# --- Loop infinito de proteção (Watchdog) -----------------------------------------
while true; do
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] launcher: iniciando o jukebox-app…"

    # Executa em PRIMEIRO PLANO: quando o app sair (crash/fechamento), o
    # controle volta exatamente aqui.
    "$APP"
    EXIT_CODE=$?

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] launcher: aplicativo encerrou (código $EXIT_CODE). Reiniciando em 3 segundos…"
    sleep 3
done
