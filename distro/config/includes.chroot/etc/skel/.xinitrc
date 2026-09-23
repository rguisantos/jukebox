# ==============================================================================
# JUKEBOX OS — MÓDULO 6: sessão X11 mínima do kiosk
# ------------------------------------------------------------------------------
# Cadeia completa de boot e auto-recuperação:
#
#   getty@tty1 (autologin) → .bash_profile → startx (-nocursor)
#     → este .xinitrc → openbox-session
#       → ~/.config/openbox/autostart  (xset + watchdog)
#         → /opt/jukebox/launcher.sh → loop infinito { jukebox-app; sleep 3 }
#
#   - App crasha  → launcher reinicia em 3s
#   - X cai       → startx sai → getty respawna → sessão inteira refaz
#   - Kernel hang → watchdog de hardware reinicia a máquina
# ==============================================================================

# --- Logs de campo em /dados (overlay gravável, sobrevive ao reboot) -------------
# Tudo que roda abaixo daqui (openbox, launcher, jukebox-app) herda este
# redirecionamento — um único arquivo para depurar a máquina no bar.
mkdir -p /dados/logs 2>/dev/null || true
LOG="/dados/logs/xsession.log"
if [ -d /dados/logs ]; then
    # Impede crescimento infinito em máquina ligada 24/7: se o log da sessão
    # anterior passou de 5 MB, recomeça do zero.
    if [ -f "$LOG" ] && [ "$(stat -c%s "$LOG" 2>/dev/null || echo 0)" -gt 5242880 ]; then
        : > "$LOG"
    fi
    exec >> "$LOG" 2>&1
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] === Sessão X11 do Jukebox OS iniciada ==="
fi

# --- GPU legada -------------------------------------------------------------------
# Placas GMA 3150 / Radeon HD onboard expõem o GL clássico do Mesa, suficiente
# para o renderer FemtoVG do Slint com aceleração de hardware. Se uma máquina
# específica exibir tela preta (GL quebrado), entre em modo de manutenção
# (sudo jukebox-maintenance), edite este arquivo e descomente:
# export LIBGL_ALWAYS_SOFTWARE=1

# Gerenciador de janelas ultraleve — o autostart dele (~/.config/openbox/
# autostart) desliga o DPMS e sobe o watchdog do aplicativo.
exec openbox-session
