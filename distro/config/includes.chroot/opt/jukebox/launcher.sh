#!/bin/sh
set -eu
set -a
. /dados/jukebox.env
set +a
cd /dados
exec /opt/jukebox/jukebox-app
