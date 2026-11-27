use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serenity::all::{
	ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow, CreateAttachment,
	CreateButton, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
	EditInteractionResponse, EditMessage, MessageId, ReactionType, UserId,
};

use crate::inline_math::InlineMathSettings;

const CONTROL_LIFETIME_SECS: u64 = 5 * 60;

#[derive(Debug, PartialEq, Eq)]
enum ControlAction {
	DeleteRender,
	PublishRender,
	DeleteSource { channel_id: u64, message_id: u64 },
	ConfirmDeleteRender { channel_id: u64, message_id: u64 },
	ConfirmDeleteSource { channel_id: u64, message_id: u64 },
	CancelDeletion,
}

#[derive(Debug, PartialEq, Eq)]
struct ControlId {
	owner_id: u64,
	expires_at: u64,
	action: ControlAction,
}

pub(crate) struct RenderControls {
	pub components: Vec<CreateActionRow>,
	pub expires_at: u64,
}

pub(crate) fn render_attribution(user_id: u64) -> String {
	format!("**<@{user_id}>**")
}

pub(crate) fn create_components(
	owner_id: UserId,
	source_author_id: UserId,
	source_channel_id: ChannelId,
	source_message_id: MessageId,
) -> RenderControls {
	let expires_at = unix_time_secs() + CONTROL_LIFETIME_SECS;
	let delete_render = format!("typst:v1:{expires_at}:{}:render", owner_id.get());
	let mut buttons = vec![
		CreateButton::new(delete_render)
			.style(ButtonStyle::Secondary)
			.emoji(ReactionType::Unicode("🗑️".to_owned())),
	];

	if owner_id == source_author_id {
		let delete_source = component_custom_ids(
			owner_id.get(),
			source_channel_id.get(),
			source_message_id.get(),
			expires_at,
		)
		.1;
		buttons.push(
			CreateButton::new(delete_source)
				.style(ButtonStyle::Secondary)
				.emoji(ReactionType::Unicode("📜".to_owned())),
		);
	}

	RenderControls {
		components: vec![CreateActionRow::Buttons(buttons)],
		expires_at,
	}
}

pub(crate) fn create_render_components(owner_id: UserId) -> RenderControls {
	let expires_at = unix_time_secs() + CONTROL_LIFETIME_SECS;
	let custom_id = format!("typst:v1:{expires_at}:{}:render", owner_id.get());

	RenderControls {
		components: vec![CreateActionRow::Buttons(vec![CreateButton::new(custom_id)
			.style(ButtonStyle::Secondary)
			.emoji(ReactionType::Unicode("🗑️".to_owned()))])],
		expires_at,
	}
}

pub(crate) fn create_private_render_components(owner_id: UserId) -> RenderControls {
	let expires_at = unix_time_secs() + CONTROL_LIFETIME_SECS;
	let (delete_render, publish_render) = private_component_custom_ids(owner_id.get(), expires_at);

	RenderControls {
		components: vec![CreateActionRow::Buttons(vec![
			CreateButton::new(publish_render)
				.style(ButtonStyle::Primary)
				.label("Publish"),
			CreateButton::new(delete_render)
				.style(ButtonStyle::Secondary)
				.emoji(ReactionType::Unicode("🗑️".to_owned())),
		])],
		expires_at,
	}
}

fn private_component_custom_ids(owner_id: u64, expires_at: u64) -> (String, String) {
	(
		format!("typst:v1:{expires_at}:{owner_id}:render"),
		format!("typst:v1:{expires_at}:{owner_id}:publish"),
	)
}

fn component_custom_ids(
	owner_id: u64,
	channel_id: u64,
	message_id: u64,
	expires_at: u64,
) -> (String, String) {
	(
		format!("typst:v1:{expires_at}:{owner_id}:render"),
		format!("typst:v1:{expires_at}:{owner_id}:source:{channel_id}:{message_id}"),
	)
}

fn parse_component_id(custom_id: &str) -> Option<ControlId> {
	let mut parts = custom_id.split(':');
	if parts.next()? != "typst" || parts.next()? != "v1" {
		return None;
	}

	let expires_at = parts.next()?.parse().ok()?;
	let owner_id = parts.next()?.parse().ok()?;
	let action = match parts.next()? {
		"render" if parts.next().is_none() => ControlAction::DeleteRender,
		"publish" if parts.next().is_none() => ControlAction::PublishRender,
		"cancel" if parts.next().is_none() => ControlAction::CancelDeletion,
		"source" => {
			let channel_id = parts.next()?.parse().ok()?;
			let message_id = parts.next()?.parse().ok()?;
			if parts.next().is_some() {
				return None;
			}
			ControlAction::DeleteSource {
				channel_id,
				message_id,
			}
		}
		"confirm-render" => {
			let channel_id = parts.next()?.parse().ok()?;
			let message_id = parts.next()?.parse().ok()?;
			if parts.next().is_some() {
				return None;
			}
			ControlAction::ConfirmDeleteRender {
				channel_id,
				message_id,
			}
		}
		"confirm-source" => {
			let channel_id = parts.next()?.parse().ok()?;
			let message_id = parts.next()?.parse().ok()?;
			if parts.next().is_some() {
				return None;
			}
			ControlAction::ConfirmDeleteSource {
				channel_id,
				message_id,
			}
		}
		_ => return None,
	};

	Some(ControlId {
		owner_id,
		expires_at,
		action,
	})
}

fn controls_expired(expires_at: u64, now: u64) -> bool {
	now >= expires_at
}

fn unix_time_secs() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.expect("system clock is before the Unix epoch")
		.as_secs()
}

pub(crate) fn schedule_expiration(
	ctx: &Context,
	channel_id: ChannelId,
	message_id: MessageId,
	expires_at: u64,
) {
	let http = ctx.http.clone();
	tokio::spawn(async move {
		let now = unix_time_secs();
		tokio::time::sleep(Duration::from_secs(expires_at.saturating_sub(now))).await;

		if let Err(error) = channel_id
			.edit_message(&http, message_id, EditMessage::new().components(Vec::new()))
			.await
		{
			eprintln!("WARN Failed to remove expired render controls: {error:?}");
		}
	});
}

pub(crate) async fn handle_component_interaction(
	ctx: &Context,
	interaction: &ComponentInteraction,
	inline_math_settings: &InlineMathSettings,
) -> serenity::Result<()> {
	let Some(control) = parse_component_id(&interaction.data.custom_id) else {
		return Ok(());
	};

	if controls_expired(control.expires_at, unix_time_secs()) {
		return respond_ephemerally(ctx, interaction, "These render controls have expired.").await;
	}
	if interaction.user.id.get() != control.owner_id {
		return respond_ephemerally(
			ctx,
			interaction,
			"Only the user who created this render can use its controls.",
		)
		.await;
	}

	if control.action == ControlAction::PublishRender {
		interaction
			.create_response(
				&ctx.http,
				CreateInteractionResponse::UpdateMessage(
					CreateInteractionResponseMessage::new()
						.content("Publishing preview...")
						.components(Vec::new()),
				),
			)
			.await?;

		let response = match publish_render(ctx, interaction).await {
			Ok(()) => "Published to the channel.".to_owned(),
			Err(error) => {
				eprintln!("WARN Failed to publish private render: {error:?}");
				let controls = create_private_render_components(interaction.user.id);
				interaction
					.edit_response(
						&ctx.http,
						EditInteractionResponse::new()
							.content(
								"Unable to publish this render. It remains private; please try again.",
							)
							.components(controls.components),
					)
					.await?;
				return Ok(());
			}
		};

		interaction
			.edit_response(
				&ctx.http,
				EditInteractionResponse::new()
					.content(response)
					.components(Vec::new()),
			)
			.await?;
		return Ok(());
	}

	match &control.action {
		ControlAction::DeleteRender => {
			let confirm_id = format!(
				"typst:v1:{}:{}:confirm-render:{}:{}",
				control.expires_at,
				control.owner_id,
				interaction.channel_id.get(),
				interaction.message.id.get(),
			);
			return prompt_deletion_confirmation(
				ctx,
				interaction,
				"Are you sure you want to delete this render?",
				confirm_id,
				control.owner_id,
				control.expires_at,
				false,
			)
			.await;
		}
		ControlAction::DeleteSource {
			channel_id,
			message_id,
		} => {
			interaction
				.create_response(
					&ctx.http,
					CreateInteractionResponse::Defer(
						CreateInteractionResponseMessage::new().ephemeral(true),
					),
				)
				.await?;
			let source_message = match ChannelId::new(*channel_id)
				.message(&ctx.http, MessageId::new(*message_id))
				.await
			{
				Ok(message) => message,
				Err(error) => {
					eprintln!("WARN Failed to validate source message for deletion: {error:?}");
					interaction
						.edit_response(
							&ctx.http,
							EditInteractionResponse::new()
								.content("Unable to verify the original message."),
						)
						.await?;
					return Ok(());
				}
			};
			if source_message.author.id.get() != control.owner_id {
				interaction
					.edit_response(
						&ctx.http,
						EditInteractionResponse::new()
							.content("You can only delete messages that you authored."),
					)
					.await?;
				return Ok(());
			}

			let confirm_id = format!(
				"typst:v1:{}:{}:confirm-source:{channel_id}:{message_id}",
				control.expires_at, control.owner_id,
			);
			return prompt_deletion_confirmation(
				ctx,
				interaction,
				"Are you sure you want to delete the original message?",
				confirm_id,
				control.owner_id,
				control.expires_at,
				true,
			)
			.await;
		}
		ControlAction::CancelDeletion => {
			interaction
				.create_response(
					&ctx.http,
					CreateInteractionResponse::UpdateMessage(
						CreateInteractionResponseMessage::new()
							.content("Deletion cancelled.")
							.components(Vec::new()),
					),
				)
				.await?;
			return Ok(());
		}
		ControlAction::ConfirmDeleteRender { .. }
		| ControlAction::ConfirmDeleteSource { .. } => {}
		ControlAction::PublishRender => unreachable!(),
	}

	interaction
		.create_response(
			&ctx.http,
			CreateInteractionResponse::Defer(
				CreateInteractionResponseMessage::new().ephemeral(true),
			),
		)
		.await?;

	let deletion = match control.action {
		ControlAction::ConfirmDeleteRender {
			channel_id,
			message_id,
		} => ChannelId::new(channel_id)
			.delete_message(&ctx.http, MessageId::new(message_id))
			.await,
		ControlAction::ConfirmDeleteSource {
			channel_id,
			message_id,
		} => {
			inline_math_settings
				.mark_source_deletion(channel_id, message_id)
				.await;
			let deletion = ChannelId::new(channel_id)
				.delete_message(&ctx.http, MessageId::new(message_id))
				.await;
			if deletion.is_err() {
				inline_math_settings
					.cancel_source_deletion(channel_id, message_id)
					.await;
			}
			deletion
		}
		_ => unreachable!(),
	};

	let response = match deletion {
		Ok(()) => "Deleted.".to_owned(),
		Err(error) => {
			eprintln!("WARN Failed to delete a message using render controls: {error:?}");
			"Unable to delete the message. Please check the bot's permissions.".to_owned()
		}
	};

	interaction
		.edit_response(&ctx.http, EditInteractionResponse::new().content(response))
		.await?;
	Ok(())
}

async fn prompt_deletion_confirmation(
	ctx: &Context,
	interaction: &ComponentInteraction,
	content: &str,
	confirm_id: String,
	owner_id: u64,
	expires_at: u64,
	already_deferred: bool,
) -> serenity::Result<()> {
	let cancel_id = format!("typst:v1:{expires_at}:{owner_id}:cancel");
	let components = vec![CreateActionRow::Buttons(vec![
		CreateButton::new(confirm_id)
			.style(ButtonStyle::Danger)
			.label("Delete"),
		CreateButton::new(cancel_id)
			.style(ButtonStyle::Secondary)
			.label("Cancel"),
	])];
	if already_deferred {
		interaction
			.edit_response(
				&ctx.http,
				EditInteractionResponse::new()
					.content(content)
					.components(components),
			)
			.await?;
	} else {
		interaction
			.create_response(
				&ctx.http,
				CreateInteractionResponse::Message(
					CreateInteractionResponseMessage::new()
						.content(content)
						.ephemeral(true)
						.components(components),
				),
			)
			.await?;
	}
	Ok(())
}

async fn respond_ephemerally(
	ctx: &Context,
	interaction: &ComponentInteraction,
	content: &str,
) -> serenity::Result<()> {
	interaction
		.create_response(
			&ctx.http,
			CreateInteractionResponse::Defer(
				CreateInteractionResponseMessage::new().ephemeral(true),
			),
		)
		.await?;
	interaction
		.edit_response(&ctx.http, EditInteractionResponse::new().content(content))
		.await?;
	Ok(())
}

async fn publish_render(
	ctx: &Context,
	interaction: &ComponentInteraction,
) -> serenity::Result<()> {
	let controls = create_render_components(interaction.user.id);
	let mut message = CreateMessage::new()
		.content(interaction.message.content.clone())
		.allowed_mentions(serenity::all::CreateAllowedMentions::new())
		.components(controls.components);

	for attachment in &interaction.message.attachments {
		message = message.add_file(CreateAttachment::url(&ctx.http, &attachment.url).await?);
	}

	let published = interaction
		.channel_id
		.send_message(&ctx.http, message)
		.await?;
	schedule_expiration(ctx, published.channel_id, published.id, controls.expires_at);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{
		component_custom_ids, controls_expired, parse_component_id, private_component_custom_ids,
		render_attribution, ControlAction, ControlId, CONTROL_LIFETIME_SECS,
	};

	#[test]
	fn render_attribution_is_bold_message_text() {
		assert_eq!(render_attribution(42), "**<@42>**");
	}

	#[test]
	fn component_ids_identify_both_delete_actions_and_their_owner() {
		let (delete_render, delete_source) = component_custom_ids(42, 100, 200, 500);

		assert_eq!(
			parse_component_id(&delete_render),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::DeleteRender,
			}),
		);
		assert_eq!(
			parse_component_id(&delete_source),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::DeleteSource {
					channel_id: 100,
					message_id: 200,
				},
			}),
		);
	}

	#[test]
	fn malformed_component_ids_are_ignored() {
		assert_eq!(parse_component_id("typst:v1:bad"), None);
		assert_eq!(parse_component_id("other:v1:500:42:render"), None);
	}

	#[test]
	fn confirmation_ids_preserve_the_message_target() {
		assert_eq!(
			parse_component_id("typst:v1:500:42:confirm-render:100:200"),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::ConfirmDeleteRender {
					channel_id: 100,
					message_id: 200,
				},
			}),
		);
		assert_eq!(
			parse_component_id("typst:v1:500:42:confirm-source:300:400"),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::ConfirmDeleteSource {
					channel_id: 300,
					message_id: 400,
				},
			}),
		);
		assert_eq!(
			parse_component_id("typst:v1:500:42:cancel"),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::CancelDeletion,
			}),
		);
	}

	#[test]
	fn private_render_controls_include_owner_bound_publish_action() {
		let (_, publish_id) = private_component_custom_ids(42, 500);

		assert_eq!(
			parse_component_id(&publish_id),
			Some(ControlId {
				owner_id: 42,
				expires_at: 500,
				action: ControlAction::PublishRender,
			}),
		);
		assert_eq!(
			parse_component_id("typst:v1:500:42:publish:extra"),
			None,
		);
	}

	#[test]
	fn controls_expire_at_five_minutes() {
		assert_eq!(CONTROL_LIFETIME_SECS, 300);
		assert!(!controls_expired(500, 499));
		assert!(controls_expired(500, 500));
	}
}
