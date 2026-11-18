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
- `<repo>` may be `<owner-hex>/<repo>`. A repo bound to the request's channel is
  used as is; one named that way from elsewhere (a DM) is used only when the
  requester owns it or is a member of the channel it's bound to.
- `--src <repo>:<path>` takes the file from the default branch of a repository
  bound to the channel (by id or name). Drive paths such as `C:\…` stay local.
- `--repo <path>` uses the channel's only bound repository; `--repo <repo>:<path>`
  picks one by id or name. A path ending in `/`, or without a file extension, is
  a directory that keeps mage's file names; a path with an extension names the
  image (the extension follows the real image) and its sidecar shares the stem.
- Commits go straight to the repository's default branch, using the agent's Git
  identity from the harness. A protected branch rejects the push and the reply
  says so; the image is still posted.
- The commit message is mage's reply without its `url:` line (the CDN link
  expires after 30 days), on one line: `model: …, 10s, history_id: …`.
- With an image, the reply (channel or DM) is only the saved location (if any)
  and notes, then the image: Buzz Desktop hides the attached sidecar's file card
  and shows its prompt, model, size, time and history id in a sidebar beside
  the image. Other clients still see the `.json` as a file. Without an image,
  the reply is mage's output.
- Buzz's message-box panel (Generate/Edit, model, aspect, resolution, preset,
  repo folder) sends the prompt as the message text and its settings in a hidden
  `buzz-composer` tag. mage-agent passes every setting to mage explicitly; only
  Edit's Auto aspect and resolution leave the flag out. `@name` words in the
  prompt are kept for mage. `install.ps1` writes the panel into `mage.json`,
  with presets read from `presets.ini`. Each control's `sidecarKey` names the
  sidecar field it comes from, so Edit on a chat image starts from its settings.
- The Preset menu follows `presets.ini` live: the panel's `refresh` entry makes
  Buzz run `mage-agent --refresh-presets` (with `BUZZ_HARNESS_FILE` naming
  `mage.json`) before each read of the panel, on every `@mage` and at most
  once a minute. mage-agent also refreshes when it starts as an agent. Presets
  whose names have spaces or quotes are left out, since Buzz would reject the
  whole panel.
- The panel's Character and Reference menus list your mage assets (`mage
  assets --json`, written into `mage.json` by `mage-agent --refresh-assets`,
  which Buzz runs each time a menu opens and once per mention when you type
  `@`; `install.ps1` fills them once). A pick puts `@handle` in the prompt,
  which mage resolves itself. Buzz shows known handles in bold yellow and
  unknown ones in bold grey, with chips above the prompt, and lists the
  assets an image's prompt names in its info sidebar.
- Overlapping requests each run the CLI, which queues them and runs one at a time.
- Repositories are reached through a temporary blobless clone (`--depth 1
  --filter=blob:none`) deleted with the job: a save uploads only the new files
  and `--src` fetches only that file. A relay that doesn't serve partial clones
  sends every file of the latest commit instead.
- Each job works in `%TEMP%\mage-agent\<message id>`, deleted once the reply is
  posted. A folder left by an agent stopped mid-job is removed the next time
  mage-agent starts, once it's more than a day old.

## Install

```powershell
.\install.ps1 -MageSource ..\buzz\.scratch\mage
```

This builds the release binary, puts `mage-agent.exe` (and, with
`-MageSource`, `mage.exe`, `config.ini`, `presets.ini`, replacing any installed
copies) in `%LOCALAPPDATA%\Programs\mage`, and writes
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

Buzz supplies `BUZZ_RELAY_URL`, `BUZZ_PRIVATE_KEY`, the Git credential helper and
the commit identity. `mage.exe` runs without the Buzz keys in its environment.

## How it talks to Buzz

buzz-acp sends each mention as an ACP `session/prompt`. mage-agent reads the
event blocks (`Event ID`, `Channel`, `Content`, `Tags`) and the `--reply-to`
anchor from `<context>`, and ignores context sections and events from an earlier
turn. Each new event runs on its own thread; the prompt is answered when its
jobs finish. On `session/cancel` (Buzz's steer path when a second mention arrives
mid-job) it answers `cancelled` at once and lets the job finish and post. While a
prompt is open it sends a `keepalive` session update every 5 minutes, so a job
waiting in mage's queue doesn't trip buzz-acp's idle timeout.

The prompt layout is written for models, not as an API: if Buzz changes it,
`src/prompt.rs` and its tests are the place to update.

## Test

```
cargo test
MAGE_AGENT_SAMPLE_JPEG=<a mage jpeg> cargo test -- --ignored
```
