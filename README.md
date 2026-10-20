# Slipstream Reader

A reading mode for EPUB books: full screen, nothing else on it, in your desktop's colours. It
began as Slipstream's reading mode (Super+R) and now stands on its own; on a Slipstream desktop,
Super+R still opens it.

- **Your library**: every book in `~/Dropbox/Books`, by its cover. Ctrl+O shows it; typing finds
  a book.
- **Reading**: ← → or h l for chapters, j k to scroll, Ctrl+= and Ctrl+- for size. **Ctrl+I**
  brings out the chapters and notes on the left, and the colours and font on the right.
- **Fonts**: the book's own, or Literata, Source Serif, EB Garamond or Atkinson Hyperlegible,
  built in. The panel says which font the book asks for and whether it carries it.
- **With a Kobo**: books your Kobo gets from a [Kobo-Sync](../NAGI.MEAT.APP/Kobo-Sync) server
  open here as the Kobo's own copy, with its highlights, notes and bookmarks, and the place you
  were. Select text and press Ctrl+H to highlight, Ctrl+Shift+H to add a note; Ctrl+D bookmarks
  the page. It all goes back to the Kobo.

## Installing

Needs GTK 4.14 or newer, WebKitGTK 6.0 (2.42 or newer) and Rust 1.92 or newer
([rustup](https://rustup.rs)).

```sh
# Ubuntu 24.04 / Debian 13
sudo apt install build-essential libgtk-4-dev libwebkitgtk-6.0-dev
# Fedora 40+
sudo dnf install gtk4-devel webkitgtk6.0-devel
# Arch
sudo pacman -S gtk4 webkitgtk-6.0

scripts/install
```

`scripts/install` builds it and puts it in `~/.local/bin`, with its menu entry.

## The Kobo sync

Put the server and its token in `~/.config/slipstream/reader-sync.json`, readable only by you:

```json
{"server": "https://books.bitcointop.club", "token": "…"}
```

Without that file the reader works as before, with no sync.

## On Windows, through WSL

Windows 11 runs Linux apps in WSL in their own windows.

1. In PowerShell: `wsl --install -d Ubuntu-24.04`, then open Ubuntu.
2. Install rustup and the packages above, then `scripts/install`.
3. Your books: `ln -s /mnt/c/Users/<you>/Dropbox ~/Dropbox`, so `~/Dropbox/Books` is the Windows
   Dropbox folder.
4. The sync file, as above.
5. Start it as `slipstream-reader`, or from the Start menu: WSL adds **Reading Mode** there.

If pages flicker or come up blank, start it with `WEBKIT_DISABLE_COMPOSITING_MODE=1`. The
Windows key is Windows' own, which is why the panels are on Ctrl+I.

## What it keeps where

| | |
|---|---|
| `~/.local/state/slipstream/reader.json` | where you are in each book, size, colours, font |
| `~/.local/state/slipstream/reader-outbox.json` | notes waiting for the server |
| `~/.cache/slipstream/reader/` | the Kobo's copies of books |
| `~/.config/slipstream/reader-sync.json` | the sync server and token |

The colours follow the Omarchy theme when there is one (Slipstream's "follow the Omarchy theme"
setting is respected), else its own dark or light.

## Licence

GPL-3.0-or-later (`COPYING`). The fonts are under the SIL Open Font License, each with its licence
in `assets/fonts/`.
