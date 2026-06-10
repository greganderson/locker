# locker

A fake lock screen. Run it and your machine *looks* locked: a fullscreen image
(or a built-in clock + padlock screen) covers every monitor, the cursor is
hidden, and nothing reacts to input. Type the unlock code — no input box, no
echo, no feedback — and it disappears.

**This is a facade, not security software.** It does not stop anyone from
switching VTs, sysrq-ing, or power-cycling the machine.

## Build & install

```sh
cargo build --release
install -Dm755 target/release/locker ~/.local/bin/locker
```

## Usage

```sh
locker
```

- **Unlock:** type the code (default `unlock`). Mistyped characters don't
  matter — only the most recently typed characters are compared, so just type
  the code again.
- **Ctrl+Z:** suspends locker like any terminal job and drops you back at your
  shell; `fg` brings the lock screen back. (Run the installed binary directly —
  under `cargo run`, job control gets murkier.)
- Close requests from the window manager are ignored; kill it with
  `kill <pid>` from another terminal if you ever lose the code.

## Configuration: `~/.lockerrc`

```ini
# the unlock code (default: unlock)
code = letmein

# fullscreen background image; scaled to fill, center-cropped.
# if unset or unloadable, the built-in lock screen is used.
image = ~/Pictures/fake-lock.png
```

See `lockerrc.example`. A handy trick: screenshot your real lock screen once
and point `image` at it.

## Built-in screen

If no image is configured (or it fails to load), locker renders its own lock
screen: dark gradient, padlock icon, live clock and date, using a system font
found via `fc-match` (text is skipped if no font is found).
