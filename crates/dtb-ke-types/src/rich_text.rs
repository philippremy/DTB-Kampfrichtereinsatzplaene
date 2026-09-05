use serde::{Deserialize, Serialize};

/// Minimal rich text for the free-form block appended to the end of a document.
///
/// Deliberately tiny: character runs carry only bold / italic / underline /
/// text color. No alignment, lists or headings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RichTextDTO {
    pub paragraphs: Vec<RichParagraphDTO>,
}

/// One paragraph: an ordered list of styled text runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RichParagraphDTO {
    pub runs: Vec<RichRunDTO>,
}

/// A contiguous span of text sharing the same styling.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct RichRunDTO {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// `#RRGGBB`, or `None` for the document's default text color.
    pub color: Option<String>,
}
