# Typst Bot

A Discord bot that renders [Typst](https://typst.app/) code and converts documents between Typst and LaTeX.

The bot supports slash commands and renders messages containing `$` as inline math or
full documents. Successful renders include buttons for deleting the output or source
message; those buttons expire after five minutes and are restricted to the original
creator.

To render an existing message, right-click it in Discord and choose **Als Typst rendern**.
Plain message text and fenced Typst code blocks are supported.

Use `/render private_preview:true` to receive an ephemeral preview with a button to publish it
to the channel. The regular `/render` response remains public by default.

## Architecture

- `typst-bot` connects to Discord and handles commands and messages.
- `worker` performs Typst compilation and communicates with `typst-bot`.

`typst-bot` starts `worker` automatically.

## Configuration

Configuration is provided through environment variables:

| Variable | Required | Description |
| --- | --- | --- |
| `DISCORD_TOKEN` | Yes | Discord bot token |
| `CACHE_DIRECTORY` | No | Directory used for downloaded package cache |
| `SETTINGS_DIRECTORY` | No | Directory used for persistent bot settings |
| `TYPST_BOT_WORKER_PATH` | No | Path to the worker executable; defaults to `./worker` |

The bot rejects a missing or empty `DISCORD_TOKEN`. Do not create a token file.

## Source conversion

Use `/latex-to-typst` to convert LaTeX source or `/typst-to-latex` to convert Typst source.
Each command accepts either pasted source text or one attached `.tex` / `.typ` file, up to
1 MiB. Text input returns converted text; file input returns a converted file. If inline
output exceeds Discord's message limit, it is returned as an attachment instead.

Conversion runs locally through [Tylax](https://github.com/scipenai/tylax); source is not
sent to an external conversion service. Tylax does not support every feature of either
format, so some documents may need manual adjustments after conversion.

## Native setup

Build and run the workspace:

```sh
cargo build --release --workspace
export DISCORD_TOKEN='your-bot-token'
export CACHE_DIRECTORY='/absolute/path/to/cache'
export SETTINGS_DIRECTORY='/absolute/path/to/settings'

./target/release/typst-bot
```

The bot looks for `./worker` by default. If `typst-bot` and `worker` are in different
locations, set `TYPST_BOT_WORKER_PATH` to the worker executable.

## Docker setup

The image includes both executables and default cache and settings directories, using a
minimal Debian runtime without a shell. Supply the Discord token through the host
environment:

```sh
export DISCORD_TOKEN='your-bot-token'
docker compose up -d --build
```

Docker Compose passes `DISCORD_TOKEN` directly into the container. It is not written to
the image or to a secret file. Package cache and bot settings are retained in the
`cache` and `settings` named volumes.

To stop the bot:

```sh
docker compose down
```

## Discord permissions

Invite the bot with the permissions it needs to read messages, create interactions,
reply to messages, attach files, and add reaction or button controls.

## Security and sandboxing

Typst is interpreted and sandboxed rather than a general-purpose host code execution
environment. Documents can download packages, but self-hosted bot instances limit file
and host-resource access. Rendering is also constrained with timeouts and worker
restarts.

For an exposed deployment, use an additional OS or container sandbox and limit available
CPU, memory, disk, and network resources.

## Privacy

The bot does not persist message content. Poise/Serenity may retain message IDs in
volatile memory for about an hour to track edits and deletions. Those IDs are discarded
when the bot restarts.

## License

AGPL-3.0. Use `@typst-bot source` to get a link to the source from a deployment of the
bot.
