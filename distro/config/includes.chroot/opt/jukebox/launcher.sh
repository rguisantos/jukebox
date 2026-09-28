#!/bin/sh
set -eu
set -a
. /dados/jukebox.env
set +a
# Recalculate after loading the operator config: it must not override this guard.
if mountpoint -q /dados; then
    JUKEBOX_DATA_PERSISTENT=1
else
    JUKEBOX_DATA_PERSISTENT=0
fi
export JUKEBOX_DATA_PERSISTENT
cd /dados
exec /opt/jukebox/jukebox-app
