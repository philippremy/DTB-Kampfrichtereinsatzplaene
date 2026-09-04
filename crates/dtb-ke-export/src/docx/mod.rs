//! DOCX export.
//!
//! A Word-idiomatic document — the same content and structure as the Typst
//! PDF, but laid out the Word way (named styles in `styles.xml`, normal flow,
//! a header, Word table styling). Built on `ooxmlsdk`, whose generated schema
//! types map 1:1 to ECMA-376, the same model the .NET Open XML SDK exposes.
//!
//! `..Default::default()` on the generated schema structs is a deliberate
//! forward-compatible idiom even where it is currently exhaustive; style
//! construction reads best as sequential pushes.
#![allow(clippy::needless_update, clippy::vec_init_then_push)]

mod content;
mod dsl;
mod fonts;
mod header;
mod image;
mod numbering;
mod render;
mod styles;

pub(crate) use header::HeaderLogos;

use ooxmlsdk::parts::wordprocessing_document::WordprocessingDocument;
use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;
use ooxmlsdk::sdk::WordprocessingDocumentType;

use crate::model::DocumentModel;
use crate::{DocxExport, ExportError};

impl From<ooxmlsdk::common::SdkError> for ExportError {
    fn from(e: ooxmlsdk::common::SdkError) -> Self {
        ExportError::Template(format!("DOCX: {e}"))
    }
}

/// Build a `.docx` package for the given digested document. `logos` carries the
/// header artwork (org emblem + swoosh) as SVG bytes; pass an empty
/// [`HeaderLogos`] for no header.
pub(crate) fn build(
    model: &DocumentModel,
    options: &DocxExport,
    logos: &HeaderLogos<'_>,
) -> Result<Vec<u8>, ExportError> {
    use ooxmlsdk::parts::numbering_definitions_part::NumberingDefinitionsPart;
    use ooxmlsdk::parts::style_definitions_part::StyleDefinitionsPart;

    let mut docx = WordprocessingDocument::create(WordprocessingDocumentType::Document);
    let main = docx.add_main_document_part()?;

    // The header part (and its image parts) must exist before the body, whose
    // section properties reference it by relationship id.
    let header_rid = header::build(&mut docx, &main, logos)?;

    let document = w::Document {
        xmlns: default_namespaces(),
        body: Some(Box::new(content::build_body(model, header_rid.as_deref()))),
        ..Default::default()
    };
    main.set_root_element(&mut docx, document)?;

    let styles_part: StyleDefinitionsPart = main.add_new_part_auto_id(&mut docx)?;
    styles_part.set_root_element(&mut docx, with_namespaces_styles(styles::build()))?;

    let numbering_part: NumberingDefinitionsPart = main.add_new_part_auto_id(&mut docx)?;
    numbering_part.set_root_element(&mut docx, numbering::build())?;

    if options.embed_fonts {
        fonts::embed(&mut docx, main)?;
    }

    Ok(docx.to_package_bytes()?)
}

fn default_namespaces() -> Vec<ooxmlsdk::common::XmlNamespace> {
    use ooxmlsdk::common::XmlNamespace;
    use ooxmlsdk::namespaces::XmlKnownNamespace as N;
    vec![XmlNamespace::known(N::W), XmlNamespace::known(N::R)]
}

fn with_namespaces_styles(mut styles: w::Styles) -> w::Styles {
    styles.xmlns = vec![ooxmlsdk::common::XmlNamespace::known(
        ooxmlsdk::namespaces::XmlKnownNamespace::W,
    )];
    styles
}
