#!/bin/sh
# Desativa descanso de tela e economia de energia (tela sempre ativa)
xset -dpms
xset s off
xset s noblank

# Inicia o Openbox como gerenciador de janelas ultra-leve
exec openbox-session
