# Share Images with Your Team

Build an environment once, push it to a registry, and let everyone run the same image:
the same apps and versions, the same settings, the same snapshot. Nobody repeats the setup.

```bash
# Once, on the machine that builds it
dorker build -t gearhead:ready .
dorker push gearhead:ready ghcr.io/your-org/gearhead:18.0

# Everyone else
dorker run -d --name dev ghcr.io/your-org/gearhead:18.0   # pulls it the first time
```

`dorker pull ghcr.io/your-org/gearhead:18.0` downloads it without starting anything, and
checks for a newer push of that tag. `dorker run` uses the local copy once it has one, like
`docker run`.

## What is sent

Only the image's AVD (its userdata and snapshot) and its metadata. The Android system image
is not: each machine gets it from Google's SDK, and `dorker run` offers to install it when
it is missing. The AVD is packed as one tar stream, compressed with zstd and cut into parts
of at most 512 MB. Pushing again only uploads the parts the registry does not have.

`push` first packs the parts into `<home>/tmp`, then uploads them, then deletes them (also
when interrupted with Ctrl-C; a later push removes what a killed one left). When `home` is on
a network mount, put them on a fast local disk instead with `--tmp-dir`, `DORKER_TMP`, or
`tmp` in the config file. `pull` needs no scratch space: it unpacks while it downloads.

[examples/shared-image](https://github.com/cesarferreira/dorker/tree/main/examples/shared-image)
is a complete example, including a script that tries the whole round trip on one machine
with a throwaway local registry.

It works with any registry that speaks the OCI distribution protocol: GitHub Container
Registry (ghcr.io), a self-hosted `registry:2`, Amazon ECR, Google Artifact Registry,
Artifactory. A registry on `localhost` or `127.0.0.1` is reached over plain HTTP; all others
over HTTPS.

## Apple Silicon and Linux under one tag

An image only runs on the CPU architecture it was built for: arm64 on Apple Silicon, x86_64
on Linux. Build the same Dorkerfile on each and push both to the same tag:

```bash
# On a Mac
dorker build -t gearhead:ready . && dorker push gearhead:ready ghcr.io/your-org/gearhead:18.0
# On Linux (or Linux CI)
dorker build -t gearhead:ready . && dorker push gearhead:ready ghcr.io/your-org/gearhead:18.0
```

The tag points to an index with one entry per architecture. A push replaces only its own
architecture's entry, and `pull` picks the one for the machine it runs on. If there is none,
it says which architectures the tag has.

## Fast starts need the same emulator

A snapshot only restores on the emulator build and system image revision that saved it.
`pull` warns when yours differ; the image still runs, but cold boots (about a minute) with
the same data. For fast starts everywhere, keep the team on the same emulator version.

## Logging in

dorker looks for credentials in this order:

1. `DORKER_REGISTRY_TOKEN` (and optionally `DORKER_REGISTRY_USER`),
2. for ghcr.io: `GITHUB_TOKEN` or `GH_TOKEN` (set in GitHub Actions),
3. `docker login` (`~/.docker/config.json`, including credential helpers),
4. for ghcr.io: the GitHub CLI (`gh auth token`).

With none, it pulls anonymously, which works for public images. For a private package on
ghcr.io, the token needs `read:packages` to pull and `write:packages` to push:

```bash
gh auth refresh -s write:packages
```

## Versioning

Use one tag per version of what the image contains, and move a `latest` tag when you
upgrade:

```bash
dorker push gearhead:ready ghcr.io/your-org/gearhead:18.1
dorker push gearhead:ready ghcr.io/your-org/gearhead:latest
```

Older tags stay available, so a regression can be checked against the previous version.
