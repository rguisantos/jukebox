# ==============================================================================
# JUKEBOX OS — MÓDULO 6: Autologin do tty1 dispara a sessão gráfica
# ------------------------------------------------------------------------------
# O override do getty@tty1 faz login automático como usuário 'jukebox'.
# Um shell de login lê este .bash_profile: se estamos no tty1 e sem X rodando,
# sobe o startx SEM CURSOR (o servidor X nem desenha o ponteiro do mouse —
# nada de unclutter, zero software extra na máquina legada).
# Qualquer outro acesso (tty2..tty6 via Alt+F2, ou SSH) cai em shell normal.
# ==============================================================================

if [[ -z $DISPLAY && $(tty) == /dev/tty1 ]]; then
    # vt1: amarra a sessão ao terminal corrente (permissão de VT sem root)
    # -keeptty: mantém o Xorg anexado ao tty (logs e sinalização corretos)
    # -nocursor: o cursor do mouse nunca é desenhado — kiosk total
    exec startx -- vt1 -keeptty -nocursor > /dev/null 2>&1
fi
