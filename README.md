# asus-backlight-idle

Many ASUS Zenbook models have no firmware-controlled keyboard backlight: the
EC turns the light on when you press its hotkey, but never dims it again —
the glow stays on forever, draining battery and drawing attention for no
reason. This tool bridges that gap.

It's a single, super-lightweight daemon (a few hundred lines of Rust, one
binary, zero CPU while idle) that turns the keyboard backlight off after N
seconds without input and brings it back the moment you type or move the
mouse — exactly the behaviour the firmware should be providing out of the box.

## Features

- Zero-polling, zero-CPU idle detection (event-driven via `poll(2)`)
- No keylogging: event payloads are discarded immediately after noting activity
- Works under Wayland, X11, TTY, and the login screen alike
- Respects manual changes made with `brightnessctl` or `Fn+F7`
- USB keyboard hotplug handled live via inotify — no restart needed
- Runs as your user, not root

## Compatibility

The only hardware assumption is a sysfs LED named `asus::kbd_backlight`,
which the kernel's `asus-wmi` driver exposes on virtually all modern ASUS
laptops. Everything else (input handling, state machine) is generic Linux.

| Should work | Probably won't |
|---|---|
| Zenbook / Zenbook Pro / Zenbook Duo | Laptops with no keyboard backlight at all |
| VivoBook | Models whose backlight isn't exposed as an LED class device |
| ROG / TUF gaming laptops | Very old models pre-dating `asus-wmi` |

Brightness levels are read dynamically from sysfs, so it doesn't matter
whether your model offers 2 levels or 3 — whatever you set is what gets
restored.

Check whether your machine qualifies:

```sh
ls /sys/class/leds/ | grep kbd_backlight
# asus::kbd_backlight  ← good to go
```

### Tested on

- ASUS Zenbook 14 UM3406HA (Ryzen AI 9, Hyprland/Wayland)

## Requirements

- Linux with evdev and standard LED class support (any recent kernel)
- Rust toolchain (only for building: `cargo`)
- systemd user session (for the per-user service)
- Membership in the `input` group (default on most single-user setups;
  check with `groups`)

## Install

```sh
./install.sh
```

What it does:

1. Builds the release binary and installs it to `/usr/local/bin`.
2. Installs a udev rule (`90-asus-kbd-backlight.rules`) that grants the
   `input` group write access to `/sys/class/leds/asus::kbd_backlight/brightness`
   — the exact same trick the `brightnessctl` package uses. You are already in
   that group, so the daemon can then run as *your user*, no root daemon.
   (Yes, this means any user in `input` can change the backlight — the price of
   simplicity; on a single-user laptop that's fine.)
3. Removes any leftover root system service from older installs.
4. Installs and enables a **user** systemd service (`systemctl --user`) so it
   starts at login and runs per-user. As installed, the unit dims after
   **15 seconds**; edit `~/.config/systemd/user/asus-backlight-idle.service`
   and `systemctl --user daemon-reload` to change it.

```sh
./install.sh uninstall   # removes everything, restores root-only brightness file
```

For it to also run *before* login (login screen / lock screen): enable
lingering for your user, so your user services start at boot:

```sh
sudo loginctl enable-linger $USER
```

If you'd rather not touch permissions at all, a root system service works too:
install the unit in this repo as `/etc/systemd/system/asus-backlight-idle.service`
with `[Install] WantedBy=multi-user.target`, then `sudo systemctl enable --now`.
The binary needs root only to write the sysfs file; everything else works as
any user in the `input` group.

## Usage

```
asus-backlight-idle [OPTIONS]

  --idle <secs>       idle seconds before dimming (default: 30)
  --led-dir <path>    LED brightness directory (default: /sys/class/leds/asus::kbd_backlight)
  --input-dir <path>  input device directory (default: /dev/input)  [testing]
  --verbose           log extra detail to stderr
```

You normally don't run it by hand — the user service does. To try it once:

```sh
pkill -f asus-backlight-idle
/usr/local/bin/asus-backlight-idle --idle 15 --verbose
```

## Behaviour in detail

- While you type or move the mouse, the backlight stays at your level.
- N seconds after the last input it goes to `0`.
- The next input brings it back to the last level *you* chose — if you set
  brightness 2 with `brightnessctl` or `Fn+F7`, it restores 2, not a hardcoded
  1. If you re-light it manually while it's dark, the daemon notices and leaves
  your choice alone.
- One nuance: if the backlight was already off when the daemon starts, the
  first keypress turns it back on (level 1). If you want it to stay off until
  you say so, turn it on once (Fn+F7) after the daemon is running and it will
  adopt that as your preference.
- Media keys, brightness keys, and the touchpad toggle count as activity too,
  so adjusting volume keeps the light on.

## Known limitations

- **Games using raw input** (`EVIOCGRAB`, e.g. some engines) capture the
  keyboard exclusively, so events never reach the daemon and the light may stay
  off while you play. Quitting the game restores normal behaviour. Rare and
  harmless — it cannot cause the light to stay *on*.
- Some ASUS models re-light the keyboard from the EC on the next keypress even
  after you set 0. That's fine: the daemon sees it as "user re-lit manually"
  and simply leaves it alone.

## Troubleshooting

**Daemon logs "WARNING: ... not found — is this really an ASUS laptop?"**
No `asus::kbd_backlight` LED exists on your machine. Check
`ls /sys/class/leds/`; if there's no kbd_backlight entry, your model either
has no backlight or doesn't expose it through `asus-wmi`. Try
`sudo modprobe asus_wmi` first.

**Light never dims.**
Is anything else watching input and keeping the timer alive? Run with
`--verbose` and check which devices were opened. Also make sure only one
instance is running: `pgrep -af asus-backlight-idle`.

**Permission denied writing brightness.**
Re-run `./install.sh` (it reapplies the udev rule), or manually:

```sh
sudo chgrp input /sys/class/leds/asus::kbd_backlight/brightness
sudo chmod g+w    /sys/class/leds/asus::kbd_backlight/brightness
```

**Service not running / not starting at boot.**

```sh
systemctl --user status asus-backlight-idle.service
journalctl --user -u asus-backlight-idle.service -f
# starts before login?
loginctl show-user $USER -p Linger
```

## Testing without root

`/dev/input/event*` is writable by the `input` group, so you can simulate input
without touching the keyboard. `tools/inject-event.py` writes events straight
into a device node:

```sh
# simulate a mouse nudge:
python3 tools/inject-event.py /dev/input/event5 2 0 5

# simulate a key press+release (KEY_A = 30) on the built-in keyboard:
python3 tools/inject-event.py /dev/input/event2 1 30 1 0

# point the daemon at a scratch LED dir to test the whole state machine
# without touching the real backlight:
mkdir -p /tmp/fakeled && echo 1 > /tmp/fakeled/brightness
./target/release/asus-backlight-idle --led-dir /tmp/fakeled --idle 3 --verbose
```

(Don't run the injection while you're actively typing in an editor — the
injected keys go wherever the focused window would receive them.)

## Design notes

- **Not polling.** The daemon blocks in `poll(2)` on the evdev device nodes of
  every keyboard/pointer. The kernel wakes it *only* when something actually
  happens. Zero CPU while idle, no busy loops, no periodic timers. The idle
  timer *is* the `poll()` timeout — no timerfd, no extra wakeups.
- **Not keylogging.** Key codes and motion deltas are looked at only to answer
  "did the user interact?" and are thrown away immediately. Nothing is logged,
  stored, or sent anywhere. (A kernel module would see exactly the same events —
  the input core delivers to all handlers — so it buys nothing privacy-wise.)
- **Works everywhere.** evdev sits below the compositor: Wayland, X11, TTY,
  even the login screen all behave identically. No per-window heuristics to
  get confused by.
- **Hotplug-safe.** An inotify watch on `/dev/input` triggers rescans when
  devices appear/disappear; dead fds are dropped automatically.

## How it works, step by step

One loop, one `poll(2)` over: the inotify fd on `/dev/input` plus one fd per
interactive device. Events are classified as activity only if they are
`EV_KEY` / `EV_REL` / `EV_ABS`; everything else (`SYN` barriers, `MSC` scan
codes, LED echo, jack detect, the power button, the lid switch) is ignored, so
it can't accidentally keep the keyboard lit.

Device selection: a device is watched if it has `BTN_LEFT` (mouse/touchpad/
trackpoint) or any of a small set of keys (letters, digits, modifiers,
Enter/Esc/Tab, F1, plus WMI hotkeys like kbdillum and brightness). This
deliberately excludes power buttons, lid switches, and PC speakers so they
can't reset the idle timer.

Brightness writes go straight to
`/sys/class/leds/asus::kbd_backlight/brightness`; external changes are adopted
by reading the file at each transition, which needs no polling.
