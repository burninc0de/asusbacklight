#!/usr/bin/env bash
# Build and install asus-backlight-idle as a *user* systemd service.
#
# One-time root step: installs a udev rule that grants the 'input' group write
# access to the ASUS keyboard backlight (you are in that group), so the daemon
# can run as your user instead of root.
#
#   ./install.sh            install + enable
#   ./install.sh uninstall  remove everything
set -euo pipefail
cd "$(dirname "$0")"

BIN=/usr/local/bin/asus-backlight-idle
RULES=/etc/udev/rules.d/90-asus-kbd-backlight.rules
SYS_UNIT=/etc/systemd/system/asus-backlight-idle.service
USER_UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
USER_UNIT="$USER_UNIT_DIR/asus-backlight-idle.service"

if [[ "${1:-}" == "uninstall" ]]; then
    systemctl --user disable --now asus-backlight-idle.service 2>/dev/null || true
    sudo systemctl disable --now asus-backlight-idle.service 2>/dev/null || true
    rm -f "$USER_UNIT"
    sudo rm -f "$SYS_UNIT" "$BIN" "$RULES"
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=leds
    sudo systemctl daemon-reload
    echo "uninstalled."
    exit 0
fi

echo "==> building release binary"
cargo build --release

echo "==> installing binary"
sudo install -Dm755 target/release/asus-backlight-idle "$BIN"

echo "==> installing udev rule (grants 'input' group write access to the backlight)"
sudo install -Dm644 90-asus-kbd-backlight.rules "$RULES"
sudo udevadm control --reload
sudo udevadm trigger --action=add --subsystem-match=leds
# Belt and suspenders: apply it right away instead of waiting on udev.
sudo chgrp input /sys/class/leds/asus::kbd_backlight/brightness 2>/dev/null || true
sudo chmod g+w /sys/class/leds/asus::kbd_backlight/brightness 2>/dev/null || true
sleep 1

echo "==> removing old root service if present"
sudo systemctl disable --now asus-backlight-idle.service 2>/dev/null || true
sudo rm -f "$SYS_UNIT"

echo "==> installing user service"
mkdir -p "$USER_UNIT_DIR"
cp asus-backlight-idle.service "$USER_UNIT"
systemctl --user daemon-reload
systemctl --user enable --now asus-backlight-idle.service

echo
echo "installed. status:"
systemctl --user status --no-pager -l asus-backlight-idle.service | head -12
echo
echo "recent logs:"
journalctl --user -u asus-backlight-idle.service -n 5 --no-pager 2>/dev/null || true
echo
echo "brightness file permissions (group should now be 'input' with write):"
ls -l /sys/class/leds/asus::kbd_backlight/brightness