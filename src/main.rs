// asus-backlight-idle
//
// Turns the ASUS keyboard backlight off after N seconds without keyboard/mouse
// input, and back on as soon as the user interacts again.
//
// Design:
//   * Event-driven, zero polling: we block in poll(2) on the evdev device nodes
//     of every keyboard/pointer device. The kernel wakes us only when the user
//     actually presses a key or moves a mouse.
//   * No keylogging: event payloads (key codes, motion deltas) are discarded as
//     soon as we've noted "activity happened". Nothing is stored or forwarded.
//   * Manual overrides are respected: if you set the backlight with brightnessctl
//     or Fn+F7 (or the EC re-lights it), the daemon adopts that level instead of
//     fighting you.
//   * Device hotplug (USB keyboards, etc.) handled via inotify on /dev/input.
//
// Runs as root (system service): /sys/class/leds/asus::kbd_backlight/brightness
// is root-only. See README.md for install instructions.

use std::env;
use std::fs;
use std::io::{self};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use evdev::raw_stream::RawDevice;
use evdev::{EventType, KeyCode};
use inotify::{Inotify, WatchMask};

const DEFAULT_IDLE_SECS: u64 = 30;
const DEFAULT_LED_DIR: &str = "/sys/class/leds/asus::kbd_backlight";
const DEFAULT_INPUT_DIR: &str = "/dev/input";
const POLL_MAX_TIMEOUT_MS: i32 = 86_400_000; // sanity cap for the poll() timeout

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Opts {
    idle: Duration,
    led_dir: PathBuf,
    input_dir: PathBuf,
    verbose: bool,
}

fn print_usage() {
    eprintln!(
        "asus-backlight-idle — dim the ASUS keyboard backlight when the user is idle\n\n\
         USAGE: asus-backlight-idle [OPTIONS]\n\n\
         OPTIONS:\
         \n    --idle <secs>       idle seconds before dimming (default: {DEFAULT_IDLE_SECS})\
         \n    --led-dir <path>    LED brightness directory (default: {DEFAULT_LED_DIR})\
         \n    --input-dir <path>  input device directory (default: {DEFAULT_INPUT_DIR})\
         \n    --verbose           log extra detail to stderr\
         \n    -h, --help          show this help"
    );
}

fn parse_args() -> Opts {
    let mut idle = DEFAULT_IDLE_SECS;
    let mut led_dir = PathBuf::from(DEFAULT_LED_DIR);
    let mut input_dir = PathBuf::from(DEFAULT_INPUT_DIR);
    let mut verbose = false;

    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        let mut next_val = || {
            args.next()
                .unwrap_or_else(|| {
                    eprintln!("missing value for {a}");
                    std::process::exit(2);
                })
        };
        match a.as_str() {
            "--idle" => {
                idle = next_val().parse().unwrap_or_else(|_| {
                    eprintln!("--idle expects a number of seconds");
                    std::process::exit(2);
                })
            }
            "--led-dir" => led_dir = PathBuf::from(next_val()),
            "--input-dir" => input_dir = PathBuf::from(next_val()),
            "--verbose" => verbose = true,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    Opts {
        idle: Duration::from_secs(idle.max(1)),
        led_dir,
        input_dir,
        verbose,
    }
}

// ---------------------------------------------------------------------------
// Backlight (LED) access
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Led {
    dir: PathBuf,
}

impl Led {
    fn read(&self) -> Option<u8> {
        fs::read_to_string(self.dir.join("brightness"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    fn write(&self, value: u8) -> io::Result<()> {
        fs::write(self.dir.join("brightness"), format!("{value}\n"))
    }
}

// ---------------------------------------------------------------------------
// Device selection and event classification
// ---------------------------------------------------------------------------

/// A device we're watching. We keep the path purely for logging.
struct Watch {
    path: PathBuf,
    dev: RawDevice,
}

/// Decide whether an input device is a keyboard/pointer (or useful hotkeys:
/// brightness/touchpad/volume keys). Everything else — power button, lid
/// switch, PC speaker, audio jack events — is ignored, so it can't keep the
/// backlight awake by itself.
fn is_interactive(dev: &RawDevice) -> bool {
    let keys = match dev.supported_keys() {
        Some(k) => k,
        None => return false,
    };

    // Pointers: mice, touchpads, trackpoints all carry BTN_LEFT.
    if keys.contains(KeyCode::BTN_LEFT) {
        return true;
    }

    // Keyboards and deliberately useful hotkeys.
    const INTERESTING: &[KeyCode] = &[
        KeyCode::KEY_ESC,
        KeyCode::KEY_TAB,
        KeyCode::KEY_ENTER,
        KeyCode::KEY_SPACE,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_A,
        KeyCode::KEY_Z,
        KeyCode::KEY_0,
        KeyCode::KEY_1,
        KeyCode::KEY_F1,
        // Asus WMI / "video bus" hotkeys — pressing Fn+F7 (backlight toggle) or
        // brightness keys is deliberate user activity too.
        KeyCode::KEY_KBDILLUMTOGGLE,
        KeyCode::KEY_KBDILLUMUP,
        KeyCode::KEY_KBDILLUMDOWN,
        KeyCode::KEY_BRIGHTNESSDOWN,
        KeyCode::KEY_BRIGHTNESSUP,
        KeyCode::KEY_TOUCHPAD_TOGGLE,
        KeyCode::KEY_VOLUMEUP,
        KeyCode::KEY_VOLUMEDOWN,
        KeyCode::KEY_MUTE,
        KeyCode::KEY_PLAYPAUSE,
        KeyCode::KEY_PREVIOUSSONG,
        KeyCode::KEY_NEXTSONG,
    ];
    INTERESTING.iter().any(|&k| keys.contains(k))
}

/// Counts as "user activity": key presses/releases, mouse motion, touchpad
/// motion. Everything else (EV_SYN barriers, MSC scan codes, LED echo, ...)
/// is ignored so it can't cause false wake-ups.
fn is_activity(ev: &evdev::InputEvent) -> bool {
    let t = ev.event_type();
    t == EventType::KEY || t == EventType::RELATIVE || t == EventType::ABSOLUTE
}

// ---------------------------------------------------------------------------
// Device enumeration / hotplug
// ---------------------------------------------------------------------------

fn event_device_paths(input_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(input_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect();
    paths.sort();
    paths
}

/// (Re)open every input device and keep the ones that are interactive.
/// Existing watches are kept as-is; new paths are opened, vanished paths dropped.
fn rescan(opts: &Opts, watches: &mut Vec<Watch>) -> io::Result<()> {
    let paths = event_device_paths(&opts.input_dir);

    // Drop watches whose node disappeared (or was renamed during replug).
    watches.retain(|w| paths.contains(&w.path));

    let existing: Vec<PathBuf> = watches.iter().map(|w| w.path.clone()).collect();
    for path in paths {
        if existing.contains(&path) {
            continue; // already open — keep the existing fd
        }
        match RawDevice::open(&path) {
            Ok(dev) => {
                if is_interactive(&dev) {
                    if opts.verbose {
                        eprintln!(
                            "  + {} ({})",
                            path.display(),
                            dev.name().unwrap_or("<unnamed>")
                        );
                    }
                    watches.push(Watch { path, dev });
                }
            }
            Err(e) => eprintln!("cannot open {}: {e}", path.display()),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Signal handling
// ---------------------------------------------------------------------------

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

unsafe fn install_signal_handlers() {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    let mut sa: libc::sigaction = std::mem::zeroed();
    sa.sa_sigaction = handler as libc::sighandler_t;
    sa.sa_flags = 0; // deliberately no SA_RESTART: we want poll() to EINTR on signals
    libc::sigemptyset(&mut sa.sa_mask);
    libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
    libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn run(opts: &Opts) -> io::Result<()> {
    // Watch /dev/input for hotplug events.
    let mut inotify = Inotify::init()?;
    inotify.watches().add(
        &opts.input_dir,
        WatchMask::CREATE | WatchMask::DELETE | WatchMask::ATTRIB,
    )?;

    let mut watches: Vec<Watch> = Vec::new();
    rescan(opts, &mut watches)?;

    let led = Led {
        dir: opts.led_dir.clone(),
    };

    // Sanity check up front: can we actually write the brightness file?
    // (Harmless no-op write of the current value.)
    if let Some(cur) = led.read() {
        if let Err(e) = led.write(cur) {
            eprintln!("WARNING: cannot write {}: {e}", led.dir.join("brightness").display());
            eprintln!("         run as root, or grant write access (see README.md).");
        }
    } else {
        eprintln!(
            "WARNING: {} not found — is this really an ASUS laptop?",
            led.dir.join("brightness").display()
        );
    }

    // State machine:
    //   Active — user has been using the machine; brightness is at `active_level`.
    //   Idle   — timer expired; brightness was set to 0 and stays there until input.
    let cur = led.read().unwrap_or(1);
    let mut active_level = if cur > 0 { cur } else { 1 }; // restore level for later
    let mut idle = cur == 0; // start in Idle only if the backlight was already off
    let mut deadline = Instant::now() + opts.idle; // first dim happens even without prior input
    let mut rescan_pending = false;

    eprintln!(
        "asus-backlight-idle: {} interactive device(s), led {}, idle {}s, current brightness {cur}",
        watches.len(),
        opts.led_dir.display(),
        opts.idle.as_secs(),
    );

    let inotify_fd = inotify.as_raw_fd();
    let shutdown: &AtomicBool = &SHUTDOWN;

    loop {
        let now = Instant::now();
        let timeout = if idle {
            -1 // no deadline while idle: block until input
        } else {
            deadline
                .saturating_duration_since(now)
                .as_millis()
                .min(POLL_MAX_TIMEOUT_MS as u128) as libc::c_int
        };

        let mut fds: Vec<libc::pollfd> = Vec::with_capacity(watches.len() + 1);
        fds.push(libc::pollfd {
            fd: inotify_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        fds.extend(watches.iter().map(|w| libc::pollfd {
            fd: w.dev.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }));

        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            return Err(e);
        }

        if rc > 0 {
            // --- hotplug notifications -------------------------------------
            if fds[0].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 4096];
                loop {
                    // read_events() reports EAGAIN / "EOF" once everything is
                    // drained — treat those as "no more events for this round".
                    let events = match inotify.read_events(&mut buf) {
                        Ok(events) => events,
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::UnexpectedEof
                            ) =>
                        {
                            break;
                        }
                        Err(e) => return Err(e),
                    };
                    let mut got = 0usize;
                    for _ in events {
                        got += 1;
                    }
                    if got == 0 {
                        break;
                    }
                    rescan_pending = true;
                }
            }

            // --- input events ----------------------------------------------
            let mut activity = false;
            let mut dead: Vec<usize> = Vec::new();
            for (i, w) in watches.iter_mut().enumerate() {
                let rev = fds[i + 1].revents;
                if rev & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                    match w.dev.fetch_events() {
                        Ok(events) => {
                            for ev in events {
                                if is_activity(&ev) {
                                    activity = true;
                                }
                            }
                        }
                        Err(_) => dead.push(i), // device unplugged / fd broke
                    }
                }
            }
            for &i in dead.iter().rev() {
                if opts.verbose {
                    eprintln!("  - {}", watches[i].path.display());
                }
                watches.swap_remove(i);
            }
            if !dead.is_empty() {
                rescan_pending = true; // recreate removed nodes, e.g. replug races
            }

            // --- state transitions ------------------------------------------
            if activity {
                if idle {
                    // User started interacting again. Re-light, but honor a manual
                    // re-light first (Fn+F7 or brightnessctl) that might have
                    // happened while we were dark.
                    match led.read() {
                        Some(b) if b > 0 => {
                            active_level = b;
                            eprintln!("activity: user already re-lit backlight (level {b})");
                        }
                        _ => {
                            if let Err(e) = led.write(active_level) {
                                eprintln!("failed to write brightness: {e}");
                            } else {
                                eprintln!("activity: backlight on (level {active_level})");
                            }
                        }
                    }
                }
                idle = false;
                deadline = Instant::now() + opts.idle;
            }

            if rescan_pending {
                rescan(opts, &mut watches)?;
                rescan_pending = false;
            }
        }

        // --- idle check -----------------------------------------------------
        // Runs on every loop iteration (also when poll() timed out).
        if !idle && Instant::now() >= deadline {
            // Adopt the user's current level for the eventual restore — this is
            // how a manual brightnessctl/Fn+F7 change while we're running is
            // picked up without polling the sysfs file.
            if let Some(b) = led.read() {
                if b > 0 {
                    active_level = b;
                }
            }
            if let Err(e) = led.write(0) {
                eprintln!("failed to write brightness: {e}");
            } else {
                eprintln!("idle: backlight off (restore level {active_level})");
            }
            idle = true;
        }
    }

    eprintln!("shutting down");
    Ok(())
}

fn main() {
    let opts = parse_args();
    unsafe { install_signal_handlers() };
    if let Err(e) = run(&opts) {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}