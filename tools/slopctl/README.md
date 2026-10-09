# slopctl

Drive the Slop handheld from a Mac over Wi-Fi SSH or USB adb with Python 3 standard library tools.

Connect a **data USB-A to USB-C cable before powering on**. A USB-C to USB-C cable connected to the MacBook did not enumerate. Press POWER for normal startup because a cable-only start may enter charger mode. After a disconnect, reboot with the cable connected. BaseOS does not reconnect adb after a later cable attachment. Install Android platform-tools once (`brew install --cask android-platform-tools`). Put adb on PATH or use its normal `/opt/homebrew/bin/adb` location. Use `/opt/homebrew/bin/python3` or run the executable directly.

Run commands from the checkout. Auto selection probes `ssh -o BatchMode=yes -o ConnectTimeout=3 slop true` once per invocation, then falls back to USB adb if SSH does not answer. Exactly one USB adb device must be ready for that fallback. Use `--transport auto|ssh|adb` to select a transport and `--host` to choose an SSH host or config alias. `SLOPCTL_TRANSPORT` and `SLOPCTL_HOST` set their defaults (`auto` and `slop`); command-line options take precedence. Place global options before the command. `-v` reports the selection on stderr, and `status` always prints the transport in use.

SSH requires a working passwordless host alias in `~/.ssh/config`. One-time `ssh slop` key and host-key setup is outside this tool. Commands use BatchMode, real remote exit status, and no stdin except file payloads or interactive sessions. SSH pushes stream bytes to `cat` into a unique temporary file beside the target, then use `mv -f`. Pulls stream `cat` stdout. Neither scp nor sftp is required. SSH screenshots stream `dd` of the visible framebuffer page directly; adb screenshots use a temporary file and pull because old adbd stdout is not binary-safe.

Each operation has a timeout of at most 30 seconds. Interactive shell sessions also end after 30 seconds. `wait` probes SSH in auto mode before waiting for USB adb; forced SSH waits within its requested connection timeout.

| Command | Example |
|---|---|
| Status, including hold and battery | `tools/slopctl/slopctl status` |
| Wait for the device, default 20 s, maximum 30 s | `tools/slopctl/slopctl wait --timeout 20` |
| Logs: slot, prev, session, boot, frontend, all | `tools/slopctl/slopctl logs all --lines 200 --grep 'slot: shader:'` |
| Keep the console powered on during an agent session | `tools/slopctl/slopctl awake on` |
| Restore normal doze power-off | `tools/slopctl/slopctl awake off` |
| Check the awake flag | `tools/slopctl/slopctl awake status` |
| Restart slot through init respawn | `tools/slopctl/slopctl restart` |
| Park the frontend with a hold in /run | `tools/slopctl/slopctl stop` |
| Clear the hold and wait for slot | `tools/slopctl/slopctl start` |
| Deploy changed files | `tools/slopctl/slopctl deploy --only system` |
| Preview a deploy | `tools/slopctl/slopctl deploy --src /path/to/dist-device --dry-run` |
| Capture the visible framebuffer page | `tools/slopctl/slopctl shot screen.png --rotate 90` |
| List button names and event codes | `tools/slopctl/slopctl buttons` |
| Press and release a button | `tools/slopctl/slopctl press A --hold 100` |
| Run a sequence with device-side delays | `tools/slopctl/slopctl seq 'A 100, wait 500, DOWN, START'` |
| Run a command with checked remote exit status | `tools/slopctl/slopctl shell uname -a` |
| Run a pipeline (one quoted argument is passed to the device shell as is) | `tools/slopctl/slopctl shell 'grep "slot: shader" /mnt/sdcard/slot.log \| tail -5'` |
| Open a bounded interactive shell | `tools/slopctl/slopctl shell` |
| Push a file directly | `tools/slopctl/slopctl push ./probe.sh /tmp/probe.sh` |
| Pull a file directly | `tools/slopctl/slopctl pull /mnt/sdcard/slot.log ./slot.log` |

`stop` requires the updated `card/System/launch_frontend.sh` on the device. Copy it into your dist-device tree before the first system deploy if that tree predates hold support. The hold lasts until `start` or a reboot. Restart uses SIGTERM, matching BaseOS shutdown. With the updated slot SIGTERM handler, this flushes saves, syncs, and exits cleanly. Restart waits up to 10 seconds for exit, then sends SIGKILL if needed with a warning that saves may be lost. It waits for the old process to exit and for a respawned pid. `stop` keeps its hold set and reports an error if SIGTERM does not finish. After existing processes exit, it polls once per second in one device shell call, sends SIGTERM to any late frontend, and requires two consecutive empty scans before reporting success. The quiet check fails after 15 seconds and leaves the hold set. Restart and stop check both `slot.log` and `slot.log.1` for each exited PID and print whether its save completed. An incomplete save or missing acknowledgement produces a warning. Older slot builds can exit without an acknowledgement. The updated handler uses one eight-second deadline and exits with status 1 if its save is incomplete.

`awake on` creates `/run/slop-awake`, keeping the console from powering itself off 3 minutes after the lid closes or POWER is tapped while an agent session is running. It requires the updated slot doze handler. `awake off` removes the flag and restores normal power-off behavior. The flag lives on tmpfs and is cleared on reboot. This is separate from the frontend hold.

## Deploy safety

The default source is `SLOPCTL_SRC`, or `<repo>/../slop-device-build/dist-device` if that directory exists. Otherwise supply `--src`. An explicit `--src` takes precedence. Selection is `system`, `shaders`, `config-examples`, or `all`. The default `all` covers System, Shaders, Audio, Config, Cheats, Labels, and Wallpapers. Other source files are skipped.

- Deploy never writes into Games, Saves, States, or BIOS. Selecting one of those as the source is refused. Symlinks and parent traversal are refused.
- Existing Config files are preserved unless their names end in `.example` or `.sample`, or contain `.example.` or `.sample.`. Missing Config files can be installed with `all`. `config-examples` installs only examples.
- Device hashes are collected in one shell call. SHA-256 is preferred, with MD5 as a runtime fallback. Missing hash tools fail before deployment.
- Changed files are pushed to unique temporary names beside their targets, then replaced with `mv -f`. The live binary is never overwritten in place. Deploy does not delete destination files.
- Deploy calls `sync`, then restarts slot unless `--no-restart` is supplied. Use `--no-restart` while the frontend is held. A dry run prints planned transfers and changes no card files. Long adb commands use temporary scripts in /tmp, which are removed after use. SSH sends scripts directly.

Raw `shell`, `push`, and `pull` are deliberate passthroughs. Deploy protection rules apply to `deploy`.

Input nodes are discovered by the same capability codes as slot and their runtime names in `/proc/bus/input/devices`, never by a fixed event number. Each write rechecks the sysfs device name. The D-pad uses ABS_HAT0X/Y. Sequence durations are milliseconds, with at most 50 actions and 20 seconds of requested delays. A missing fractional sleep falls back to whole seconds. Screenshots support BGRA/BGRX at 32 bpp and RGB565 at 16 bpp; rotation is clockwise. Without an output path they go under `./slopctl-shots/` with a timestamp.

Run the off-device checks with `python3 -m unittest discover -s tools/slopctl -v` and `task test:card`.
