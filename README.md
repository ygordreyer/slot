# Slop

Slop is Ygor Dreyer's combined build of [slot](https://github.com/BrandonKowalski/slot),
the Game Boy frontend by Brandon Kowalski. It targets the Anbernic RG34XXSP running
[BaseOS 1.3.0](https://github.com/pvaibhav/BaseOS) on one SD card. GBA, GB and GBC support remains.

This build combines upstream slot with:

- [Prashant Vaibhav's fork](https://github.com/pvaibhav/slot): home Wi-Fi,
  RetroAchievements, background labels, panel calibration and schedutil.
- [markbruno's slot-plus](https://github.com/markbruno/slot-plus): shaders and cheats.
- [flo's fork](https://github.com/flo333/slot): release of the audio device after silence.
- DroidSerif's commit [524ba7d](https://github.com/ygordreyer/slot/commit/524ba7d).
- Ygor Dreyer's own changes, listed below.

The upstream base is `a54157c`. Brandon's guide is at [slot-cfw.fyi](https://slot-cfw.fyi);
[CHANGELOG.md](CHANGELOG.md) contains upstream's notes. Slop has no GitHub releases.

## What Slop adds

### In game

- **GAME MENU.** Press `SELECT` + `MENU` to pause the game and choose
  **ACHIEVEMENTS**, **SETTINGS** or **LINK**.
- **Settings during play.** The six rows are **Fast Forward**, **Fast Forward Sound**,
  **Colour Correction**, **Shader**, **Show FPS** and **Rumble**. `B` returns to GAME MENU,
  `SELECT` + `MENU` closes to the game, and holding `MENU` ejects the cart.
- **Optional FPS display.** Enable **Show FPS** to display the measured frame rate.
  The overlay hides during achievement banners and the achievements viewer.
- **Rewind shader direction.** Hold `L2` for slot's rewind. Slop sends shaders a backwards
  `FrameDirection` only when a rewind state was restored; rewind is refused during links.
- **Cheat picker.** `SELECT` + `X` opens the paused game's cheat list, with choices saved
  back to its `.cht` file. See [Cheats](#cheats) for paths and save backups.

### Achievements

- **On-device account setup.** The shelf menu's **RetroAchievements** row opens an
  on/off toggle, username, masked password, sign-in and sign-out screen. A successful
  sign-in saves a token rather than the password.
- **Offline GBA achievements.** Definitions and unlocks are cached on the card, and pending
  unlocks sync when home Wi-Fi returns. Let the library cache finish online before playing
  offline; online play also updates RetroAchievements rich presence.
- **In-game viewer and sync status.** **ACHIEVEMENTS** lists the current game's unlocks
  and descriptions. Its status line distinguishes **ONLINE**, **SERVER BUSY, RETRYING**,
  **OFFLINE** and **SIGN IN AGAIN**, and reports unlocks waiting to sync.

### Display and shaders

- **Multi-pass presets and parameters.** Slop loads RetroArch `.glslp` presets under
  `Shaders/` and single `.glsl` files at that folder's top level. `A` on **Shader** opens
  parameter adjustment and reset controls; values persist per look in `Config/shader-params/`.
- **GBA shader pack.** The card includes CRT, LCD, colour, motion-blur and interpolation
  looks. `crt-lite` and `crt-lite-glow` provide scanlines and phosphor masks; `bevel` adds
  a pixel bevel, and the VBA-Color LCD presets combine colour processing with LCD masks.
- **Preset profiles and overlays.** Optional `slot_*` keys in a `.glslp` apply supported
  mGBA core options and a native 720x480 PNG overlay with opacity. Core overrides are restored
  when leaving the profile; overlay paths are relative to the preset, and no overlay PNG is shipped.
- **Exact integer scale.** GBA's 240x160 picture fills 720x480 at exactly 3x.
  **LCD** remains the default look; **Off** uses nearest-neighbour scaling without a shader effect.
- **Panel calibration and frame blending.** Hardware colour LUTs are selected by BaseOS's
  `BASEOS_TARGET`: `rgsp` (RG SP) and `rg34xxsp` (RG34XXSP). Other target strings get no LUT;
  mGBA uses Simple interframe blending with built-in looks, and custom shaders disable it
  unless their profile overrides it. gpSP frame mixing is enabled; **Colour Correction** defaults to Off.

The shipped presets appear in the **Shader** row under these names:

| Folder under `Shaders/` | Shader-row names |
| --- | --- |
| `crt/` | `crt-lite`, `crt-lite-glow`, `zfast-crt` |
| `handheld/` | `agb001-gba-color-motionblur`, `ags001-gba-color-motionblur`, `ags001`, `bevel`, `gba-color`, `lcd1x`, `vba-color-lcd1x`, `vba-color-lcd3x`, `zfast-lcd` |
| `interpolation/` | `pixellate-sharp-shimmerless` |
| `motionblur/` | `response-time` |

The VBA-Color recipes turn mGBA colour correction off to avoid applying it twice.
Shader source credits and licences are in [card/Shaders/SOURCES.txt](card/Shaders/SOURCES.txt).
The bevel addition follows Knulli's GBA shader choice, as credited in its commit.

`.slang` shaders are not supported. Single shaders can request linear filtering with
`#pragma slot_filter linear` and use `PrevTexture` for frame blending. A shader load failure
shows **Shader failed**, falls back to LCD and writes the error to the log.

### Audio

- **Audio profiles.** Preset `slot_audio_*` keys configure output rate, latency, gain,
  sinc resampling quality and an optional RetroArch `.dsp` chain. `Audio/ChipTuneEnhance.dsp`
  is included; leaving an audio profile or failing to load it restores baseline audio.
- **Silent device release.** The ALSA device is released after 3 seconds of silence
  to stop speaker buzzing, then reopened when sound returns.

### Shelf and library

- **Favorites.** `Y` toggles the selected cart. Favorites have a marker and a shelf
  before the platform shelves, with one ROM key per line in `Config/favorites.txt`.
- **Background labels.** Missing cartridge labels download in the background without
  replacing existing labels. Matching includes title aliases and fallback artwork from
  other regions, then regionless and World cartridge art.
- **12-hour clock.** **12-Hour Clock** changes the shelf, menu Date & Time and save-state
  timestamps to AM/PM. The clock-setting screen still uses 24-hour time.

### Wi-Fi and network

- **Home Wi-Fi and network screen.** **Wi-Fi Networks** scans, joins and forgets saved
  networks, with an on-screen keyboard for passwords and hidden SSIDs. It shows association,
  authentication, address acquisition, connection failures and signal in dBm or quality.
- **Link over home Wi-Fi.** Connected handhelds discover hosts running the same game on
  the home LAN. Without a home connection, slot's direct wireless link remains available;
  starting a link is refused while cheats are applied to the running game.

### Power and system

- **Held POWER shutdown.** Holding `POWER` starts shutdown directly. A writable CPU
  governor is set to `schedutil` when the card's frontend launcher starts.
- **Rumble and charging fallbacks.** Without an evdev force-feedback node, rumble uses
  the first writable `power_supply/*/moto` node. An online charger can report Charging when
  the battery says Not charging; the commit credits Knulli and MustardOS (muOS) for these behaviours.
- **Save flush and stay-awake flag.** `SIGTERM` and `SIGINT` pause emulation, flush the
  resume state and battery save, and sync before exit. `/run/slop-awake` prevents doze-timeout
  shutdown during remote testing; held POWER and critical battery still shut down.

### Developer tools

- **Remote device control.** [slopctl](tools/slopctl/README.md) uses SSH or USB adb for
  `status`, `wait`, `logs`, `shell`, `push`, `pull`, `deploy`, `restart`, `stop`, `start`,
  `awake`, `shot`, `buttons`, `press` and `seq`. Deploy checks hashes and replaces changed
  files atomically; `shot` captures the visible framebuffer.

```sh
tools/slopctl/slopctl --help
tools/slopctl/slopctl deploy --src dist-device --dry-run
tools/slopctl/slopctl shot screen.png --rotate 90
```

## Controls

For SELECT chords, hold `SELECT` first and press the other button promptly.
These include Slop's additions and the slot controls needed to use them.

| Where | Input | Action |
| --- | --- | --- |
| Shelf | Tap `MENU` | Open or close the full settings menu |
| Shelf | `Y` | Toggle the selected cart's favorite status |
| In game | `SELECT` + `MENU` | Open GAME MENU; close the menu, settings or achievements viewer back to play |
| In game or game settings | Hold `MENU` for 1 second | Eject the cart |
| In game | Double-tap `MENU` | Open the save-state picker |
| In game | `SELECT` + `R1` | Save a numbered state |
| In game | `SELECT` + `L1` | Load the newest numbered state |
| In game | Hold `L2` | Rewind; release to stop |
| In game | Hold `R2` | Fast forward; release to stop |
| In game | Double-tap `R2` | Latch fast forward; press and release again to stop |
| In game | `SELECT` + `X` | Open or close the cheat picker |
| Anywhere settings allow | `SELECT` + `Y` | Toggle Colour Correction; a preset can lock this setting |
| Anywhere | `SELECT` + `Up` / `Down` | Increase / decrease brightness |
| Anywhere | `SELECT` + `Left` / `Right` | Decrease / increase blue-light filtering |
| Anywhere | `VOL+` + `VOL-` | Toggle mute |
| Anywhere | Hold `POWER` for 1 second | Shut down |
| GAME MENU | `Up` / `Down`, `A` | Select and open ACHIEVEMENTS, SETTINGS or LINK |
| GAME MENU | `B` | Return to the game |
| Game settings | `Up` / `Down`, `Left` / `Right` | Select a row and change its value |
| Game settings | `B` | Return to GAME MENU |
| Shader row | `Left` / `Right`, `A` | Choose a look; open its parameters |
| Shader parameters | `Up` / `Down`, `Left` / `Right` | Select and adjust a parameter |
| Shader parameters | `X`, `Y` | Reset the selected parameter; reset all parameters |
| Shader parameters | `B` or tap `MENU` | Close parameters |
| Achievement list | `Up` / `Down`, `L1` / `R1` | Move one row; move one page |
| Achievement list | `A` | Open the selected achievement's description |
| Achievement description | `Up` / `Down` or `L1` / `R1` | Change description page |
| Achievements | `B` | Return from the description to the list; from the list to play |
| Cheat picker | `Up` / `Down`, `L1` / `R1` | Move one row; move one page |
| Cheat picker | `A`, `Left` / `Right` | Toggle selected cheat; turn it off / on |
| Cheat picker | `B`, tap `MENU` or `SELECT` + `X` | Close and apply choices |
| Wi-Fi Networks | `Y`, `X` | Rescan; request forgetting the selected saved network |
| Wi-Fi Networks | `Up` / `Down`, `A`, `B` | Select; connect or edit an entry (or enable Wi-Fi); go back |
| Account screen | `Up` / `Down`, `A`, `B` | Select; toggle or open the selected action; go back |
| On-screen keyboard | D-pad, `A`, `B` | Select a key; type; delete (cancel when empty) |
| On-screen keyboard | `X`, `Y` | Cycle lowercase, one-shot Shift and Caps; switch symbols |
| On-screen keyboard | `L1` / `R1`, `SELECT` | Move the text cursor; reveal or mask a password |
| On-screen keyboard | `START`, `L2` | Confirm; cancel editing |
| LINK picker | `Left` / `Right`, `SELECT`, `A`, `B` | Choose host / join; change link hardware; start; close |

## Cheats

Put a RetroArch cheat file at `Cheats/<platform>/<rom name>.cht`, with `GBA`, `GB` or
`GBC` as the platform folder and the ROM stem as the name. For example:

```toml
cheats = 1
cheat0_desc = "Infinite HP"
cheat0_code = "82003B4C+0063"
cheat0_enable = true
```

Cheats marked `cheatN_enable = true` turn on when the game starts. In the paused cheat
picker, changes are written back to the `.cht` file, so they are kept for next time.
Cheats are skipped in link cable sessions.

The first time cheats run, an existing battery save is copied to
`Saves/<platform>/<rom name>.sav.before-cheats` (or `.srm.before-cheats` for an `.srm` save).
Some cheats can damage saves, so keep that copy until you are sure.

## How to configure

Paths below are relative to the card root, normally `/mnt/sdcard` on the device.
The device distribution copies the two credential examples; favorites are created by `Y`.

| Path | Contents or setup |
| --- | --- |
| `Config/wifi.toml` | Copy `wifi.toml.example` and fill in `[[networks]]`, `ssid` and `password`, or use Wi-Fi Networks. For an open network use `security = "open"` without a password. Enable Home Wi-Fi in the shelf menu. |
| `Config/retroachievements.toml` | Copy `retroachievements.toml.example` and set `enabled` and `username` plus a legacy password or emulator login token, or sign in on-device. A Web API key is not a login token. |
| `Config/favorites.txt` | One card-relative ROM key per line, such as `Games/GBA/Example.gba`. This file has no shipped example. |
| `Config/shader-params/` | Saved parameter overrides for each shader or preset. |
| `Shaders/` | Shipped GBA pack, custom `.glslp` presets and their pass sources. Top-level single `.glsl` files are also selectable. |
| `Config/overlays/` | An optional location for your own 720x480 PNG. A preset in `Shaders/handheld/` can name it as `slot_overlay = "../../Config/overlays/overlay.png"`. |
| `Cheats/GBA/`, `Cheats/GB/`, `Cheats/GBC/` | RetroArch `.cht` files named after the ROM stem. |
| `Audio/` | DSP files, including `ChipTuneEnhance.dsp`. A preset in `Shaders/handheld/` can use `slot_audio_dsp = "../../Audio/ChipTuneEnhance.dsp"`. |
| `Saves/RetroAchievements/` | Per-account tokens, cached definitions and pending unlocks, managed by slot. |

## Building

The device target is aarch64 Linux. On a Mac with Colima, keep Cargo outputs in a Docker
volume because virtiofs mounts can drop executable bits from Cargo build scripts:

```sh
docker run --rm -v "$PWD":/src -v slop-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:1-bullseye sh -c 'cargo build --profile device -p slot -p slot-services --no-default-features --features slot/device'
```

Export both binaries for the packaging paths used by [taskfile.yml](taskfile.yml):

```sh
mkdir -p target-device/device
docker run --rm -v "$PWD":/src -v slop-target:/target -w /src rust:1-bullseye sh -c 'cp /target/device/slot /target/device/slot-services /src/target-device/device/'
```

Follow the `dist:device` copy steps in the taskfile to initialise `dist-device` and copy
both binaries, `card/System/`, the two config examples, `card/Shaders/` and `card/Audio/`.
Packaging also needs the pinned mGBA and gpSP cores, bundled wireless tools and their
licences and corresponding sources, prepared by `core:device`, `core:gpsp` and `network:tools`.
On the one-card BaseOS setup, copy the distribution contents to that card's content root.

## Known limits

- Hardcore achievements cannot count until RetroAchievements recognises slot's user agent.
  The site reports **Unknown Emulator**; softcore unlocks count.
- Achievements pause during link cable sessions. Cheats are skipped in those sessions,
  and a running game with applied cheats cannot start a link.
- The save-state picker lists numbered (timestamp-named) states only. It does not list `resume.state`.
- Achievements are implemented for GBA only; GB and GBC games remain playable.

## AI Disclosure

From upstream:

The Rust frontend was put together by Claude Opus. I reviewed everything that was
produced. All documentation is 100% free-range, meatbag prose.

The project is extremely low stakes. I wanted a bespoke frontend for my RG SP and thought
that something that evokes the feeling of using my GBA SP as a kid would be pretty neat.

Use it, don't use it, I don't care.

Figured I should share the end result of all the wasted water. ✌🏻

From pvaibhav's fork: its additions were written with AI assistance too.
