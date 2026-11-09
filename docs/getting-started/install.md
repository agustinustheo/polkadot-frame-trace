# Install

You need:

- Docker, to run Jaeger. Any other trace viewer that accepts [OTLP](../reference/glossary.md#otlp) over HTTP also works.
- The RPC URL of a node of the chain you want to trace. It must serve `state_getReadProof`. See [Choose an endpoint](../guides/trace-a-block.md#choose-an-endpoint) before tracing many blocks.

[What polkadot-frame-trace supports](../reference/support.md) lists the chains and nodes it works with.
Most people install a release binary. You can also [build from source](#build-from-source).

## Install a release binary

Each release has an archive for these systems:

| System | Target in the file name |
| --- | --- |
| Linux, x86_64 | `x86_64-unknown-linux-gnu` |
| Linux, ARM64 | `aarch64-unknown-linux-gnu` |
| macOS, Apple silicon | `aarch64-apple-darwin` |
| macOS, Intel | `x86_64-apple-darwin` |

The Linux binaries need glibc 2.35 or newer, as in Ubuntu 22.04.
HTTPS endpoints are checked against the system's certificate store; on minimal Linux systems and containers, install `ca-certificates`.

Find the newest version on the [releases page](https://github.com/agustinustheo/polkadot-frame-trace/releases/latest).
Download it with your version and target:

```sh
VERSION=0.1.0
TARGET=x86_64-unknown-linux-gnu
curl -fsSLO "https://github.com/agustinustheo/polkadot-frame-trace/releases/download/v$VERSION/polkadot-frame-trace-$VERSION-$TARGET.tar.gz"
curl -fsSLO "https://github.com/agustinustheo/polkadot-frame-trace/releases/download/v$VERSION/SHA256SUMS"
```

If the version or target is wrong, `curl` stops with an error:

```text
curl: (56) The requested URL returned error: 404
```

Check the archive against `SHA256SUMS`. On macOS, use `shasum -a 256` in place of `sha256sum`:

```sh
sha256sum --check --ignore-missing SHA256SUMS
```

You should see `OK` after the archive's name. On macOS with Apple silicon:

```text
polkadot-frame-trace-0.1.0-aarch64-apple-darwin.tar.gz: OK
```

Unpack the archive and copy `polkadot-frame-trace` to `~/.local/bin`:

```sh
tar -xzf "polkadot-frame-trace-$VERSION-$TARGET.tar.gz"
mkdir -p ~/.local/bin
cp "polkadot-frame-trace-$VERSION-$TARGET/polkadot-frame-trace" ~/.local/bin/
```

If `~/.local/bin` is not on your `PATH`, add this line to your shell's startup file, such as `~/.bashrc` or `~/.zshrc`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Nightly binary

The [Nightly](https://github.com/agustinustheo/polkadot-frame-trace/releases/tag/nightly) pre-release is rebuilt each night that the code changed.

> **Warning:** Nightly builds pass the automated checks before they are published, but they may change the command line or the JSON output without notice.

Download the nightly archive:

```sh
TARGET=x86_64-unknown-linux-gnu
curl -fsSLO "https://github.com/agustinustheo/polkadot-frame-trace/releases/download/nightly/polkadot-frame-trace-nightly-$TARGET.tar.gz"
curl -fsSLO "https://github.com/agustinustheo/polkadot-frame-trace/releases/download/nightly/SHA256SUMS"
```

Set `VERSION=nightly` before you repeat the checksum, unpack and copy steps.

Its `--version` shows the build date and the first seven characters of the commit, for example `polkadot-frame-trace 0.1.0-nightly.20261106+<commit>`.

## Build from source

You need Rust 1.93.0 or newer, from [rustup](https://rustup.rs).
The first build takes several minutes. Install the newest code with Cargo:

```sh
cargo install --locked --git https://github.com/agustinustheo/polkadot-frame-trace polkadot-frame-trace
```

```text
    Updating git repository `https://github.com/agustinustheo/polkadot-frame-trace`
...
   Installed package `polkadot-frame-trace v0.1.0 (https://github.com/agustinustheo/polkadot-frame-trace#...)` (executable `polkadot-frame-trace`)
```

Add `--tag v<version>` to build a release instead.

Or clone the repository, which has the saved blocks the next step replays.
`rust-toolchain.toml` pins Rust 1.93.0, and `rustup` installs it.

```sh
git clone https://github.com/agustinustheo/polkadot-frame-trace
cd polkadot-frame-trace
cargo install --locked --path .
```

```text
Cloning into 'polkadot-frame-trace'...
...
   Installed package `polkadot-frame-trace v0.1.0 (.../polkadot-frame-trace)` (executable `polkadot-frame-trace`)
```

Cargo puts the binary in `~/.cargo/bin`.

## Check it works

```sh
polkadot-frame-trace --version
```

```text
polkadot-frame-trace 0.1.0
```

Next: [Your first trace in Jaeger](first-trace.md).
