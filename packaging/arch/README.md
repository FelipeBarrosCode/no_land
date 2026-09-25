# Arch Linux / CachyOS / Manjaro Installation

This directory contains the `PKGBUILD` for Arch Linux and Arch-based distributions (CachyOS, EndeavourOS, Manjaro).

## Installing locally

To build and install the package locally:

```bash
cd packaging/arch
makepkg -si
```

This will automatically download the binary release, extract the full resource bundle (including `/usr/lib/Noland Connect/` and managed binaries), set permissions, and install it through `pacman`.

## Publishing to AUR

To publish `noland-connect-bin` to the Arch User Repository:

1. Clone your AUR repository:
   ```bash
   git clone ssh://aur@aur.archlinux.org/noland-connect-bin.git
   ```
2. Copy this `PKGBUILD` into the repository.
3. Generate `.SRCINFO`:
   ```bash
   makepkg --printsrcinfo > .SRCINFO
   ```
4. Commit and push:
   ```bash
   git add PKGBUILD .SRCINFO
   git commit -m "chore: release 0.1.8"
   git push origin master
   ```
