# Photo Sorter

Desktop app for sorting photos by hand with the keyboard. Open a folder of
photos, bind a key to each destination folder, then press a key to move the
current photo there. The next photo is shown straight away.

Supported formats: JPEG, PNG, WebP, BMP, TIFF, GIF.

## Usage

1. **Open source folder**: the folder holding the photos to sort.
2. **+ Add target folder**: once per destination.
3. Click the `--` button next to a target folder, then press the key to bind
   to it.
4. Press the bound keys to sort.

Clicking a target folder's name also moves the current photo there, and `x`
removes the folder from the list.

### Keys

| Key | Action |
| --- | --- |
| Bound key | Move the current photo to that folder |
| `←` / `→` | Previous / next photo |
| Wheel (or `Ctrl`+wheel) | Zoom around the pointer |
| Drag | Pan when zoomed |
| `Space` | Toggle between fit-to-window and 1:1 |
| `Esc` | Leave zoom, or cancel a key binding |
| `Ctrl+Z` | Undo the last move |

### Contact sheet

`G` switches between the single photo and a grid of thumbnails, to sort
several photos at once.

| Input | Action |
| --- | --- |
| Click / `Ctrl`+click / `Shift`+click | Select one / add or remove / select a range |
| `Space` | Select or deselect the current photo |
| `Ctrl+A` / `Esc` | Select all / clear the selection |
| Bound key | Move the selected photos (or the current one if none) |
| Double-click | Open the photo in single view |
| `Ctrl+Z` | Undo the last move, the whole batch at once |

`←`, `→`, `G`, `Z`, `Space` and `Esc` are reserved and cannot be bound to a
folder.

### Notes

- Photos are moved, not copied. Nothing is deleted.
- An existing file is never overwritten: if the name is taken, the photo is
  saved as `IMG_0042 (1).jpg`.
- Target folders, shortcuts and the last source folder are remembered between
  runs.

## Building

Requires [Rust](https://rustup.rs/). On Windows you also need the MSVC
linker ("Desktop development with C++" in Visual Studio Build Tools).

```sh
git clone https://github.com/Alexismill/rust-photo-sorter.git
cd rust-photo-sorter
cargo run --release
```

On Windows, `.\package.ps1` builds a standalone `.exe` and zips it into
`dist\`.

Development:

```sh
cargo test
cargo clippy
```

## License

MIT, see [LICENSE](LICENSE).
