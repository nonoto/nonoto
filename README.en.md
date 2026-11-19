<!-- last_synced: 2026-11-18 -->

# Nonoto

> [日本語](./README.md) | English

Nonoto is a local-first document sharing service that keeps the source of truth on the user's own device.
It is currently at the stage of verifying that the architecture works.

This project is developed in Japanese.
Code comments, documentation, commit messages, issues, and pull requests are written in Japanese.
The Japanese [README.md](./README.md) is the canonical version, and this file is its translation.

## Layout

| Directory                     | Contents                                                                 |
| ----------------------------- | ------------------------------------------------------------------------ |
| `crates/nonoto-core`          | Page operations, the encrypted on-device store, and key storage          |
| `crates/nonoto-cli`           | The `nonoto` command (create, list, get, and update pages; outputs JSON) |
| `crates/nonoto-desktop`       | The GPUI desktop app                                                     |
| `spikes/store-comparison`     | Code comparing approaches to the encrypted store                         |
| `spikes/sqlite3mc-comparison` | Code comparing SQLCipher and sqlite3mc (outside the workspace)           |
| `spikes/crdt-comparison`      | Code comparing CRDT libraries for page bodies (yrs, Automerge, Loro)     |

## Developing in the dev container

Implementation and automated checks run in the dev container defined by `Dockerfile.dev`.

```sh
make fmt                             # Format
make lint                            # Lint with clippy
make test                            # Run the tests
make build-core-windows              # Build the core for Windows
make build-store-comparison-windows  # Build the encrypted store benchmark for Windows
make build-sqlite3mc-windows         # Build the sqlite3mc comparison for Windows
make build-crdt-comparison-windows   # Build the CRDT library comparison for Windows
make measure-sqlite3mc               # Benchmark sqlite3mc and SQLCipher under the same conditions
make measure-crdt-comparison         # Benchmark the CRDT library candidates on the same document
make screenshot-desktop              # Start the desktop app on a virtual display and save the screen to tmp/
make screenshot-editor               # Open the Markdown syntax sample in the editor and save light and dark screens to tmp/
make repeat-test                     # Repeat a test and keep the output of failed runs in tmp/repeat-test/
```

The `nonoto` command and the desktop app share environment variables for the data location and the key storage.

| Variable           | Meaning                                                                                                    |
| ------------------ | ---------------------------------------------------------------------------------------------------------- |
| `NONOTO_DATA_DIR`  | Data location. Defaults to the OS convention (`~/Library/Application Support/nonoto` on macOS)             |
| `NONOTO_KEY_STORE` | Key storage (`keychain` or `file`). Defaults to the keychain on macOS and Windows, and to a file elsewhere |

## Adding and upgrading dependencies

`cargo add` and `cargo update` do not select versions published less than 3 days ago (`global-min-publish-age` in `.cargo/config.toml`).
Versions already in `Cargo.lock` are still used for builds, even if they are less than 3 days old.

To use a newer version on purpose, such as a security fix, upgrade one crate at a time.
If the version you want does not satisfy the requirement in `Cargo.toml`, update that requirement first.

```sh
CARGO_RESOLVER_INCOMPATIBLE_PUBLISH_AGE=allow cargo update <crate> --precise <version>
```

Until that version is 3 days old, running `cargo update` without naming a crate can downgrade the dependency to a version that satisfies the requirement and is at least 3 days old.
If Cargo cannot find such a version, dependency resolution fails.
During that time, name the crate when upgrading other dependencies: `cargo update <crate>`.

Dependencies used in two or more places keep their versions in `[workspace.dependencies]` in the workspace `Cargo.toml`.
`make lint` fails when it finds an unused dependency in `[dependencies]` or `[build-dependencies]`.

## Repeating tests

For rare failures, `make repeat-test` repeats a test, one process per run, to measure how often it fails.
It keeps the output of failed runs and a summary of the conditions and results (`summary.txt`) in `tmp/repeat-test/<date-time>-<id>/`, and exits with status 1 if any run fails.

```sh
make repeat-test                                                    # Repeat the concurrent store-open test 300 times
make repeat-test RUNS=1000 NONOTO_OPEN_THREADS=64                   # Change the number of runs and concurrent threads
make repeat-test REPEAT_TEST="-p nonoto-core --lib store::sqlcipher"  # Repeat other tests, given as cargo test arguments
```

`make test` also repeats the concurrent store-open test 60 times, each run in a child process with 64 threads.
Change the number of runs with `NONOTO_OPEN_PROCESSES` (for example, `make test NONOTO_OPEN_PROCESSES=10`).

## Benchmarking the encrypted store

This compares the encrypted store approaches and the full-text index variants of approach A (substring scan, trigram, bigram, and unigram) on the same pages.

```sh
cargo run --release --locked -p store-comparison -- --pages 10000
```

| Argument  | Meaning                                                                                   |
| --------- | ----------------------------------------------------------------------------------------- |
| `--pages` | Number of pages. Defaults to 10000                                                        |
| `--dir`   | Directory for the databases. Defaults to a temporary directory that is removed at the end |

The results are written to standard output as Markdown.
The final "必須の条件" (required conditions) table shows whether each variant meets each condition, and the "計測の値" (measurements) table shows the main values.
The pages are generated from a fixed random seed, so every run uses the same content.
Pages are saved one at a time, so a run takes about 7 minutes for 10,000 pages and about 30 minutes for 50,000 pages in the dev container.

### Comparing encryption implementations

`spikes/sqlite3mc-comparison` compares an encryption implementation that does not depend on OpenSSL (SQLite3 Multiple Ciphers, sqlite3mc below) with SQLCipher, using the same store code.
It lives outside the workspace, because `rusqlite` cannot link to both the workspace's SQLCipher and sqlite3mc.
Run cargo in the crate's directory, so that the crate's `.cargo/config.toml` is read.

The sqlite3mc source is not kept in the repository; `make fetch-sqlite3mc` downloads it to `target/sqlite3mc/` after verifying its checksum.
`make lint`, `make test`, `make build-sqlite3mc-windows`, and `make measure-sqlite3mc` download it first if needed.

```sh
make measure-sqlite3mc                     # Benchmark with 10,000 pages
make measure-sqlite3mc MEASURE_PAGES=1000  # Benchmark with a different number of pages
```

This builds the sqlite3mc version and the SQLCipher version (the `sqlcipher` feature), measures both with the same number of pages, and shows saving all pages, startup, memory added at startup, saving and updating one page, and the file size in one table.
The SQLCipher version builds a bundled OpenSSL, so its first build takes several minutes.

## Benchmarking the CRDT libraries

Benchmarks the CRDT library candidates for page bodies (yrs, Automerge, Loro) on a document built from the same input sequence.

```sh
make measure-crdt-comparison             # Benchmark with a 5,000-line document
make measure-crdt-comparison LINES=1000  # Benchmark with a different number of lines
```

The document is built from an empty state by applying an input sequence (typing one character at a time, IME commits, deletions for retyping, and pastes).
The input sequence is generated from a random number generator with a fixed seed, so every run produces the same content.
The results are written to standard output as Markdown, with one table listing the time to apply one edit and to load, the sizes of updates, snapshots, and version identifiers, and the memory added by loading.
The final "required conditions" table shows whether each candidate met the conditions (applying one edit within 5 ms and loading within 100 ms).
In the dev container, 5,000 lines take about one minute.

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

Choose a data directory separate from your regular data, then add 10,000 short pages for completion searches and a 5,000-line page.

```sh
export NONOTO_DATA_DIR=/tmp/nonoto-editor-check

# Create 10,000 short pages in one process (change PAGES to adjust the count)
make seed-pages PAGES=10000

# Create one page with thousands of Markdown lines (change LINES to adjust the count)
make seed-large-page LINES=5000

# Create a target for wiki links in the rendered view
cargo run --release -p nonoto-cli -- create --title はじめに --body '# 見出し'
```

`make seed-pages` retrieves the key and opens the database once, then adds the requested number of pages through the existing core API.
It accepts `NONOTO_KEY_STORE` and uses the same default key storage as the app when the variable is omitted.
If the app is open while you add data, restart it to see the new pages in the list.

## Checking the editor

Run `cargo run --release -p nonoto-desktop` in the same terminal used to add the data, so the app opens the same data directory.

### A large page

Open "大きなページ (5000行)", press "編集" (Edit), and keep typing or scroll up and down quickly to see whether the editor stutters.

### Wiki link completion

Typing `[[` in the body shows title candidates.
Continue in Japanese, such as `[[は`, and check that the text being composed stays intact and that the candidates narrow after you confirm it.
Press Enter to choose a candidate.

### Completion with 10,000 pages

Type `[[検証用ページ` in Japanese in the body and check that candidates appear.
Continue with text such as ` 000` to narrow them, use the arrow keys to move through the candidates, and press Enter to choose one.
Check that typing and candidate updates remain responsive with at least 10,000 pages in the data directory.

### List continuation

Pressing Enter at the end of a `- 項目` line adds `- ` on the next line.
Pressing Enter again on the empty item removes the marker.
Check that the Enter that confirms the IME composition does not continue the list.

### Indentation

Select several lines and press Tab and Shift-Tab to check that the indentation of the selected lines changes.

### Built-in features

Typing `(` adds `)`.
Check that Command-F searches the body, and that Command-Z and Command-Shift-Z undo and redo.

### Wiki links in the rendered view

Save a body with `[[はじめに]]` and `[[無いページ]]`, then click `はじめに` in the rendered view to move to the "はじめに" page.
Check that `[[無いページ]]` is not a link, appears as written with its brackets, and does nothing when clicked.

### VoiceOver

Start VoiceOver with Command-F5, move to the body input while editing, and listen to whether the body is read aloud.

## Cleaning up

The location set with `NONOTO_DATA_DIR` can be deleted as a whole directory.
Deleting the keychain item makes data encrypted with that key unreadable.
Delete it only when you also delete the data location.
