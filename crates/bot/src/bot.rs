use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::str::FromStr;
use std::time::Duration;

use poise::serenity_prelude::GatewayIntents;
use poise::{async_trait, CreateReply};
use protocol::VersionResponse;
use serenity::all::{Context as SerenityContext, Interaction};
use serenity::builder::{CreateAllowedMentions, CreateAttachment, CreateEmbed};
use serenity::client::FullEvent;
use tokio::join;
use tokio::sync::{mpsc, Mutex};

use crate::render_controls::{
	create_components, create_render_components, handle_component_interaction, render_attribution,
	schedule_expiration,
};
use crate::inline_math::InlineMathSettings;
use crate::worker::Worker;
use crate::SOURCE_URL;

/// U+200D is a zero-width joiner.
/// It prevents the triple backtick from being interpreted as a codeblock but retains ligature support.
const ZERO_WIDTH_JOINER: char = '\u{200D}';

/// Prevent garbled output from codeblocks unwittingly terminated by their own content.
fn sanitize_code_block(raw: &str) -> impl Display + '_ {
	struct Helper<'a>(&'a str);

	impl Display for Helper<'_> {
		fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			for section in self.0.split_inclusive("```") {
				let (safe, should_append) = section
					.strip_suffix("```")
					.map_or((section, false), |safe| (safe, true));
				formatter.write_str(safe)?;
				if should_append {
					write!(formatter, "``{ZERO_WIDTH_JOINER}`")?;
				}
			}

			Ok(())
		}
	}

	Helper(raw)
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid theme")]
struct InvalidTheme;

impl FromStr for Theme {
	type Err = InvalidTheme;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"transparent" | "t" => Self::Transparent,
			"light" | "l" => Self::Light,
			"dark" | "d" => Self::Dark,
			_ => return Err(InvalidTheme),
		})
	}
}

#[derive(Default, Debug, Clone, Copy)]
enum Theme {
	Transparent,
	Light,
	#[default]
	Dark,
}

impl Theme {
	const fn preamble(self) -> &'static str {
		match self {
			Self::Transparent => "",
			Self::Light => "#set page(fill: white)\n",
			Self::Dark => concat!(
				"#set page(fill: rgb(49, 51, 56))\n",
				"#set text(fill: rgb(219, 222, 225))\n",
			),
		}
	}
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid page size")]
struct InvalidPageSize;

impl FromStr for PageSize {
	type Err = InvalidPageSize;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"preview" | "p" => Self::Preview,
			"auto" | "a" => Self::Auto,
			"default" | "d" => Self::Default,
			_ => return Err(InvalidPageSize),
		})
	}
}

#[derive(Default, Debug, Clone, Copy)]
enum PageSize {
	#[default]
	Preview,
	Auto,
	Default,
}

impl PageSize {
	const fn preamble(self) -> &'static str {
		match self {
			Self::Preview => "#set page(width: 300pt, height: auto, margin: 10pt)\n",
			Self::Auto => "#set page(width: auto, height: auto, margin: 10pt)\n",
			Self::Default => "",
		}
	}
}

#[derive(Default, Debug, Clone, Copy)]
struct Preamble {
	page_size: PageSize,
	theme: Theme,
}

impl Preamble {
	fn preamble(self) -> String {
		let page_size = self.page_size.preamble();
		let theme = self.theme.preamble();
		if theme.is_empty() && page_size.is_empty() {
			String::new()
		} else {
			format!(
				concat!(
					"// Begin preamble\n",
					"// Page size:\n",
					"{page_size}",
					"// Theme:\n",
					"{theme}",
					"// End preamble\n",
				),
				page_size = page_size,
				theme = theme,
			)
		}
	}
}

struct Data {
	pool: Mutex<Worker>,
	inline_math_settings: InlineMathSettings,
}

type PoiseError = Box<dyn std::error::Error + Send + Sync + 'static>;
type Context<'a> = poise::Context<'a, Data, PoiseError>;

#[derive(Debug, Default)]
struct RenderFlags {
	preamble: Preamble,
}

#[async_trait]
impl<'a> poise::PopArgument<'a> for RenderFlags {
	async fn pop_from(
		args: &'a str,
		attachment_index: usize,
		ctx: &serenity::prelude::Context,
		message: &poise::serenity_prelude::Message,
	) -> Result<(&'a str, usize, Self), (PoiseError, Option<String>)> {
		fn inner(raw: &HashMap<String, String>) -> Result<RenderFlags, PoiseError> {
			let mut parsed = RenderFlags::default();

			for (key, value) in raw {
				match key.as_str() {
					"theme" | "t" => {
						parsed.preamble.theme = value.parse().map_err(|_| "invalid theme")?;
					}
					"pagesize" | "ps" => {
						parsed.preamble.page_size = value.parse().map_err(|_| "invalid page size")?;
					}
					_ => {
						return Err(format!("unrecognized flag {key:?}").into());
					}
				}
			}

			Ok(parsed)
		}

		let (remaining, pos, raw) =
			poise::prefix_argument::KeyValueArgs::pop_from(args, attachment_index, ctx, message).await?;

		inner(&raw.0)
			.map(|parsed| (remaining, pos, parsed))
			.map_err(|error| (error, None))
	}
}

fn render_help() -> String {
	let default_preamble = Preamble::default().preamble();

	format!(
		"\
**Render Typst code**
Render Typst source as an image. Use `/render` with source in the `code` option, or run
`?render [pagesize=<size>] [theme=<theme>] <code block>`.
Prefix alias: `?r [ps=<size>] [t=<theme>] <code block>`.

**Page size** — `pagesize` / `ps`
- `preview` / `p` (default): 300pt wide, automatic height, 10pt margin
- `auto` / `a`: automatic width and height, 10pt margin
- `default` / `d`: use Typst's page defaults

**Theme** — `theme` / `t`
- `dark` / `d` (default): Discord-dark background and text
- `light` / `l`: white background
- `transparent` / `t`: no page fill or text color override
Slash options use the full values (`preview`, `auto`, `default`; `dark`, `light`, `transparent`).

**Default preamble**
```
{default_preamble}
```
Use `pagesize=default theme=transparent` to remove the preamble.

**Examples**
```
?render `hello, world!`
?r ps=a t=l `A small, light render`
?render pagesize=default theme=transparent ``‍`
= Heading!

And some text.

#lorem(100)
``‍`
```"
	)
}

fn normalize_help_command(command: Option<&str>) -> Option<&str> {
	match command {
		Some("r") => Some("render"),
		Some("render-prefix") => Some("render"),
		Some("ast-prefix") => Some("ast"),
		Some("inline-math") => Some("inline_math"),
		command => command,
	}
}

fn ast_help() -> String {
	"\
**Show the Typst syntax tree**
Use `/ast` with Typst source in the `code` option, or run `?ast <code block>` with a prefix
command. The parsed syntax tree is returned as formatted text.

**Examples**
```
?ast `hello, world!`
/ast code:hello, world!
```"
		.to_owned()
}

fn version_help() -> String {
	"\
**Show version information**
Use `/version` or `?version` to show the bot's build hash and the Typst version it uses."
		.to_owned()
}

fn source_help() -> String {
	"\
**Open the source repository**
Use `/source` or `?source` to get a link to the bot's public source code."
		.to_owned()
}

fn inline_math_help() -> String {
	"\
**Automatic inline-math rendering**
Messages containing `$...$` are rendered automatically in servers where the feature is enabled.
Use `/inline-math enabled:false` to disable it for this server, or
`/inline-math enabled:true` to enable it again. The setting is server-specific and persists
across restarts. This command is only available in servers."
		.to_owned()
}

fn help_command_help() -> String {
	"\
**Get command help**
Use `/help` to see the command overview, `/help command:<name>` for detailed command help,
`?help` for the same overview, or `?help <name>` for details.

Detailed help is available for `render`, `ast`, `version`, `source`, `inline_math`, and `help`."
		.to_owned()
}

fn general_help() -> String {
	"\
**Typst Bot**
Render Typst documents and snippets directly in Discord.

**Quick start**
- `?render ```<Typst code>````
- Alias: `?r ```<Typst code>````
- Slash command: `/render` with source in the `code` option
- Inline math: include `$...$` in any message, for example `The value is $x^2$`.

**Commands**
- `/render`, `?render`, `?r` — render Typst; use `/help command:render` for options and examples
- `/ast`, `?ast` — show the parsed syntax tree
- `/version`, `?version` — show the bot and Typst versions
- `/source`, `?source` — open the bot's source repository
- `/inline-math enabled:<true|false>` — manage automatic inline-math rendering in this server
- `/help`, `?help` — show this menu or help for one command

**Tips**
- Edit or delete a prefix-command message and the bot will update or remove the response.
- Render controls let the creator remove the render or source message for five minutes.
- Use `/help command:<name>` or `?help <name>` for command details."
		.to_owned()
}

fn general_help_embed() -> CreateEmbed {
	CreateEmbed::new()
		.title("Typst Bot")
		.description(general_help())
		.field(
			"Render Typst",
			"`/render` or `?render <code block>`",
			false,
		)
		.field(
			"Inline math",
			"Include `$...$` in a message.\nManage server behavior with `/inline-math enabled:false`.",
			false,
		)
		.field(
			"Other commands",
			"`/ast` · `/version` · `/source` · `/inline-math` · `/help`",
			false,
		)
		.field(
			"Detailed help",
			"`?help render` or `/help command:render`",
			false,
		)
}

/// Extracts the contents of a code block.
///
/// If the language is `ansi`, then ANSI escape codes will be stripped from the input.
struct CodeBlock {
	source: String,
}

#[async_trait]
impl<'a> poise::PopArgument<'a> for CodeBlock {
	async fn pop_from(
		mut args: &'a str,
		attachment_index: usize,
		ctx: &serenity::prelude::Context,
		message: &poise::serenity_prelude::Message,
	) -> Result<(&'a str, usize, Self), (PoiseError, Option<String>)> {
		if let Some(code_block_start) = args.find("```") {
			args = &args[code_block_start..];
		}

		let (rest, attachment_index, code_block) =
			poise::prefix_argument::CodeBlock::pop_from(args, attachment_index, ctx, message).await?;

		let mut source = code_block.code;
		// Strip ANSI escapes if provided.
		if code_block.language.as_deref() == Some("ansi") {
			source = strip_ansi_escapes::strip_str(source);
		}

		// Remove all occurrences of zero width joiners surrounded by backticks.
		// This is used to enter Typst code blocks within Discord-markdown code blocks.
		// Two replace calls are needed to remove all patterns of `ABA`: ABABABA => AABAA => AAAA.
		let pattern = format!("`{ZERO_WIDTH_JOINER}`");
		let replacement = "``";
		source = source
			.replace(&pattern, replacement)
			.replace(&pattern, replacement);

		Ok((rest, attachment_index, CodeBlock { source }))
	}
}

struct Rest;

#[async_trait]
impl<'a> poise::PopArgument<'a> for Rest {
	async fn pop_from(
		_args: &'a str,
		attachment_index: usize,
		_ctx: &serenity::prelude::Context,
		_message: &poise::serenity_prelude::Message,
	) -> Result<(&'a str, usize, Self), (PoiseError, Option<String>)> {
		Ok(("", attachment_index, Self))
	}
}

#[poise::command(
	prefix_command,
	track_edits,
	broadcast_typing,
	user_cooldown = 1,
	help_text_fn = "render_help",
	rename = "render-prefix",
	aliases("render", "r")
)]
async fn render(
	ctx: Context<'_>,
	flags: RenderFlags,
	code: CodeBlock,
	#[rename = "rest"] _: Rest,
) -> Result<(), PoiseError> {
	let pool = &ctx.data().pool;

	let mut source = code.source;
	source.insert_str(0, &flags.preamble.preamble());

	let mut progress = String::new();
	let (progress_send, mut progress_recv) = mpsc::channel(4);
	let (res, ()) = {
		let mut pool = pool.lock().await;
		join!(pool.render(source, progress_send), async {
			// When `render` finishes, it will drop the sender so this loop will finish.
			while let Some(item) = progress_recv.recv().await {
				progress.reserve(item.len() + 1);
				progress.push_str(&item);
				progress.push('\n');
				let message = format!("Progress: ```ansi\n{}\n```", sanitize_code_block(&progress));
				_ = ctx.say(message).await;
			}
		})
	};

	match res {
		Ok(res) => {
			let (source_channel_id, source_message_id) = match ctx {
				poise::Context::Prefix(prefix) => (prefix.msg.channel_id, prefix.msg.id),
				poise::Context::Application(_) => unreachable!("render is a prefix command"),
			};
			let controls = create_components(ctx.author().id, source_channel_id, source_message_id);
			let expires_at = controls.expires_at;
			let mut message = CreateReply::default()
				.reply(true)
				.components(controls.components);

			let mut content = format!("{}\n", render_attribution(ctx.author().id.get()));

			if res.images.is_empty() {
				writeln!(content, "Note: no pages generated").unwrap();
			}

			if res.more_pages > 0 {
				let more_pages = res.more_pages;
				writeln!(
					content,
					"Note: {more_pages} more page{s} ignored",
					s = if more_pages == 1 { "" } else { "s" },
				)
				.unwrap();
			}

			if !res.warnings.is_empty() {
				writeln!(
					content,
					"Render succeeded with warnings:\n```ansi\n{}\n```",
					sanitize_code_block(&res.warnings),
				)
				.unwrap();
			}

			if !content.is_empty() {
				message = message.content(content);
			}

			for (i, image) in res.images.into_iter().enumerate() {
				let image = CreateAttachment::bytes(image, format!("page-{}.png", i + 1));
				message = message.attachment(image);
			}

			let sent_message = ctx.send(message).await?;
			let sent_message = sent_message.message().await?;
			schedule_expiration(
				ctx.serenity_context(),
				sent_message.channel_id,
				sent_message.id,
				expires_at,
			);
		}
		Err(error) => {
			let message = format!(
				"An error occurred:\n```ansi\n{}\n```",
				sanitize_code_block(&format!("{error:?}")),
			);
			ctx.reply(message).await?;
		}
	}

	Ok(())
}

/// Render Typst code as an image.
#[poise::command(slash_command, rename = "render")]
async fn slash_render(
	ctx: Context<'_>,
	#[description = "Typst source code"] code: String,
	#[description = "Preview, auto, or default page size"]
	pagesize: Option<String>,
	#[description = "Dark, light, or transparent theme"]
	theme: Option<String>,
) -> Result<(), PoiseError> {
	let mut preamble = Preamble::default();
	if let Some(pagesize) = pagesize {
		preamble.page_size = pagesize.parse().map_err(|_| "invalid page size")?;
	}
	if let Some(theme) = theme {
		preamble.theme = theme.parse().map_err(|_| "invalid theme")?;
	}

	let mut source = preamble.preamble();
	source.push_str(&code);

	ctx.defer().await?;

	let pool = &ctx.data().pool;
	let (progress_send, _progress_recv) = mpsc::channel(4);
	let res = pool.lock().await.render(source, progress_send).await;

	match res {
		Ok(res) => {
			let controls = create_render_components(ctx.author().id);
			let mut message = CreateReply::default()
				.components(controls.components);

			let mut content = format!("{}\n", render_attribution(ctx.author().id.get()));
			if res.images.is_empty() {
				writeln!(content, "Note: no pages generated").unwrap();
			}
			if res.more_pages > 0 {
				writeln!(
					content,
					"Note: {} more page{} ignored",
					res.more_pages,
					if res.more_pages == 1 { "" } else { "s" },
				)
				.unwrap();
			}
			if !res.warnings.is_empty() {
				writeln!(
					content,
					"Render succeeded with warnings:\n```ansi\n{}\n```",
					sanitize_code_block(&res.warnings),
				)
				.unwrap();
			}
			message = message.content(content);

			for (i, image) in res.images.into_iter().enumerate() {
				message = message.attachment(CreateAttachment::bytes(
					image,
					format!("page-{}.png", i + 1),
				));
			}

			let sent_message = ctx.send(message).await?;
			let sent_message = sent_message.message().await?;
			schedule_expiration(
				ctx.serenity_context(),
				sent_message.channel_id,
				sent_message.id,
				controls.expires_at,
			);
		}
		Err(error) => {
			let message = format!(
				"An error occurred:\n```ansi\n{}\n```",
				sanitize_code_block(&format!("{error:?}")),
			);
			ctx.reply(message).await?;
		}
	}

	Ok(())
}

/// Show this menu.
#[poise::command(prefix_command, track_edits, slash_command)]
async fn help(
	ctx: Context<'_>,
	#[description = "Specific command to show help about"] command: Option<String>,
) -> Result<(), PoiseError> {
	match normalize_help_command(command.as_deref()) {
		None => ctx.send(CreateReply::default().embed(general_help_embed())).await?,
		Some("render") => {
			let embed = CreateEmbed::new()
				.title("Render Typst")
				.description(render_help());
			ctx.send(CreateReply::default().embed(embed)).await?
		}
		Some("ast") => {
			let embed = CreateEmbed::new()
				.title("AST")
				.description(ast_help());
			ctx.send(CreateReply::default().embed(embed)).await?
		}
		Some("version") => {
			ctx.send(
				CreateReply::default().embed(
					CreateEmbed::new()
						.title("Version")
						.description(version_help()),
				),
			)
			.await?
		}
		Some("source") => {
			ctx.send(
				CreateReply::default().embed(
					CreateEmbed::new()
						.title("Source")
						.description(source_help()),
				),
			)
			.await?
		}
		Some("inline_math") => {
			ctx.send(
				CreateReply::default().embed(
					CreateEmbed::new()
						.title("Inline math")
						.description(inline_math_help()),
				),
			)
			.await?
		}
		Some("help") => {
			let embed = CreateEmbed::new()
				.title("Help")
				.description(help_command_help());
			ctx.send(CreateReply::default().embed(embed)).await?
		}
		Some(command) => {
			ctx.say(format!(
				"Unknown command `{command}`.\nUse `/help` or `?help` to see available commands."
			))
			.await?
		}
	};
	Ok(())
}

/// Enable or disable automatic inline-math rendering in this server.
#[poise::command(slash_command, rename = "inline-math")]
async fn inline_math(
	ctx: Context<'_>,
	#[description = "Whether automatic inline-math rendering is enabled"] enabled: bool,
) -> Result<(), PoiseError> {
	let Some(guild_id) = ctx.guild_id() else {
		ctx.reply("Inline-math settings are only available in servers.")
			.await?;
		return Ok(());
	};

	ctx.data()
		.inline_math_settings
		.set_enabled(guild_id.get(), enabled)
		.await;

	ctx.reply(if enabled {
		"Automatic inline-math rendering is enabled."
	} else {
		"Automatic inline-math rendering is disabled."
	})
	.await?;
	Ok(())
}

/// Get a link to the bot's source.
#[poise::command(prefix_command, slash_command)]
async fn source(ctx: Context<'_>) -> Result<(), PoiseError> {
	ctx.reply(format!("<{SOURCE_URL}>")).await?;
	Ok(())
}

/// Get the AST for the given code.
///
/// Syntax: `@typst-bot ast <code block> [...]`
/// Use `/ast` with Typst source in the `code` option.
///
/// **Examples**
///
/// ```
/// @typst-bot ast `hello, world!`
///
/// @typst-bot ast ``‍`
/// = Heading!
///
/// And some text.
///
/// #lorem(100)
/// ``‍`
///
/// @typst-bot ast `#((3): 4)` Interesting parse result here.
/// ```
#[poise::command(
	prefix_command,
	track_edits,
	broadcast_typing,
	rename = "ast-prefix",
	aliases("ast")
)]
async fn ast(
	ctx: Context<'_>,
	#[description = "Code to parse"] code: CodeBlock,
	#[rename = "rest"]
	#[description = "Extra message content"]
	_: Rest,
) -> Result<(), PoiseError> {
	respond_with_ast(ctx, code.source).await
}

async fn respond_with_ast(ctx: Context<'_>, source: String) -> Result<(), PoiseError> {
	let pool = &ctx.data().pool;

	let res = pool.lock().await.ast(source).await;

	match res {
		Ok(ast) => {
			let message = format!("```ansi\n{}```", sanitize_code_block(&ast));
			ctx.reply(message).await?;
		}
		Err(error) => {
			let message = format!(
				"An error occurred:\n```ansi\n{}```",
				sanitize_code_block(&format!("{error:?}")),
			);
			ctx.reply(message).await?;
		}
	}

	Ok(())
}

/// Get the AST for the given Typst source.
#[poise::command(slash_command, rename = "ast")]
async fn slash_ast(
	ctx: Context<'_>,
	#[description = "Typst source code"] code: String,
) -> Result<(), PoiseError> {
	ctx.defer().await?;
	respond_with_ast(ctx, code).await
}

/// Show the bot's Typst version.
#[poise::command(prefix_command, slash_command)]
async fn version(ctx: Context<'_>) -> Result<(), PoiseError> {
	if matches!(&ctx, poise::Context::Application(_)) {
		ctx.defer().await?;
	}

	let pool = &ctx.data().pool;

	let res = pool.lock().await.version().await;

	match res {
		Ok(VersionResponse {
			version: typst_version,
		}) => {
			let bot_hash = env!("BUILD_SHA");
			let message = format!("\
The bot was built from git hash [`{bot_hash}`](<https://github.com/MrAn0nym/typst-bot/tree/{bot_hash}>)
The bot is using Typst [version **{typst_version}**](<https://github.com/typst/typst/releases/v{typst_version}>)\
");
			ctx.reply(message).await?;
		}
		Err(error) => {
			let message = format!("An error occurred:\n```ansi\n{error}```");
			ctx.reply(message).await?;
		}
	}

	Ok(())
}

async fn handle_error(
	error: poise::FrameworkError<'_, Data, Box<dyn std::error::Error + Send + Sync>>,
) -> serenity::Result<()> {
	if let poise::FrameworkError::ArgumentParse {
		ctx, input, error, ..
	} = error
	{
		let command_name = &ctx.command().name;
		let name = normalize_help_command(Some(command_name)).unwrap_or(command_name);
		let usage = format!(
			"Use `@typst-bot help {name}` for usage. Feel free to edit or delete your message and the bot will react.",
		);
		let response = input.map_or_else(
			|| format!("**{error}**\n{usage}"),
			|input| format!("**Cannot parse `{input}` as argument: {error}**\n{usage}"),
		);
		ctx.reply(response).await?;
		Ok(())
	} else {
		poise::builtins::on_error(error).await
	}
}

async fn handle_framework_event(
	ctx: &SerenityContext,
	event: &FullEvent,
	_framework: poise::FrameworkContext<'_, Data, PoiseError>,
	_data: &Data,
) -> Result<(), PoiseError> {
	if let FullEvent::InteractionCreate {
		interaction: Interaction::Component(interaction),
	} = event
	{
		if let Err(error) = handle_component_interaction(ctx, interaction).await {
			eprintln!("ERROR Failed to respond to render control interaction: {error:?}");
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{general_help, normalize_help_command, render_help};

	#[test]
	fn ast_is_registered_as_a_slash_command() {
		let command = super::slash_ast();

		assert_eq!(command.name, "ast");
		assert!(command.slash_action.is_some());
		assert!(super::ast().prefix_action.is_some());
	}

	#[test]
	fn prefix_and_slash_commands_with_shared_user_names_have_distinct_dispatch_names() {
		let prefix_render = super::render();
		let slash_render = super::slash_render();
		let prefix_ast = super::ast();
		let slash_ast = super::slash_ast();

		assert_ne!(prefix_render.name, slash_render.name);
		assert!(prefix_render.aliases.iter().any(|alias| alias == "render"));
		assert!(prefix_render.aliases.iter().any(|alias| alias == "r"));
		assert_ne!(prefix_ast.name, slash_ast.name);
		assert!(prefix_ast.aliases.iter().any(|alias| alias == "ast"));
	}

	#[test]
	fn general_help_lists_all_user_facing_features() {
		let help = general_help();

		assert!(help.contains("`render`"));
		assert!(help.contains("`ast`"));
		assert!(help.contains("`version`"));
		assert!(help.contains("`source`"));
		assert!(help.contains("`/inline-math`"));
		assert!(help.contains("$...$"));
	}

	#[test]
	fn ast_help_explains_prefix_and_slash_usage() {
		let help = super::ast_help();

		assert!(help.contains("`/ast`"));
		assert!(help.contains("`?ast <code block>`"));
	}

	#[test]
	fn render_help_documents_the_available_flags_and_aliases() {
		let help = render_help();

		assert!(help.contains("pagesize"));
		assert!(help.contains("theme"));
		assert!(help.contains("preview"));
		assert!(help.contains("transparent"));
		assert!(help.contains("`/render`"));
	}

	#[test]
	fn help_command_aliases_resolve_to_canonical_names() {
		assert_eq!(normalize_help_command(Some("r")), Some("render"));
		assert_eq!(normalize_help_command(Some("render-prefix")), Some("render"));
		assert_eq!(normalize_help_command(Some("ast-prefix")), Some("ast"));
		assert_eq!(normalize_help_command(Some("inline-math")), Some("inline_math"));
		assert_eq!(normalize_help_command(Some("render")), Some("render"));
		assert_eq!(normalize_help_command(None), None);
		assert_eq!(normalize_help_command(Some("unknown")), Some("unknown"));
	}
}

pub async fn run(inline_math_settings: InlineMathSettings) {
	let token = match (std::env::var_os("DISCORD_TOKEN"), std::env::var_os("DISCORD_TOKEN_FILE")) {
		(Some(token), None) => token.into_string().expect("`DISCORD_TOKEN` not UTF-8"),
		(None, Some(path)) => std::fs::read_to_string(path).expect("reading from `DISCORD_TOKEN_FILE`"),
		(Some(_token), Some(_path)) => panic!("both `DISCORD_TOKEN` and `DISCORD_TOKEN_FILE` provided.\nThis is ambiguous and insecure. Please only use one or the other."),
		(None, None) => panic!("need `DISCORD_TOKEN` or `DISCORD_TOKEN_FILE` env var"),
	};
	let token = token.trim();

	let pool = Worker::spawn().await.unwrap();

	let edit_tracker_time = Duration::from_hours(1);

	let intents = GatewayIntents::non_privileged();
	let framework = poise::Framework::builder()
		.options(poise::FrameworkOptions {
			prefix_options: poise::PrefixFrameworkOptions {
				prefix: Some("?".to_owned()),
				edit_tracker: Some(poise::EditTracker::for_timespan(edit_tracker_time).into()),
				..Default::default()
			},
			commands: vec![
				render(),
				slash_render(),
				help(),
				inline_math(),
				source(),
				ast(),
				slash_ast(),
				version(),
			],
			allowed_mentions: Some(CreateAllowedMentions::new()),
			event_handler: |ctx, event, framework, data| {
				Box::pin(handle_framework_event(ctx, event, framework, data))
			},
			on_error: |error| {
				Box::pin(async move {
					if let Err(error) = handle_error(error).await {
						eprintln!("ERROR Failed while handling a framework error: {error:?}");
					}
				})
			},
			..Default::default()
		})
		.setup(|ctx, _ready, framework| {
			Box::pin(async move {
				poise::builtins::register_globally(ctx, &framework.options().commands).await?;
				Ok(Data {
					pool: Mutex::new(pool),
					inline_math_settings,
				})
			})
		})
		.build();

	let mut client = serenity::client::ClientBuilder::new(token, intents)
		.framework(framework)
		.await
		.unwrap();

	eprintln!("ready");

	client.start().await.unwrap();
}
