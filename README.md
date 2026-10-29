# LG webOS Remote for COSMIC

A separate Linux COSMIC panel applet for controlling LG webOS TVs on your local network. This is an early port of the LG remote in this repository; it does not change either macOS app.

## Current scope

- COSMIC panel applet with a bundled aerial-TV icon and separate Control, Apps, Inputs, Pointer, Picture, and Setup tabs
- SSDP discovery of LG webOS TVs on the local network
- Encrypted webOS pairing on port 3001
- First-use SHA-256 certificate review; a changed saved certificate is rejected
- Pairing keys and certificate pins stored with GNOME Secret Service through `secret-tool`
- Icon-based directional, Home, Back, OK, volume, channel, playback, keyboard-text, and power-off controls
- Mouse-like pointer movement and click through the TV's certificate-pinned pointer socket
- Read and adjust supported picture settings, then apply them explicitly
- Search installed apps before launching them
- Query installed apps and inputs, launch apps, and switch inputs

The LG TV must be on the same local network. During first pairing, approve the fingerprint in the popup and then accept the pairing request on the TV. The TLS certificate is inspected before the app sends pairing or control data. Credentials are not saved to app configuration.

If the TV reports `401 Unauthorized` for apps or pointer control after updating the applet, open **Setup → Forget saved pairing**, reconnect, review the certificate, and accept the new pairing request on the TV. The updated registration explicitly requests the app-list, keyboard, and pointer permissions. This reset only removes this applet's local credentials; it does not send a TV control command.

Compatibility is not yet verified on a TV from Linux. The original controller was tested with one webOS 5.0 TV; this applet needs separate testing before additional versions can be claimed.

## Arch Linux package

Build and install a local package from this checkout:

```sh
makepkg -si
```

The package installs the applet binary, desktop entry, and icon under `/usr`. Its runtime requirements are OpenSSL and GNOME Secret Service (`libsecret`), including the `secret-tool` command. A compatible COSMIC desktop session is required to use the applet. To build manually without packaging, use the steps below.

## CachyOS / Arch manual build

Install the base build dependencies and Rust:

```sh
sudo pacman -S --needed base-devel git rust cargo just pkgconf openssl libsecret
```

Build from this directory:

```sh
cargo build --release
```

Install the binary and COSMIC applet desktop entry into the user profile:

```sh
install -Dm755 target/release/lg-webos-cosmic ~/.local/bin/lg-webos-cosmic
install -Dm644 resources/io.github.tvmenubar.LGWebOSRemote.desktop ~/.local/share/applications/io.github.tvmenubar.LGWebOSRemote.desktop
install -Dm644 resources/io.github.tvmenubar.LGWebOSRemote.svg ~/.local/share/icons/hicolor/scalable/apps/io.github.tvmenubar.LGWebOSRemote.svg
```

Restart the COSMIC panel/session, then add **LG webOS Remote** from the panel applet picker. If the picker does not list it, confirm that your COSMIC build supports third-party applets and that the desktop file is present under `~/.local/share/applications`.

Your Secret Service keyring must be available and unlocked. `secret-tool` stores the TV certificate pin and pairing key separately for each TV address.

## Debian package

The `debian/` directory contains Debian packaging metadata. From a Debian build environment with the package build dependencies installed, build a binary package with:

```sh
dpkg-buildpackage -us -uc -b
```

The resulting `.deb` installs the applet binary, desktop entry, and icon under `/usr`. Runtime requirements include `libsecret-tools` for `secret-tool`, OpenSSL libraries, and a compatible COSMIC desktop session. Rust dependencies, including `libcosmic`, must be available to Cargo while building. The Git based `libcosmic` dependency means fully offline/reproducible Debian builds will need a vendored dependency source prepared for the target release.

## GitHub release packages

Publishing a GitHub Release with a `v`-prefixed tag matching the version in `Cargo.toml` (for example, `v0.1.0`) triggers `.github/workflows/release-packages.yml`. It builds the Debian package in Debian 13 and an Arch package, then attaches both files to the release. Cargo uses the committed `Cargo.lock`, including the pinned `libcosmic` Git revision.

## Current limitations

Wake-on-LAN, TV-provided app icons, screen-time history, and media casting are not implemented yet. You can discover TVs on the local network or enter an IPv4 address manually. Picture adjustments depend on TV permissions. The libcosmic dependency tracks its upstream Git repository, whose APIs can change; compile against the COSMIC version installed on your system.
