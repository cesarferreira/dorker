# Configuration

Settings that differ between machines (a Mac and a Linux box, for example) go in `~/.config/dorker/config.toml`. If `$XDG_CONFIG_HOME` is set, the file is `$XDG_CONFIG_HOME/dorker/config.toml`. `DORKER_CONFIG` names another file. Every setting is optional:

```toml
home = "/mnt/big-disk/dorker"  # where images and instances are kept
sdk = "~/Android/Sdk"          # the Android SDK
gpu = "swiftshader_indirect"   # default GPU mode (a GPU line wins)
min_free_mb = 8192             # least free disk needed to start work
```

```bash
dorker config        # each setting in effect, and where it came from
dorker config init   # write a commented example
```

A mistake in the file is an error that names the file.

## What wins

An environment variable beats the file, and so does a flag or a Dorkerfile line.

| Setting | Environment variable | Notes |
|---|---|---|
| `home` | `DORKER_HOME` | Where images and instances are kept. Default `~/.dorker` |
| `sdk` | `ANDROID_HOME` | The Android SDK |
| `gpu` | | Default GPU mode. A `GPU` line in the Dorkerfile wins. Part of an image's identity, so changing it rebuilds images |
| `min_free_mb` | `DORKER_MIN_FREE_MB` | Least free disk needed to start work |

Things that carry a risk are not settable here. `--pull` and `--allow-provision` stay a flag or an environment variable (`DORKER_PULL`, `DORKER_ALLOW_PROVISION`).

## Where to put `home`

Put `home` on one local filesystem that can clone files. A new instance is a clone of its image: `cp -c` on macOS (APFS) and `cp --reflink=auto` on Linux (XFS created with `reflink=1`, or Btrfs). The instance shares the image's blocks until it writes, so the copy finishes in milliseconds.

NFS and ext4 cannot clone, and dorker then copies the whole image (several GB) for every instance. `~/.dorker` is on your home directory, which is often a network mount. Point `home` or `DORKER_HOME` at a local disk before running several workers. The difference is measured in [Benchmarks](../benchmarks.md#parallel-instrumentation).

Next: [Architecture →](../architecture.md)
