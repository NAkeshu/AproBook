# AproBook

> **Your books, organized and ready — right when you want to read.**

**本地优先的 EPUB、PDF、无 DRM MOBI 与 TXT 书库和阅读器。**

AproBook is a personal library for collecting, organizing, and reading your own digital books. It keeps books and metadata on your machine, remembers where you stopped, and avoids accounts, stores, and cloud services. [Why “AproBook”?](docs/BRAND.md)

![AproBook icon](crates/ebook-desktop/assets/aprobook-icon.svg)

## Platform status

**v0.5.0 is developed and verified only for Apple Silicon macOS 13 or later.** Windows, Linux, and Intel Mac are not tested or supported in this release; the Rust workspace and Cargo alone do not guarantee that those platforms build or run correctly. Cross-platform adaptation is outside this version's scope. The macOS bundle includes an arm64 PDFium dynamic library.

## What it does

- Import EPUB, PDF, DRM-free MOBI, and TXT files by picker, folder scan, or drag-and-drop. Originals stay untouched; managed copies live inside the chosen library.
- Edit book metadata, cover, authors, tags, folders, favorite flag, rating, and reading status. Search, filter, sort, and manage multiple selected books at once. Drag a rectangle across the shelf to select books.
- Read with a table of contents, bookmarks, keyboard navigation, themes and text layout controls. PDF supports page navigation, zoom, fit-width, and fit-page.
- Resume from saved positions, reveal a managed file in Finder, or remove its managed copy through macOS Trash.

Only DRM-free files are supported. AproBook does not bypass DRM or PDF passwords.

## Build and run on macOS

Install a current Rust toolchain and Apple's command-line developer tools. From the project root:

~~~sh
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p ebook-desktop
~~~

To create a standalone Apple Silicon app:

~~~sh
zsh scripts/bundle-macos.sh
open dist/AproBook-v0.5.0.app
~~~

The script refuses to overwrite an existing v0.5.0 bundle; move that bundle aside before rebuilding. This is a locally ad-hoc-signed development build, **not** a notarized public download.

## Libraries, privacy, and upgrading

On first launch, create a library in a new or empty folder. “Open existing library” checks its marker, SQLite schema, and items/ directory before opening it. Each library contains library.sqlite3, .theebook-library.json, and managed books under items/. These internal names are intentionally unchanged so libraries made by theEBookViewer v0.0.0–v0.4.0 remain readable. Do not open one library concurrently on multiple devices or sync its live SQLite database through a cloud drive.

The app's settings now live in the AproBook user configuration directory. On first run, if the new settings do not exist, valid settings from the former theEBookViewer directory are **copied**, including the recent-library path; the old files remain untouched. Existing AproBook settings take precedence. Regenerable TXT caches move to an AproBook cache directory; they may be rebuilt.

Settings are available from the welcome screen, sidebar, reader toolbar, and the macOS **⌘,** shortcut. The recent-library entry can be cleared without deleting books. Leaving a library releases its lock and returns to the welcome screen.

## Current limitations

- No DRM bypass, OCR, annotations, full-text search, cloud sync, or simultaneous multi-device access.
- EPUB publisher styling and uncommon MOBI files may not render perfectly. HTML-book positions are approximate after layout changes.
- TXT encoding and chapter detection are heuristic; changing encoding can shift bookmarks and progress.
- PDF currently shows one page at a time, not continuous scrolling. First-page PDF cover extraction is best-effort.
- Automated tests use synthetic format fixtures; broad compatibility with real-world publisher files still needs manual validation.

The macOS bundle includes PDFium branch chromium/7350 from [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries). The archive SHA-256 is 279d69972954968a61ffab26ac60dc1b80066133e8f5e82cb3ee47d1df5cd28e. PDFium and third-party notices are in crates/ebook-desktop/assets/pdfium/; this project's code is [MIT-licensed](LICENSE). See the [changelog](CHANGELOG.md) for release history.
