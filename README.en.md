<!-- last_synced: 2026-11-13 -->

# Nonoto

> [日本語](./README.md) | English

Nonoto is a local-first document sharing service that keeps the source of truth on the user's own device.
It is currently at the stage of verifying that the architecture works.

This project is developed in Japanese.
Code comments, documentation, commit messages, issues, and pull requests are written in Japanese.
The Japanese [README.md](./README.md) is the canonical version, and this file is its translation.

## Layout

| Directory                 | Contents                                                                    |
| ------------------------- | --------------------------------------------------------------------------- |
| `crates/nonoto-core`      | Page operations, the encrypted on-device store, and key storage             |
| `crates/nonoto-cli`       | The `nonoto` command (create, list, get, and update pages; outputs JSON)    |
| `crates/nonoto-desktop`   | The GPUI desktop app                                                        |
| `spikes/store-comparison` | Code comparing approaches to the encrypted store                            |

## Developing in the dev container

Implementation and automated checks run in the dev container defined by `Dockerfile.dev`.

```sh
make fmt                 # Format
make lint                # Lint with clippy
make test                # Run the tests
make build-core-windows  # Build the core for Windows
make screenshot-desktop  # Start the desktop app on a virtual display and save the screen to tmp/
```

The `nonoto` command and the desktop app share environment variables for the data location and the key storage.

| Variable           | Meaning                                                                                                          |
| ------------------ | ---------------------------------------------------------------------------------------------------------------- |
| `NONOTO_DATA_DIR`  | Data location. Defaults to the OS convention (`~/Library/Application Support/nonoto` on macOS)                   |
| `NONOTO_KEY_STORE` | Key storage (`keychain` or `file`). Defaults to the keychain on macOS and Windows, and to a file elsewhere      |

## Checking on a Mac

Things that can only be checked on a Mac, such as Japanese input (IME), VoiceOver, and the keychain, are checked on a local Mac.

### Setup

1. Install Xcode from the App Store, launch it once, and accept the license
2. Select that Xcode

   ```sh
   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
   ```

3. Install [mise](https://mise.jdx.dev/) and, at the repository root, install the Rust version from `mise.toml`

   ```sh
   mise install rust
   ```

If `xcrun -f metal` prints a path, the desktop app can be built.
If the build reports that `metal` cannot be found, install it with `xcodebuild -downloadComponent MetalToolchain`.

### Fetching a branch

Fetch a branch made in the dev container by adding the development repository as a remote.

```sh
git remote add dev <URL of the development repository>
git fetch dev
git switch --track dev/<branch name>
```

### Building and running

```sh
cargo run --release -p nonoto-desktop
```

The first build takes over ten minutes.
On first launch, the key is saved in the keychain as an item named `nonoto`.

To try it apart from your usual data, set the data location.

```sh
export NONOTO_DATA_DIR=/tmp/nonoto-mac
cargo run --release -p nonoto-desktop
```

### Adding test data

Add data with the same environment variables you run the app with.

```sh
# Create one Markdown page with thousands of lines (change the count with LINES)
make seed-large-page LINES=5000

# Create a short page
cargo run --release -p nonoto-cli -- create --title はじめに --body '# 見出し'
```

If the app is open while you add data, restart it to see the new pages in the list.

### Cleaning up

The location set with `NONOTO_DATA_DIR` can be deleted as a whole directory.
Deleting the keychain item makes data encrypted with that key unreadable.
Delete it only when you also delete the data location.
