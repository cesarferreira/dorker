# Install

Requires [Rust](https://rustup.rs) **1.85+** and `~/.cargo/bin` on your `PATH`.

```bash
cargo install dorker
```

Verify:

```bash
dorker --help
```

## Build from source

For development, or for changes that are not on crates.io yet:

```bash
git clone https://github.com/cesarferreira/dorker.git
cd dorker
cargo install --path . --locked
```

Or, with the Makefile:

```bash
make install-release
```

A debug install compiles faster and produces a larger binary:

```bash
make install
```

Run without installing:

```bash
make build-release
./target/release/dorker
```

dorker also needs the Android SDK (`emulator`, `platform-tools`) and a system image. [Quick start](quick-start.md) checks the host and installs an image.

Next: [Quick start →](quick-start.md)
