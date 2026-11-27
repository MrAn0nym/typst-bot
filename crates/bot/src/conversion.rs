const MAX_INPUT_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversionDirection {
	LatexToTypst,
	TypstToLatex,
}

impl ConversionDirection {
	pub(crate) const fn input_extension(self) -> &'static str {
		match self {
			Self::LatexToTypst => "tex",
			Self::TypstToLatex => "typ",
		}
	}

	pub(crate) const fn output_filename(self) -> &'static str {
		match self {
			Self::LatexToTypst => "converted.typ",
			Self::TypstToLatex => "converted.tex",
		}
	}

	pub(crate) const fn output_language(self) -> &'static str {
		match self {
			Self::LatexToTypst => "typ",
			Self::TypstToLatex => "tex",
		}
	}
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ConversionInputError {
	#[error("Provide source text or attach a .{0} file.")]
	MissingSource(&'static str),
	#[error("Provide either source text or a file attachment, not both.")]
	MultipleSources,
	#[error("Input must be no larger than 1 MiB.")]
	TooLarge,
	#[error("This command expects a .{0} file.")]
	WrongFileType(&'static str),
	#[error("Input must be valid UTF-8 text.")]
	InvalidUtf8,
	#[error("Input cannot be empty.")]
	Empty,
	#[error("The attachment could not be downloaded. Please try again.")]
	AttachmentDownloadFailed,
}

pub(crate) fn convert_document(direction: ConversionDirection, source: &str) -> String {
	match direction {
		ConversionDirection::LatexToTypst => tylax::latex_document_to_typst(source),
		ConversionDirection::TypstToLatex => tylax::typst_document_to_latex(source),
	}
}

pub(crate) fn validate_attachment(
	direction: ConversionDirection,
	filename: &str,
	size: usize,
) -> Result<(), ConversionInputError> {
	validate_size(size)?;

	let expected_extension = direction.input_extension();
	if !std::path::Path::new(filename)
		.extension()
		.and_then(|extension| extension.to_str())
		.is_some_and(|extension| extension.eq_ignore_ascii_case(expected_extension))
	{
		return Err(ConversionInputError::WrongFileType(expected_extension));
	}

	Ok(())
}

pub(crate) fn validate_source(source: String) -> Result<String, ConversionInputError> {
	validate_size(source.len())?;
	if source.trim().is_empty() {
		return Err(ConversionInputError::Empty);
	}

	Ok(source)
}

pub(crate) fn decode_attachment(bytes: Vec<u8>) -> Result<String, ConversionInputError> {
	validate_size(bytes.len())?;
	let source = String::from_utf8(bytes).map_err(|_| ConversionInputError::InvalidUtf8)?;
	validate_source(source)
}

fn validate_size(size: usize) -> Result<(), ConversionInputError> {
	if size > MAX_INPUT_BYTES {
		return Err(ConversionInputError::TooLarge);
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::{
		convert_document, decode_attachment, validate_attachment, validate_source, ConversionDirection,
		ConversionInputError, MAX_INPUT_BYTES,
	};

	#[test]
	fn converts_latex_documents_to_typst() {
		let source = r#"\documentclass{article}
\begin{document}
\section{Introduction}
Hello, world!
\end{document}"#;

		let converted = convert_document(ConversionDirection::LatexToTypst, source);

		assert!(converted.contains("Introduction"));
		assert!(converted.contains("Hello, world!"));
	}

	#[test]
	fn converts_typst_documents_to_latex() {
		let source = "= Introduction\n\nHello, *world*!";

		let converted = convert_document(ConversionDirection::TypstToLatex, source);

		assert!(converted.contains("Introduction"));
		assert!(converted.contains("Hello"));
		assert!(converted.contains(r"\section"));
	}

	#[test]
	fn accepts_matching_attachment_extensions_without_case_sensitivity() {
		assert!(validate_attachment(ConversionDirection::LatexToTypst, "PAPER.TEX", 10).is_ok());
		assert!(validate_attachment(ConversionDirection::TypstToLatex, "PAPER.TYP", 10).is_ok());
	}

	#[test]
	fn rejects_attachment_with_wrong_extension_or_oversized_input() {
		assert!(validate_attachment(ConversionDirection::LatexToTypst, "paper.typ", 10).is_err());
		assert!(
			validate_attachment(ConversionDirection::TypstToLatex, "paper.typ", 1_048_577).is_err()
		);
	}

	#[test]
	fn rejects_empty_or_oversized_inline_source() {
		assert_eq!(
			validate_source(" \n".to_owned()),
			Err(ConversionInputError::Empty),
		);
		assert_eq!(
			validate_source("x".repeat(MAX_INPUT_BYTES + 1)),
			Err(ConversionInputError::TooLarge),
		);
	}

	#[test]
	fn rejects_non_utf8_attachment_contents() {
		assert_eq!(
			decode_attachment(vec![0xff]),
			Err(ConversionInputError::InvalidUtf8),
		);
	}

	#[test]
	fn accepts_source_at_the_size_limit() {
		assert!(validate_source("x".repeat(MAX_INPUT_BYTES)).is_ok());
	}
}
