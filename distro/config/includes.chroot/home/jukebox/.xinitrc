# ==============================================================================
# JUKEBOX OS — Sessão X11 do kiosk (executada pelo startx via .profile)
# ------------------------------------------------------------------------------
# Fluxo de boot da máquina:
#   getty@tty1 (autologin jukebox) -> bash login shell -> .profile -> startx
#   -> este .xinitrc -> openbox + jukebox-app em tela cheia
# ==============================================================================

# Desativa descanso de tela e economia de energia (tela sempre ativa)
xset -dpms
xset s off
xset s noblank

# Redireciona logs da sessão para depuração em campo (tty2 / arquivo)
exec >> /dados/logs/xsession.log 2>&1

# --- GPU legada -----------------------------------------------------------------
# Placas GMA 3150 / Radeon HD onboard expõem GL 2.0 clássico do Mesa, que é
# o suficiente para o renderer FemtoVG do Slint com aceleração de hardware.
# Se uma máquina específica exibir tela preta (GL quebrado), entre em modo
# de manutenção (sudo jukebox-maintenance) e descomente a linha abaixo:
# export LIBGL_ALWAYS_SOFTWARE=1

# --- Configuração do serviço PIX (Módulo 5) ------------------------------------
# Backend na nuvem que gera o QR Code dinâmico e responde o polling de status
export PIX_API_URL="https://meu-backend.com"
# Identificador único desta máquina no backend (troque por máquina!)
export PIX_MAQUINA_ID="JUKEBOX-001"

# --- Ambiente gráfico mínimo ----------------------------------------------------
# Esconde o cursor do mouse após 3s de inatividade (tela é navegada por teclado)
unclutter -idle 3 -root &

# Gerenciador de janelas ultraleve: garante foco e decoração zero para a
# janela do jukebox (obrigatório para o overlay de vídeo do GStreamer
# receber eventos de exposição corretamente)
openbox &

# Aguarda o Xorg assentar antes de subir o app (evita race no boot)
sleep 1

# --- Localiza o binário do jukebox ---------------------------------------------
APP=""
for CANDIDATE in /opt/jukebox/jukebox-app /usr/local/bin/jukebox-app ./jukebox-app; do
    if [ -x "$CANDIDATE" ]; then
        APP="$CANDIDATE"
        break
    fi
done

if [ -n "$APP" ]; then
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Iniciando jukebox: $APP"
    exec "$APP"
fi

# Binário não encontrado: mensagem de campo no lugar de tela preta muda
echo "ERRO: binário do jukebox não encontrado em /opt/jukebox ou /usr/local/bin"
xmessage -center -timeout 0 "JUKEBOX OS\n\nBinario nao encontrado.\nInstale em /opt/jukebox/jukebox-app" &
exec openbox-session
