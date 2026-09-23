# ==============================================================================
# JUKEBOX OS — Autologin no tty1 dispara a sessão gráfica do kiosk
# ------------------------------------------------------------------------------
# O override do getty@tty1 faz login automático como usuário 'jukebox'.
# Um shell de login lê este .profile: se estamos no tty1 e sem X rodando,
# sobe o startx (que executa o .xinitrc com openbox + jukebox-app).
# Qualquer outro acesso (tty2..tty6 via Alt+F2) cai num shell normal.
# ==============================================================================

if [ -z "$DISPLAY" ] && [ "$(tty 2>/dev/null)" = "/dev/tty1" ]; then
    exec startx -- vt1 -keeptty > /dev/null 2>&1
fi
