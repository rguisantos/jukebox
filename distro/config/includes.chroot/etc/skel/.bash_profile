# ==============================================================================
# JUKEBOX OS — MÓDULO 6: autologin do tty1 dispara a sessão gráfica
# ------------------------------------------------------------------------------
# Este arquivo mora no /etc/skel e é copiado para a home do usuário jukebox
# pelo hook do live-build (useradd -m). O override do getty@tty1 faz login
# automático; o shell de login lê este arquivo: estando no tty1 e sem X
# rodando, sobe o servidor X SEM CURSOR (o Xorg nem desenha o ponteiro —
# nada de unclutter na máquina legada).
#
# Qualquer outro acesso (tty2..tty6 via Alt+F2, ou SSH) cai em shell normal,
# sem iniciar sessão gráfica — é a porta de manutenção do operador.
# ==============================================================================

if [[ -z $DISPLAY && $(tty) == /dev/tty1 ]]; then
    # vt1:      amarra a sessão X ao terminal corrente (permissão de VT sem root)
    # -keeptty: mantém o Xorg anexado ao tty (logs e sinalização corretos)
    # -nocursor: o cursor do mouse nunca é desenhado — kiosk total
    exec startx -- vt1 -keeptty -nocursor > /dev/null 2>&1
fi
