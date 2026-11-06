# Templates

A template is a folder of starter files for a project: a `Dorkerfile` plus whatever helper
scripts it needs. `dorker init --template <name>` copies one into a folder, and
`dorker templates` lists them. They live in [`templates/`](https://github.com/cesarferreira/dorker/tree/main/templates) and are built into
the binary, so they work after a plain `cargo install dorker`.

```bash
dorker templates                      # list
dorker templates android-auto         # files, next steps and notes of one
dorker init --template android-auto   # copy it here (--force overwrites)
dorker init --template android-auto some/folder
```

## Add a template

Open a pull request that adds a folder `templates/<name>/`. That is all: the next build picks
it up, and `cargo nextest run` checks it.

```
templates/my-template/
  template.toml     required
  Dorkerfile        required
  README.md         optional; `dorker templates my-template` shows it
  anything else     copied as is (scripts: start with `#!`, be executable)
```

`template.toml` has a one-line description and the steps to show after `init`:

```toml
description = "One line, up to 120 characters"
next = "./fetch.sh"
next = "dorker run -d --name demo"
```

Rules (the test suite enforces them):

- The folder name is lowercase words joined by hyphens, for example `android-auto`.
- The Dorkerfile must parse.
- Every `*.sh` file starts with `#!` and has the executable bit (`chmod +x`).
- No file paths with `..`, and nothing outside the template's folder.

Before opening the pull request, run the template for real: `dorker init --template my-template`
in an empty folder, follow your own `next` steps, and say in the pull request what you ran it
on (CPU, `dorker --version`). Do not commit files you do not own, such as other people's APKs;
download them with a script, like `fetch.sh` in `android-auto`.

If a template needs a script on the host, use `PROVISION` (runs against the build emulator)
or `FETCH` (creates an `INSTALL` file that is missing, for example by downloading it) in the
Dorkerfile. `dorker` asks the user before running either, and the MCP server refuses them.
`android-auto` uses `FETCH ./fetch.sh` so that `dorker run` is the only command needed.

Next: [Build and run →](workflows/build-and-run.md)
