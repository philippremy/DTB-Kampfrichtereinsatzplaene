use dtb_ke_types::{RichParagraphDTO, RichRunDTO, RichTextDTO};

/// Editor-side mirror of [`RichTextDTO`]. Structurally identical today; kept
/// separate so the eventual text editor can attach cursor/selection state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RichText {
    pub paragraphs: Vec<Paragraph>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paragraph {
    pub runs: Vec<Run>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl From<RichTextDTO> for RichText {
    fn from(dto: RichTextDTO) -> Self {
        Self {
            paragraphs: dto.paragraphs.into_iter().map(Paragraph::from).collect(),
        }
    }
}

impl From<RichParagraphDTO> for Paragraph {
    fn from(dto: RichParagraphDTO) -> Self {
        Self {
            runs: dto.runs.into_iter().map(Run::from).collect(),
        }
    }
}

impl From<RichRunDTO> for Run {
    fn from(dto: RichRunDTO) -> Self {
        Self {
            text: dto.text,
            bold: dto.bold,
            italic: dto.italic,
            underline: dto.underline,
        }
    }
}

impl From<&RichText> for RichTextDTO {
    fn from(model: &RichText) -> Self {
        Self {
            paragraphs: model
                .paragraphs
                .iter()
                .map(RichParagraphDTO::from)
                .collect(),
        }
    }
}

impl From<&Paragraph> for RichParagraphDTO {
    fn from(model: &Paragraph) -> Self {
        Self {
            runs: model.runs.iter().map(RichRunDTO::from).collect(),
        }
    }
}

impl From<&Run> for RichRunDTO {
    fn from(model: &Run) -> Self {
        Self {
            text: model.text.clone(),
            bold: model.bold,
            italic: model.italic,
            underline: model.underline,
        }
    }
}
