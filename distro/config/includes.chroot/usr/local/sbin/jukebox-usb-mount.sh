#!/bin/sh
# ==============================================================================
# JUKEBOX OS — Montador de pendrive chamado pelo udev (99-jukebox-usb.rules)
# ------------------------------------------------------------------------------
# Uso:
#   jukebox-usb-mount.sh add    /dev/sdXn   -> monta read-only em /media/usb
#   jukebox-usb-mount.sh remove /dev/sdXn   -> desmonta /media/usb
#
# Notas de engenharia:
#   - Roda como root no momento do evento udev (hotplug), ANTES do app
#     sequer perceber: o jukebox-app apenas faz polling de /media/usb;
#   - Usa flock para não disparar duas montagens simultâneas (alguns
#     pendrives geram eventos add + change em rajada);
#   - O devnode montado é registrado em /run/jukebox-usb.dev para que a
#     desmontagem no "remove" seja precisa mesmo com outros discos USB;
#   - Retentativas no "add": alguns controladores USB lentos levam alguns
#     instantes para expor o nó de dispositivo após o evento.
# ==============================================================================

ACTION="$1"
DEVNODE="$2"
MOUNT_POINT="/media/usb"
STATE_FILE="/run/jukebox-usb.dev"
LOCK_FILE="/run/jukebox-usb.lock"

# Ignora chamadas sem argumento (udev às vezes chama RUN sem $devnode)
[ -n "$DEVNODE" ] || exit 0

# Garante o diretório de estado do runtime
mkdir -p /run 2>/dev/null

# Seção crítica: um hotplug por vez (eventos add/change chegam em rajada)
exec 9>"$LOCK_FILE"
flock 9

case "$ACTION" in
  add)
    # Já montado? Ignora eventos duplicados do mesmo pendrive
    if grep -qs " $MOUNT_POINT " /proc/mounts; then
      exit 0
    fi

    # Espera o devnode aparecer (controladores USB lentos)
    n=0
    while [ ! -b "$DEVNODE" ] && [ "$n" -lt 10 ]; do
      sleep 0.5
      n=$((n + 1))
    done
    [ -b "$DEVNODE" ] || exit 0

    mkdir -p "$MOUNT_POINT"

    # Montagem read-only, sem atime (menos I/O no barramento USB 2.0)
    if mount -o ro,noatime "$DEVNODE" "$MOUNT_POINT" 2>/dev/null; then
      echo "$DEVNODE" > "$STATE_FILE"
      logger -t jukebox-usb "Pendrive montado (read-only): $DEVNODE -> $MOUNT_POINT"
    else
      logger -t jukebox-usb "Falha ao montar $DEVNODE em $MOUNT_POINT"
      rmdir "$MOUNT_POINT" 2>/dev/null
    fi
    ;;

  remove)
    # Só desmonta se o dispositivo removido for o que está montado
    MOUNTED_DEV="$(cat "$STATE_FILE" 2>/dev/null)"
    if [ "$DEVNODE" = "$MOUNTED_DEV" ]; then
      # Desmontagem preguiçosa: se o app ainda estiver lendo (cópia em
      # andamento), os blocos já abertos terminam antes de sumir de vez
      umount -l "$MOUNT_POINT" 2>/dev/null
      rm -f "$STATE_FILE"
      rmdir "$MOUNT_POINT" 2>/dev/null
      logger -t jukebox-usb "Pendrive removido: $MOUNT_POINT desmontado"
    fi
    ;;
esac

exit 0
