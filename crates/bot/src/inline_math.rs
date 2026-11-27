use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude::{GatewayIntents, Message};
use serenity::all::{
	Context, CreateAllowedMentions, CreateAttachment, CreateMessage, EventHandler, Interaction,
};
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::render_controls::{
	create_components, handle_component_interaction, render_attribution, schedule_expiration,
};
use crate::worker::Worker;

const PREAMBLE: &str = concat!(
	"// Begin preamble\n",
	"// Page size:\n",
	"#set page(width: 300pt, height: auto, margin: 10pt)\n",
	"// Theme:\n",
	"#set page(fill: rgb(49, 51, 56))\n",
	"#set text(fill: rgb(219, 222, 225))\n",
	"// End preamble\n",
);

struct InlineMathHandler {
	worker: Mutex<Worker>,
	settings: InlineMathSettings,
}

#[derive(Clone, Default)]
pub(crate) struct InlineMathSettings {
	opt_out_guilds: Arc<RwLock<HashSet<u64>>>,
	persistence_path: Option<Arc<PathBuf>>,
}

impl InlineMathSettings {
	pub(crate) fn from_environment() -> Self {
		let settings_directory = std::env::var_os("SETTINGS_DIRECTORY")
			.or_else(|| std::env::var_os("CACHE_DIRECTORY"));
		let Some(settings_directory) = settings_directory else {
			return Self::default();
		};

		Self::from_directory(PathBuf::from(settings_directory))
	}

	fn from_directory(settings_directory: PathBuf) -> Self {
		if let Err(error) = std::fs::create_dir_all(&settings_directory) {
			tracing::warn!(?error, "Failed to create settings directory");
		}
		let persistence_path = settings_directory.join("inline_math_opt_outs.bin");
		let opt_out_guilds = std::fs::read(&persistence_path)
			.ok()
			.and_then(|bytes| bincode::deserialize(&bytes).ok())
			.unwrap_or_default();

		Self {
			opt_out_guilds: Arc::new(RwLock::new(opt_out_guilds)),
			persistence_path: Some(Arc::new(persistence_path)),
		}
	}

	pub(crate) async fn set_enabled(&self, guild_id: u64, enabled: bool) {
		let mut opt_out_guilds = self.opt_out_guilds.write().await;
		if enabled {
			opt_out_guilds.remove(&guild_id);
		} else {
			opt_out_guilds.insert(guild_id);
		}
		if let Some(path) = &self.persistence_path {
			match bincode::serialize(&*opt_out_guilds) {
				Ok(bytes) => {
					if let Err(error) = std::fs::write(path.as_ref(), bytes) {
						tracing::warn!(?error, "Failed to persist inline-math settings");
					}
				}
				Err(error) => tracing::warn!(?error, "Failed to encode inline-math settings"),
			}
		}
	}

	pub(crate) async fn is_enabled(&self, guild_id: u64) -> bool {
		!self.opt_out_guilds.read().await.contains(&guild_id)
	}
}

fn source_for_message(content: &str) -> Option<String> {
	content.contains('$').then(|| format!("{PREAMBLE}{content}"))
}

#[serenity::async_trait]
impl EventHandler for InlineMathHandler {
	async fn message(&self, ctx: Context, message: Message) {
		if message.author.bot {
			return;
		}

		if let Some(guild_id) = message.guild_id {
			if !self.settings.is_enabled(guild_id.get()).await {
				return;
			}
		}

		if let Some(source) = source_for_message(&message.content) {
			let (progress_sender, _progress_receiver) = mpsc::channel(1);
			let rendered = self.worker.lock().await.render(source, progress_sender).await;
			match rendered {
				Ok(rendered) if !rendered.images.is_empty() => {
					let controls = create_components(message.author.id, message.channel_id, message.id);
					let expires_at = controls.expires_at;
					let mut reply = CreateMessage::new()
						.reference_message(&message)
						.content(render_attribution(message.author.id.get()))
						.allowed_mentions(CreateAllowedMentions::new())
						.components(controls.components);
					for (index, image) in rendered.images.into_iter().enumerate() {
						reply = reply.add_file(CreateAttachment::bytes(
							image,
							format!("page-{}.png", index + 1),
						));
					}
					match message.channel_id.send_message(&ctx.http, reply).await {
						Ok(render_message) => schedule_expiration(
							&ctx,
							render_message.channel_id,
							render_message.id,
							expires_at,
						),
						Err(error) => tracing::error!(?error, "Failed to send inline math render"),
					}
				}
				Ok(_) => {
					let reply = CreateMessage::new()
						.reference_message(&message)
						.content("The message rendered without producing any pages.");
					if let Err(error) = message.channel_id.send_message(&ctx.http, reply).await {
						tracing::error!(?error, "Failed to send empty inline math render response");
					}
				}
				Err(error) => {
					tracing::warn!(?error, "Failed to render inline math");
					let reply = CreateMessage::new()
						.reference_message(&message)
						.content(format!("Failed to render message: {error}"));
					if let Err(error) = message.channel_id.send_message(&ctx.http, reply).await {
						tracing::error!(?error, "Failed to send inline math render error");
					}
				}
			}
		}
	}

	async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
		if let Interaction::Component(interaction) = interaction {
			if let Err(error) = handle_component_interaction(&ctx, &interaction).await {
				tracing::error!(?error, "Failed to respond to render control interaction");
			}
		}
	}
}

pub async fn run(settings: InlineMathSettings) {
	let token = match (std::env::var_os("DISCORD_TOKEN"), std::env::var_os("DISCORD_TOKEN_FILE")) {
		(Some(token), None) => token.into_string().expect("`DISCORD_TOKEN` not UTF-8"),
		(None, Some(path)) => std::fs::read_to_string(path).expect("reading from `DISCORD_TOKEN_FILE`"),
		(Some(_), Some(_)) => panic!("both `DISCORD_TOKEN` and `DISCORD_TOKEN_FILE` provided"),
		(None, None) => panic!("need `DISCORD_TOKEN` or `DISCORD_TOKEN_FILE` env var"),
	};

	let handler = InlineMathHandler {
		worker: Mutex::new(Worker::spawn().await.expect("spawning Typst worker")),
		settings,
	};
	let intents = GatewayIntents::GUILD_MESSAGES
		| GatewayIntents::DIRECT_MESSAGES
		| GatewayIntents::MESSAGE_CONTENT;
	let mut client = serenity::Client::builder(token.trim(), intents)
		.event_handler(handler)
		.await
		.expect("creating inline math Discord client");

	if let Err(error) = client.start().await {
		tracing::error!(?error, "Inline math Discord client stopped");
	}
}

#[cfg(test)]
mod tests {
	use super::{source_for_message, InlineMathSettings, PREAMBLE};
	use std::time::{SystemTime, UNIX_EPOCH};

	#[test]
	fn renders_the_entire_message_when_it_contains_a_dollar_sign() {
		let message = "Text before $x$ and after.";
		assert_eq!(
			source_for_message(message).as_deref(),
			Some(format!("{PREAMBLE}{message}").as_str()),
		);
	}

	#[test]
	fn does_not_render_messages_without_a_dollar_sign() {
		assert_eq!(source_for_message("No math here"), None);
	}

	#[test]
	fn renders_messages_with_single_or_unpaired_dollar_signs() {
		assert_eq!(source_for_message("Price: $5").as_deref(), Some(format!("{PREAMBLE}Price: $5").as_str()));
	}

	#[tokio::test]
	async fn inline_math_can_be_disabled_and_reenabled_per_guild() {
		let settings = InlineMathSettings::default();

		assert!(settings.is_enabled(42).await);
		settings.set_enabled(42, false).await;
		assert!(!settings.is_enabled(42).await);
		settings.set_enabled(42, true).await;
		assert!(settings.is_enabled(42).await);
	}

	#[tokio::test]
	async fn guild_settings_are_isolated() {
		let settings = InlineMathSettings::default();

		settings.set_enabled(42, false).await;
		assert!(!settings.is_enabled(42).await);
		assert!(settings.is_enabled(100).await);
	}

	#[tokio::test]
	async fn guild_settings_persist_across_settings_instances() {
		let unique = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.unwrap()
			.as_nanos();
		let directory = std::env::temp_dir().join(format!(
			"typst-bot-inline-math-test-{}-{unique}",
			std::process::id()
		));
		let settings = InlineMathSettings::from_directory(directory.clone());

		settings.set_enabled(42, false).await;

		let reloaded = InlineMathSettings::from_directory(directory.clone());
		assert!(!reloaded.is_enabled(42).await);
		assert!(reloaded.is_enabled(100).await);

		_ = std::fs::remove_dir_all(directory);
	}
}
