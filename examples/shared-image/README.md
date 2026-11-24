# Shared image

One emulator environment for a whole team. Someone builds this folder's `Dorkerfile` and
pushes it to a registry; everyone else runs it by name and gets the same device, the same
settings and a snapshot that starts in seconds. Each developer installs their own build of
the app on top.

| File | What it is for |
|---|---|
| `Dorkerfile` | The shared environment (here: Android 35, animations off, screen on, a snapshot) |
| `publish.sh` | Build it and push it: `./publish.sh ghcr.io/your-org/team-env:1` |
| `dev.sh` | Run it and install your APK: `./dev.sh ghcr.io/your-org/team-env:1 app-debug.apk` |
| `try-locally.sh` | The whole round trip on one machine, with a throwaway registry in Docker |
| `ci/publish-image.yml` | A GitHub Actions workflow that pushes the Linux image when the Dorkerfile changes |

## Try it without an account

```bash
cd examples/shared-image
./try-locally.sh          # needs Docker, the Android SDK and about 10 GB free
```

It starts a registry on `localhost:5055`, builds and pushes the image from one dorker store,
then runs it from a second, empty store: the second store pulls it and restores its snapshot.
Everything is removed at the end.

## With GitHub Container Registry

```bash
gh auth refresh -s write:packages                # once: lets dorker push to ghcr.io
./publish.sh ghcr.io/your-org/team-env:1         # on a Mac, and on Linux (or in CI)

# Teammates (read:packages is enough for them)
./dev.sh ghcr.io/your-org/team-env:1 app/build/outputs/apk/debug/app-debug.apk
```

The package is private by default. Give your team read access on its GitHub page
(Package settings → Manage access), or connect it to the repository.

`dorker run` pulls the image the first time and uses the local copy afterwards. To pick up
a new push of the same tag, run `dorker pull ghcr.io/your-org/team-env:1`. Prefer a new tag
for each change, so older versions stay available.

## Things to know

- An image runs only on the CPU architecture it was built on. Push from both an Apple
  Silicon Mac (arm64) and Linux (x86_64) to the same tag; `pull` picks the right one.
- The Android system image is not uploaded. `dorker run` offers to install it from
  Google's SDK when a machine does not have it.
- A snapshot restores only on the emulator version that saved it. With another version the
  image still runs, but cold boots (about a minute). `pull` says so.
- `push` packs the image into compressed parts (several GB) before uploading. They go to
  `<home>/tmp` unless `--tmp-dir`, `DORKER_TMP` or `tmp` in the config file says otherwise;
  point it at a local disk when your dorker home is on a network mount.

More in [docs/workflows/sharing.md](../../docs/workflows/sharing.md).
