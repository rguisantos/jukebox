# Inicia o servidor X automaticamente apenas no TTY1
if [ -z "$DISPLAY" ] && [ "$(tty)" = "/dev/tty1" ]; then
    exec startx -- -nocursor >/dev/null 2>&1
fi
