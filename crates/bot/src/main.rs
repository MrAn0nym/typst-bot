#![deny(
	absolute_paths_not_starting_with_crate,
	keyword_idents,
	macro_use_extern_crate,
	meta_variable_misuse,
	missing_abi,
	missing_copy_implementations,
	non_ascii_idents,
	nonstandard_style,
	noop_method_call,
	rust_2018_idioms,
	unused_qualifications
)]
#![warn(clippy::pedantic)]
#![forbid(unsafe_code)]

mod bot;
mod inline_math;
mod render_controls;
mod worker;

const SOURCE_URL: &str = "https://github.com/MrAn0nym/typst-bot";

fn discord_token() -> String {
	let token = std::env::var("DISCORD_TOKEN")
		.unwrap_or_else(|error| panic!("DISCORD_TOKEN environment variable is required: {error}"));
	let token = token.trim();

	if token.is_empty() {
		panic!("DISCORD_TOKEN environment variable cannot be empty");
	}

	token.to_owned()
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
	let inline_math_settings = inline_math::InlineMathSettings::from_environment();
	tokio::join!(
		bot::run(inline_math_settings.clone()),
		inline_math::run(inline_math_settings),
	);
}
