#!/usr/bin/env bash
# Build and install asus-backlight-idle.
#
# Default: hardened *system* service (least privilege).
# Only the daemon gets raw input access — not your whole session. Works
# without global 'input' membership (DynamicUser + SupplementaryGroups=input)
# and runs before login. Tight sandbox: NoNewPrivileges, ProtectSystem=strict, etc.
#
#   ./install.sh            install system service (default, recommended)
#   ./install.sh --user     install per-user service (needs 'input' group, opt-in)
#   ./install.sh uninstall  remove everything (both system + user)
set -euo pipefail
cd "$(dirname "$0")"

BIN=/usr/local/bin/asus-backlight-idle
RULES=/etc/udev/rules.d/90-asus-kbd-backlight.rules
SYS_UNIT=/etc/systemd/system/asus-backlight-idle.service
USER_UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
USER_UNIT="$USER_UNIT_DIR/asus-backlight-idle.service"
SYSTEM_SRC="asus-backlight-idle-system.service"
USER_SRC="asus-backlight-idle-user.service"
# Fallback: old repo had only asus-backlight-idle.service as user template
if [[ ! -f "$SYSTEM_SRC" ]]; then SYSTEM_SRC="asus-backlight-idle.service"; fi
if [[ ! -f "$USER_SRC" ]]; then USER_SRC="asus-backlight-idle.service"; fi

MODE="system"
for arg in "${@:-}"; do
  case "$arg" in
    --user) MODE="user" ;;
    --system) MODE="system" ;;
    uninstall) MODE="uninstall" ;;
    -h|--help)
      echo "Usage: $0 [--system] [--user] [uninstall]"
      echo "  (no args)  -> system service (default, hardened, least privilege)"
      echo "  --user     -> per-user service (requires 'input' group, opt-in)"
      echo "  uninstall  -> remove both"
      exit 0
      ;;
  esac
done

if [[ "$MODE" == "uninstall" ]]; then
    echo "==> disabling services"
    systemctl --user disable --now asus-backlight-idle.service 2>/dev/null || true
    sudo systemctl disable --now asus-backlight-idle.service 2>/dev/null || true
    rm -f "$USER_UNIT"
    sudo rm -f "$SYS_UNIT" "$BIN" "$RULES"
    sudo udevadm control --reload 2>/dev/null || true
    sudo udevadm trigger --subsystem-match=leds 2>/dev/null || true
    sudo systemctl daemon-reload 2>/dev/null || true
    systemctl --user daemon-reload 2>/dev/null || true
    echo "uninstalled."
    exit 0
fi

echo "==> building release binary"
cargo build --release

echo "==> installing binary"
sudo install -Dm755 target/release/asus-backlight-idle "$BIN"

echo "==> installing udev rule (fallback for --user manual runs)"
sudo install -Dm644 90-asus-kbd-backlight.rules "$RULES"
sudo udevadm control --reload 2>/dev/null || true
sudo udevadm trigger --action=add --subsystem-match=leds 2>/dev/null || true
# Apply immediately for current boot (udev RUN only fires on add)
sudo chgrp input /sys/class/leds/asus::kbd_backlight/brightness 2>/dev/null || true
sudo chmod g+w /sys/class/leds/asus::kbd_backlight/brightness 2>/dev/null || true
sleep 1

if [[ "$MODE" == "user" ]]; then
    echo "==> installing USER service (per-user, needs 'input' group)"
    echo "    Note: global 'input' allows any process to read keys, so the default"
    echo "    system service avoids it. This mode opts you into 'input'."
    if ! id -nG "$USER" | grep -qw input; then
        echo "    You are NOT in 'input' (groups: $(id -nG)): granting..."
        if sudo gpasswd -a "$USER" input >/dev/null; then
            echo "    Added $USER to 'input'. You must log out/in (or reboot) for it to apply."
            echo "    Hot-fix for now: sg input -c 'systemctl --user restart asus-backlight-idle.service'"
        else
            echo "    WARNING: could not add to 'input'. User service will fail with 'Permission denied'."
        fi
    else
        echo "    Already in 'input' — good."
    fi
    echo "==> removing system service if present"
    sudo systemctl disable --now asus-backlight-idle.service 2>/dev/null || true
    sudo rm -f "$SYS_UNIT"
    sudo systemctl daemon-reload 2>/dev/null || true

    mkdir -p "$USER_UNIT_DIR"
    cp "$USER_SRC" "$USER_UNIT"
    systemctl --user daemon-reload
    systemctl --user enable --now asus-backlight-idle.service

    echo
    echo "installed (user). status:"
    systemctl --user status --no-pager -l asus-backlight-idle.service 2>&1 | head -20
    echo
    echo "recent logs:"
    journalctl --user -u asus-backlight-idle.service -n 10 --no-pager 2>/dev/null || true
else
    echo "==> installing SYSTEM service (default, hardened, least privilege)"
    echo "    No global 'input' membership needed — only the daemon gets input."
    echo "==> removing user service if present"
    systemctl --user disable --now asus-backlight-idle.service 2>/dev/null || true
    rm -f "$USER_UNIT"
    systemctl --user daemon-reload 2>/dev/null || true

    sudo install -Dm644 "$SYSTEM_SRC" "$SYS_UNIT"
    sudo systemctl daemon-reload
    sudo systemctl enable --now asus-backlight-idle.service

    echo
    echo "installed (system). status:"
    systemctl status --no-pager -l asus-backlight-idle.service 2>&1 | head -20
    echo
    echo "recent logs:"
    journalctl -u asus-backlight-idle.service -n 10 --no-pager 2>/dev/null || true
    echo
    echo "Tip: edit /etc/systemd/system/asus-backlight-idle.service to change --idle, then"
    echo "     sudo systemctl daemon-reload && sudo systemctl restart asus-backlight-idle.service"
fi

echo
echo "brightness file:"
ls -l /sys/class/leds/asus::kbd_backlight/brightness 2>&1 || true
echo
id -nG 2>&1 | tr ' ' '\n' | sed 's/^/  groups: /' | head
