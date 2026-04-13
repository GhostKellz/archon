#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <destdir>" >&2
  exit 1
fi

destdir="$1"
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
release_dir="$repo_root/target/x86_64-unknown-linux-gnu/release"

mkdir -p "$destdir/usr/bin"
mkdir -p "$destdir/usr/share/archon"
mkdir -p "$destdir/usr/share/archon/tools/build/scripts"
mkdir -p "$destdir/usr/lib/systemd/user"
mkdir -p "$destdir/etc/chromium/native-messaging-hosts"
mkdir -p "$destdir/etc/opt/chrome/native-messaging-hosts"

install -Dm755 "$release_dir/archon" "$destdir/usr/bin/archon"
install -Dm755 "$release_dir/archon-host" "$destdir/usr/bin/archon-host"
install -Dm755 "$release_dir/ghostdns" "$destdir/usr/bin/ghostdns"
install -Dm755 "$release_dir/archon-settings" "$destdir/usr/bin/archon-settings"

install -Dm644 "$repo_root/assets/systemd/user/archon-host.service" "$destdir/usr/lib/systemd/user/archon-host.service"
install -Dm644 "$repo_root/assets/systemd/user/ghostdns.service" "$destdir/usr/lib/systemd/user/ghostdns.service"
install -Dm644 "$repo_root/assets/native-messaging/archon_host.chromium.json" "$destdir/etc/chromium/native-messaging-hosts/sh.ghostkellz.archon.host.json"
install -Dm644 "$repo_root/assets/native-messaging/archon_host.chrome.json" "$destdir/etc/opt/chrome/native-messaging-hosts/sh.ghostkellz.archon.host.json"
install -Dm755 "$repo_root/tools/build/scripts/deploy_ghostdns.sh" "$destdir/usr/share/archon/tools/build/scripts/deploy_ghostdns.sh"

mkdir -p "$destdir/usr/share/archon/themes/chromium"
cp -r "$repo_root/extensions/themes"/* "$destdir/usr/share/archon/themes/chromium/"

mkdir -p "$destdir/usr/share/archon/extensions/archon-sidebar"
cp -r "$repo_root/extensions/archon-sidebar"/* "$destdir/usr/share/archon/extensions/archon-sidebar/"

echo "smoke install complete: $destdir"
