#!/bin/sh
# Executa a jukebox do checkout com a configuração privada do PC de testes.
set -eu
profile="$HOME/.config/jukebox/jukebox.env"
if [ ! -f "$profile" ] || [ "$(stat -c '%a' "$profile")" != 600 ]; then
    echo 'Instale antes o perfil local com --local (permissão 0600).' >&2
    exit 1
fi
if [ -d /dados ] && ! mountpoint -q /dados; then
    echo '/dados existe sem montagem: verifique o diretório de dados antes de receber créditos.' >&2
    exit 1
fi
set -a
. "$profile"
set +a
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$script_dir/../../jukebox-app"
exec cargo run --release
