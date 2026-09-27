#!/usr/bin/env bash
# Run inside Debian 13. Does not modify the host's live-build scripts.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fail() { echo "[jukebox] $*" >&2; exit 1; }
required=(
  config/package-lists/jukebox.list.chroot
  config/hooks/live/01-setup-kiosk.hook.chroot
  config/includes.chroot/opt/jukebox/launcher.sh
  config/includes.chroot/opt/jukebox/catalog_sync.py
  config/includes.chroot/usr/local/bin/jukebox-wifi
  config/includes.chroot/usr/local/sbin/jukebox-install-to-disk
  config/includes.chroot/etc/skel/.xinitrc
  config/includes.chroot/etc/systemd/system/jukebox-kiosk.service
)
for path in "${required[@]}"; do
  [[ -s "$ROOT/distro/$path" ]] || fail "Arquivo obrigatório ausente: distro/$path"
done
if [[ "${1:-}" == --check ]]; then
  echo 'Configuração básica presente.'
  exit 0
fi
source /etc/os-release
[[ "$ID" == debian && "$VERSION_ID" == 13 ]] || fail 'Compile no Debian 13; veja distro/Dockerfile.'
for tool in cargo pkg-config lb xorriso mksquashfs; do
  command -v "$tool" >/dev/null || fail "Dependência ausente: $tool"
done
pkg-config --exists gstreamer-1.0 gstreamer-video-1.0 || fail 'Instale as dependências de desenvolvimento do GStreamer.'
PRIV=()
[[ $EUID == 0 ]] || PRIV=(sudo)
cd "$ROOT/jukebox-app"
cargo build --release --locked
install -m 0755 target/release/jukebox-app "$ROOT/distro/config/includes.chroot/opt/jukebox/jukebox-app"
cd "$ROOT/distro"
"${PRIV[@]}" lb clean
"${PRIV[@]}" lb config
"${PRIV[@]}" lb build
find "$ROOT/distro" -maxdepth 1 -name '*.iso' -print
