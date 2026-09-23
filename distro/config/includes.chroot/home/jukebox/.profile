# ==============================================================================
# JUKEBOX OS — Autologin no tty1 dispara a sessão gráfica
# ------------------------------------------------------------------------------
# Fallback POSIX: o bash de login lê .bash_profile primeiro (arquivo irmão,
# com a lógica idêntica); este .profile cobre shells não-bash (ex.: console
# serial de manutenção) mantendo o mesmo comportamento de kiosk.
# ==============================================================================

if [ -z "$DISPLAY" ] && [ "$(tty 2>/dev/null)" = "/dev/tty1" ]; then
    # -nocursor: o servidor X não desenha o ponteiro do mouse (kiosk total)
    exec startx -- vt1 -keeptty -nocursor > /dev/null 2>&1
fi
