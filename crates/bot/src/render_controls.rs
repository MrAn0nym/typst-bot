use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serenity::all::{
	ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateInteractionResponse,
	CreateInteractionResponseMessage, EditInteractionResponse, EditMessage, MessageId, ReactionType, UserId,
};

const CONTROL_LIFETIME_SECS: u64 = 5 * 60;

#[derive(Debug, PartialEq, Eq)]
enum ControlAction {
	DeleteRender,
	DeleteSource { channel_id: u64, message_id: u64 },
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
	source_channel_id: ChannelId,
	source_message_id: MessageId,
) -> RenderControls {
	let expires_at = unix_time_secs() + CONTROL_LIFETIME_SECS;
	let (delete_render, delete_source) = component_custom_ids(
		owner_id.get(),
		source_channel_id.get(),
		source_message_id.get(),
		expires_at,
	);

	RenderControls {
		components: vec![CreateActionRow::Buttons(vec![
			CreateButton::new(delete_render)
				.style(ButtonStyle::Secondary)
				.emoji(ReactionType::Unicode("🗑️".to_owned())),
			CreateButton::new(delete_source)
				.style(ButtonStyle::Secondary)
				.emoji(ReactionType::Unicode("📜".to_owned())),
		])],
		expires_at,
	}
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
			tracing::warn!(?error, "Failed to remove expired render controls");
		}
	});
}

pub(crate) async fn handle_component_interaction(
	ctx: &Context,
	interaction: &ComponentInteraction,
) -> serenity::Result<()> {
	let Some(control) = parse_component_id(&interaction.data.custom_id) else {
		return Ok(());
	};

	interaction
		.create_response(
			&ctx.http,
			CreateInteractionResponse::Defer(
				CreateInteractionResponseMessage::new().ephemeral(true),
			),
		)
		.await?;

	let response = if controls_expired(control.expires_at, unix_time_secs()) {
		"These render controls have expired.".to_owned()
	} else if interaction.user.id.get() != control.owner_id {
		"Only the user who created this render can use its controls.".to_owned()
	} else {
		let deletion = match control.action {
			ControlAction::DeleteRender => interaction.message.delete(&ctx.http).await,
			ControlAction::DeleteSource {
				channel_id,
				message_id,
			} => {
				ChannelId::new(channel_id)
					.delete_message(&ctx.http, MessageId::new(message_id))
					.await
			}
		};

		match deletion {
			Ok(()) => "Deleted.".to_owned(),
			Err(error) => {
				tracing::warn!(?error, "Failed to delete a message using render controls");
				"Unable to delete the message. Please check the bot's permissions.".to_owned()
			}
		}
	};

	interaction
		.edit_response(&ctx.http, EditInteractionResponse::new().content(response))
		.await?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{
		component_custom_ids, controls_expired, parse_component_id, render_attribution,
		ControlAction, ControlId, CONTROL_LIFETIME_SECS,
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
	fn controls_expire_at_five_minutes() {
		assert_eq!(CONTROL_LIFETIME_SECS, 300);
		assert!(!controls_expired(500, 499));
		assert!(controls_expired(500, 500));
	}
}
