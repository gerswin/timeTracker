Icon assets

macOS:
- Place status bar template icon at `assets/icons/macos/iconTemplate.png` (monochrome, transparent background).
- It will be copied to the app bundle at `Contents/Resources/iconTemplate.png` by `scripts/macos_pack.sh`.

Windows:
- Place tray icon at `assets/icons/windows/icon.ico` (32x32 or multi-size).
- The Windows UI loads this icon (embedded at build time). Set env `RIPOR_NO_EMBED_ICON=1` to skip embedding (uses placeholder).
