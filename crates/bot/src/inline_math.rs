use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use poise::serenity_prelude::{GatewayIntents, Message};
use serenity::all::{
	Context, CreateAllowedMentions, CreateAttachment, CreateMessage, EventHandler, Interaction,
	MessageUpdateEvent,
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
	rendered_messages: Mutex<HashMap<(u64, u64), Vec<u64>>>,
}

#[derive(Clone, Default)]
pub(crate) struct InlineMathSettings {
	opt_out_guilds: Arc<RwLock<HashSet<u64>>>,
	pending_source_deletions: Arc<Mutex<HashSet<(u64, u64)>>>,
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
			eprintln!("WARN Failed to create inline-math settings directory: {error:?}");
		}
		let persistence_path = settings_directory.join("inline_math_opt_outs.bin");
		let opt_out_guilds = match std::fs::read(&persistence_path) {
			Ok(bytes) => match bincode::deserialize(&bytes) {
				Ok(guilds) => guilds,
				Err(error) => {
					eprintln!("ERROR Failed to decode inline-math settings: {error:?}");
					HashSet::new()
				}
			},
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
			Err(error) => {
				eprintln!("ERROR Failed to read inline-math settings: {error:?}");
				HashSet::new()
			}
		};

		Self {
			opt_out_guilds: Arc::new(RwLock::new(opt_out_guilds)),
			pending_source_deletions: Arc::new(Mutex::new(HashSet::new())),
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
						eprintln!("ERROR Failed to persist inline-math settings: {error:?}");
					}
				}
				Err(error) => eprintln!("ERROR Failed to encode inline-math settings: {error:?}"),
			}
		}
	}

	pub(crate) async fn is_enabled(&self, guild_id: u64) -> bool {
		!self.opt_out_guilds.read().await.contains(&guild_id)
	}

	pub(crate) async fn mark_source_deletion(&self, channel_id: u64, message_id: u64) {
		self.pending_source_deletions
			.lock()
			.await
			.insert((channel_id, message_id));
	}

	pub(crate) async fn cancel_source_deletion(&self, channel_id: u64, message_id: u64) {
		self.pending_source_deletions
			.lock()
			.await
			.remove(&(channel_id, message_id));
	}

	async fn consume_source_deletion(&self, channel_id: u64, message_id: u64) -> bool {
		self.pending_source_deletions
			.lock()
			.await
			.remove(&(channel_id, message_id))
	}
}

fn source_for_message(content: &str) -> Option<String> {
	content.contains('$').then(|| format!("{PREAMBLE}{content}"))
}

fn remove_rendered_message(
	rendered_messages: &mut HashMap<(u64, u64), Vec<u64>>,
	channel_id: u64,
	message_id: u64,
) -> Vec<u64> {
	let rendered_ids = rendered_messages
		.remove(&(channel_id, message_id))
		.unwrap_or_default();

	for remaining_ids in rendered_messages.values_mut() {
		remaining_ids.retain(|rendered_id| *rendered_id != message_id);
	}
	rendered_messages.retain(|_, rendered_ids| !rendered_ids.is_empty());

	rendered_ids
}

#[serenity::async_trait]
impl EventHandler for InlineMathHandler {
	async fn message(&self, ctx: Context, message: Message) {
		self.process_message(&ctx, &message).await;
	}

	async fn message_update(
		&self,
		ctx: Context,
		_old_if_available: Option<Message>,
		new: Option<Message>,
		event: MessageUpdateEvent,
	) {
		let message = match new {
			Some(message) => message,
			None => match event.channel_id.message(&ctx.http, event.id).await {
				Ok(message) => message,
				Err(error) => {
					eprintln!(
						"ERROR Failed to fetch updated message {} in channel {}: {error:?}",
						event.id, event.channel_id
					);
					return;
				}
			},
		};
		self.process_message(&ctx, &message).await;
	}

	async fn message_delete(
		&self,
		ctx: Context,
		channel_id: serenity::all::ChannelId,
		deleted_message_id: serenity::all::MessageId,
		_guild_id: Option<serenity::all::GuildId>,
	) {
		if self
			.settings
			.consume_source_deletion(channel_id.get(), deleted_message_id.get())
			.await
		{
			let mut rendered_messages = self.rendered_messages.lock().await;
			remove_rendered_message(
				&mut rendered_messages,
				channel_id.get(),
				deleted_message_id.get(),
			);
			return;
		}

		let rendered_ids = {
			let mut rendered_messages = self.rendered_messages.lock().await;
			remove_rendered_message(
				&mut rendered_messages,
				channel_id.get(),
				deleted_message_id.get(),
			)
		};
		for rendered_id in rendered_ids {
			if let Err(error) = channel_id
				.delete_message(&ctx.http, serenity::all::MessageId::new(rendered_id))
				.await
			{
				eprintln!(
					"ERROR Failed to delete inline-math render {rendered_id} after source deletion: {error:?}"
				);
			}
		}
	}

	async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
		if let Interaction::Component(interaction) = interaction {
			if let Err(error) =
				handle_component_interaction(&ctx, &interaction, &self.settings).await
			{
				eprintln!("ERROR Failed to respond to inline-math render control: {error:?}");
			}
		}
	}
}

impl InlineMathHandler {
	async fn process_message(&self, ctx: &Context, message: &Message) {
		if message.author.bot {
			return;
		}

		let source_key = (message.channel_id.get(), message.id.get());
		let mut rendered_messages = self.rendered_messages.lock().await;
		let previous_ids = rendered_messages.remove(&source_key).unwrap_or_default();
		let mut failed_deletions = Vec::new();
		for rendered_id in previous_ids {
			if let Err(error) = message
				.channel_id
				.delete_message(&ctx.http, serenity::all::MessageId::new(rendered_id))
				.await
			{
				eprintln!(
					"ERROR Failed to remove previous inline-math render {rendered_id}: {error:?}"
				);
				failed_deletions.push(rendered_id);
			}
		}
		if !failed_deletions.is_empty() {
			rendered_messages.insert(source_key, failed_deletions);
		}

		if let Some(guild_id) = message.guild_id {
			if !self.settings.is_enabled(guild_id.get()).await {
				return;
			}
		}
		let Some(source) = source_for_message(&message.content) else {
			return;
		};

		let (progress_sender, _progress_receiver) = mpsc::channel(1);
		let rendered = self.worker.lock().await.render(source, progress_sender).await;
		let reply = match rendered {
			Ok(rendered) if !rendered.images.is_empty() => {
				let controls = create_components(
					message.author.id,
					message.author.id,
					message.channel_id,
					message.id,
				);
				let expires_at = controls.expires_at;
				let mut reply = CreateMessage::new()
					.reference_message(message)
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
					Ok(render_message) => {
						rendered_messages
							.entry(source_key)
							.or_default()
							.push(render_message.id.get());
						schedule_expiration(
							ctx,
							render_message.channel_id,
							render_message.id,
							expires_at,
						);
					}
					Err(error) => eprintln!("ERROR Failed to send inline-math render: {error:?}"),
				}
				return;
			}
			Ok(_) => CreateMessage::new()
				.reference_message(message)
				.content("The message rendered without producing any pages."),
			Err(error) => {
				eprintln!("ERROR Failed to render inline math: {error:?}");
				CreateMessage::new()
					.reference_message(message)
					.content(format!("Failed to render message: {error}"))
			}
		};

		match message.channel_id.send_message(&ctx.http, reply).await {
			Ok(response) => rendered_messages
				.entry(source_key)
				.or_default()
				.push(response.id.get()),
			Err(error) => eprintln!("ERROR Failed to send inline-math response: {error:?}"),
		}
	}
}

pub async fn run(settings: InlineMathSettings) {
	let token = crate::discord_token();

	let handler = InlineMathHandler {
		worker: Mutex::new(Worker::spawn().await.expect("spawning Typst worker")),
		settings,
		rendered_messages: Mutex::new(HashMap::new()),
	};
	let intents = GatewayIntents::GUILD_MESSAGES
		| GatewayIntents::DIRECT_MESSAGES
		| GatewayIntents::MESSAGE_CONTENT;
	let mut client = serenity::Client::builder(token.trim(), intents)
		.event_handler(handler)
		.await
		.expect("creating inline math Discord client");

	if let Err(error) = client.start().await {
		eprintln!("ERROR Inline-math Discord client stopped: {error:?}");
	}
}

#[cfg(test)]
mod tests {
	use super::{remove_rendered_message, source_for_message, InlineMathSettings, PREAMBLE};
	use std::collections::HashMap;
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
	async fn source_deletion_controls_skip_inline_render_cleanup_once() {
		let settings = InlineMathSettings::default();
		let inline_settings = settings.clone();
		settings.mark_source_deletion(1, 10).await;

		assert!(inline_settings.consume_source_deletion(1, 10).await);
		assert!(!settings.consume_source_deletion(1, 10).await);

		settings.mark_source_deletion(1, 11).await;
		settings.cancel_source_deletion(1, 11).await;
		assert!(!settings.consume_source_deletion(1, 11).await);
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

	#[test]
	fn deleting_a_source_returns_its_renders_and_forgets_deleted_render_ids() {
		let mut rendered = HashMap::from([
			((1, 10), vec![20, 21]),
			((1, 11), vec![22]),
		]);

		assert_eq!(remove_rendered_message(&mut rendered, 1, 10), vec![20, 21]);
		assert_eq!(remove_rendered_message(&mut rendered, 1, 22), Vec::<u64>::new());
		assert!(rendered.is_empty());
	}
}
