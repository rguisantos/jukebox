# ==============================================================================
# JUKEBOX OS — MÓDULO 6: fallback POSIX do autologin (shells não-bash)
# ------------------------------------------------------------------------------
# O bash de login lê o .bash_profile primeiro (arquivo irmão, com a lógica
# idêntica); este .profile cobre shells POSIX puros (ex.: console serial de
# manutenção) mantendo o mesmo comportamento de kiosk no tty1.
# ==============================================================================

if [ -z "$DISPLAY" ] && [ "$(tty 2>/dev/null)" = "/dev/tty1" ]; then
    # -nocursor: o servidor X não desenha o ponteiro do mouse (kiosk total)
    exec startx -- vt1 -keeptty -nocursor > /dev/null 2>&1
fi
