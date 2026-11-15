# mage-agent

A Buzz custom harness with no LLM behind it. Mention it in a channel and it runs
the `mage` image CLI, posts the terminal output with the image and its `.json` sidecar attached, and can
commit the image and its `.json` sidecar to a repository bound to the channel.

```
@mage gen a red fox at dawn --aspect landscape --res 2K
@mage gen --preset logo acme rocket --repo assets/logos/
@mage gen a red fox --repo site:img/hero.jpg
@mage edit make the sky purple            (with an image attached to the message)
@mage edit --src C:\pics\cat.png add a hat
@mage edit --src site:img/hero.jpg make it night --repo site:img/
@mage jobs
```

- Every mage command and flag passes through unchanged, except `--output`, which
  mage-agent sets to a temp directory.
- `edit` without `--src` uses the first image attached to the same message.
- `--src <repo>:<path>` takes the file from the default branch of a repository
  bound to the channel (by id or name). Drive paths such as `C:\…` stay local.
- `--repo <path>` uses the channel's only bound repository; `--repo <repo>:<path>`
  picks one by id or name. A path ending in `/`, or without a file extension, is
  a directory that keeps mage's file names; a path with an extension names the
  image (the extension follows the real image) and its sidecar shares the stem.
- Commits go straight to the repository's default branch, using the agent's Git
  identity from the harness. A protected branch rejects the push and the reply
  says so; the image is still posted.
- Overlapping requests each run the CLI, so mage's single-job rule answers them.

## Install

```powershell
.\install.ps1 -MageSource ..\buzz\.scratch\mage
```

This builds the release binary, puts `mage-agent.exe` (and, with
`-MageSource`, `mage.exe`, `config.ini`, `presets.ini`) in
`%LOCALAPPDATA%\Programs\mage`, and writes
`%APPDATA%\xyz.block.buzz.app.dev\custom_harnesses\mage.json`. `just reset`
deletes that file; run the script again afterwards. Use `-App prod` for the
installed app.

Then create an agent named `mage` in Buzz Desktop with the **Mage** harness and
add it to the channels it should serve.

## Environment

| Variable | Default |
|---|---|
| `MAGE_CLI` | `mage.exe` next to `mage-agent.exe`, else `mage` on `PATH` |
| `BUZZ_CLI` | `buzz` on `PATH` (Buzz puts its sidecars there) |
| `MAGE_AGENT_HOME` | `%LOCALAPPDATA%\mage-agent` (repository clones live in `repos\`) |

Buzz supplies `BUZZ_RELAY_URL`, `BUZZ_PRIVATE_KEY`, the Git credential helper and
the commit identity. `mage.exe` runs without the Buzz keys in its environment.

## How it talks to Buzz

buzz-acp sends each mention as an ACP `session/prompt`. mage-agent reads the
event blocks (`Event ID`, `Channel`, `Content`, `Tags`) and the `--reply-to`
anchor from `<context>`, and ignores context sections and events from an earlier
turn. Each new event runs on its own thread; the prompt is answered when its
jobs finish. On `session/cancel` (Buzz's steer path when a second mention arrives
mid-job) it answers `cancelled` at once and lets the job finish and post.

The prompt layout is written for models, not as an API: if Buzz changes it,
`src/prompt.rs` and its tests are the place to update.

## Test

```
cargo test
MAGE_AGENT_SAMPLE_JPEG=<a mage jpeg> cargo test -- --ignored
```
