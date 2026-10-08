# slot.

A fork of [slot](https://github.com/BrandonKowalski/slot) by Brandon Kowalski. slot is a
bespoke, Game Boy-centric frontend for the Anbernic RG SP. Brandon wrote slot and its guide.
This fork follows upstream and adds the features listed below.

Has support for GBA, GBC, and GB titles only.

The user guide is upstream's, at [slot-cfw.fyi](https://slot-cfw.fyi). Upstream's release
notes are in the [changelog](CHANGELOG.md).

## What this fork adds

- **LCD ghosting / interframe blending** is enabled by default. It's subtle.
- **Colour calibration** for the RG SP and RG34XXSP panels, based on an accurate 3x1D LUT
  measured by me. Both white point and gamma curve are corrected. Currently no other firmware
  has this!
- Holding POWER **shuts down** directly, without showing a menu. I did not find value in a
  restart option.
- **Home Wi-Fi.** Slot can now connect to your home Wi-Fi. Just set up your Wi-Fi credentials
  in `Config/wifi.toml` and then turn HOME WI-FI on from the menu. Everything happens
  automatically from then onwards. Most of the features below depend on this.
- **Hassle-free cart label scraping.** Just throw your ROMs on the SD card, and slot downloads
  missing labels in the background. It will not replace a label you added yourself, though.
  No config is needed for this feature.
- **RetroAchievements.** You can earn softcore GBA achievements, online or offline. Put your
  account in `Config/retroachievements.toml` for this to work. All your games' achievements
  will be cached if Wi-Fi is on, so once the sync is done, you can go out and play without
  losing your achievements. They'll sync automatically when you're back home. When you're
  playing at home with Wi-Fi on, your achievements will sync in real time with rich presence.
- **Linking over home Wi-Fi.** When at home and connected to Wi-Fi, two handhelds can link
  using that instead of having to create ad hoc networks. Outside of home, you can continue
  using slot's normal method. This works seamlessly, no config needed.

## How to configure

The release includes an `.example` copy of each config file in `Config/`. Remove `.example`
from the name, and fill in your details. You only need to configure your Wi-Fi and
RetroAchievements credentials. Everything else is automatic.

## Versions

A release of this fork uses upstream's version with a suffix. `v1.4.0-pvaibhav.1` is the
first release built on upstream 1.4.0, and `v1.4.0-pvaibhav.2` would be the second.

## Shaders and cheats (fork additions)

This fork adds two things to slot: shaders for the game screen, and cheats.

### Shaders

Put single-pass RetroArch `.glsl` shaders in `Shaders/` on the card. Tap `MENU` on the
carousel and use the **Shader** row to pick one with Left and Right. It is saved like every
other setting and applies to every game.

- **LCD** is slot's own look and stays the default. **Off** is the plain picture at 3x.
- Only single `.glsl` files work, not `.glslp` presets or `.slang` shaders.
- Shader parameters run at their defaults.
- Add the line `#pragma slot_filter linear` to a shader that expects smooth filtering, such
  as sharp-bilinear. RetroArch ignores it.
- A shader can read the previous frame through `PrevTexture`, for frame blending.
- If a shader fails to compile, slot shows "Shader failed", goes back to LCD, and writes the
  compiler's error to its log.

`examples/shaders/blend-grid.glsl` is a small example to start from.

### Cheats

Put a RetroArch cheat file at `Cheats/<platform>/<rom name>.cht`, with `GBA`, `GB`, or
`GBC` as the platform folder and the ROM stem as the name. Files from the libretro cheat
database work as they are:

```
cheats = 1
cheat0_desc = "Infinite HP"
cheat0_code = "82003B4C+0063"
cheat0_enable = true
```

Cheats marked `cheatN_enable = true` turn on when the game starts.

To choose cheats on the device, press `SELECT` + `X` in a game. The game pauses and a list of
that game's cheats appears:

| Input          | Action                                   |
| -------------- | ---------------------------------------- |
| `Up` `Down`    | Move through the list                    |
| `L1` `R1`      | Jump a page                              |
| `A`            | Turn the highlighted cheat on or off     |
| `Left` `Right` | Turn it off / on                         |
| `B`            | Close the list and go back to the game   |

Your choices are written back into the `.cht` file, so they're kept for next time and the file
still works in RetroArch. Cheats are skipped in link cable sessions.

The first time cheats run on a game, its battery save is copied to
`Saves/<platform>/<rom name>.sav.before-cheats`. Some cheats can break a save for good, so keep that
copy until you're sure.

mGBA supports GameShark, Action Replay and CodeBreaker codes. If a code does nothing on gpSP,
try the game on mGBA.

## 12-hour clock

Tap `MENU` on the carousel and turn on **12-Hour Clock** to show times as 3:07 PM rather
than 15:07: on the shelf, in the menu's Date & Time, and on older save states. The screen for
setting the clock still uses 24-hour time.

## AI Disclosure

From upstream:

The Rust frontend was put together by Claude Opus. I reviewed everything that was
produced. All documentation is 100% free-range, meatbag prose.

The project is extremely low stakes. I wanted a bespoke frontend for my RG SP and thought
that something that evokes the feeling of using my GBA SP as a kid would be pretty neat.

Use it, don't use it, I don't care.

Figured I should share the end result of all the wasted water. ✌🏻

This fork's additions were written with AI assistance too.
