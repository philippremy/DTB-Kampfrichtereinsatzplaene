//! Optional embedded fonts.
//!
//! ECMA-376 §17.8: an embedded font is an *obfuscated* TrueType part
//! (`application/vnd.openxmlformats-officedocument.obfuscatedFont`, `.odttf`) —
//! the first 32 bytes XOR-scrambled with a key derived from a per-font GUID
//! (`w:fontKey`). `ooxmlsdk` provides the parts and the `w:font` / `w:embed*`
//! schema; the obfuscation itself is the only thing we add.
//!
//! Two `w:font` families go in: the four `Archivo` weights (body text) and the
//! single `Archivo Condensed ExtraBold` face used for the title — matching the
//! Typst template, which reaches the condensed cut via `weight: "extrabold",
//! stretch: 75%`.

use ooxmlsdk::parts::font_part::FontPart;
use ooxmlsdk::parts::font_table_part::FontTablePart;
use ooxmlsdk::parts::main_document_part::MainDocumentPart;
use ooxmlsdk::parts::wordprocessing_document::WordprocessingDocument;
use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;
use uuid::Uuid;

use super::styles::{FONT, TITLE_FONT};
use crate::ExportError;

const OBFUSCATED_FONT: &str = "application/vnd.openxmlformats-officedocument.obfuscatedFont";

/// The four Archivo weights, in `dtb-ke-resource`.
const WEIGHTS: [(&str, Slot); 4] = [
    ("fonts/Archivo-Regular.ttf", Slot::Regular),
    ("fonts/Archivo-Bold.ttf", Slot::Bold),
    ("fonts/Archivo-Italic.ttf", Slot::Italic),
    ("fonts/Archivo-BoldItalic.ttf", Slot::BoldItalic),
];

/// The condensed extra-bold cut used for the title.
const TITLE_WEIGHT: &str = "fonts/Archivo_Condensed-ExtraBold.ttf";

#[derive(Clone, Copy)]
enum Slot {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

/// Obfuscate a TrueType/OpenType font per ECMA-376 §17.8.1: XOR the first 32
/// bytes with `reverse(guid_bytes)` repeated. `guid` is the value that goes
/// into `w:fontKey` (as `{UPPER-HEX}`).
fn obfuscate(font: &[u8], guid: &Uuid) -> Vec<u8> {
    let mut out = font.to_vec();
    let key: Vec<u8> = guid.as_bytes().iter().rev().copied().collect(); // 16 bytes
    for (i, byte) in out.iter_mut().take(32).enumerate() {
        *byte ^= key[i % 16];
    }
    out
}

/// Obfuscate `path`'s bytes into a new font part and return `(rel_id, fontKey)`
/// for a `w:embed*` element. `None` if the file isn't committed.
fn embed_part(
    docx: &mut WordprocessingDocument,
    font_table: &FontTablePart,
    path: &str,
    rel_id: String,
) -> Result<Option<(String, String)>, ExportError> {
    let Some(bytes) = dtb_ke_resource::get(path) else {
        return Ok(None);
    };
    let guid = Uuid::new_v4();
    let part: FontPart = font_table.add_new_part_with_content_type_and_extension(
        docx,
        rel_id.clone(),
        OBFUSCATED_FONT,
        "odttf",
    )?;
    part.set_data(docx, obfuscate(&bytes, &guid))?;
    let key = format!("{{{}}}", guid.hyphenated().to_string().to_uppercase());
    Ok(Some((rel_id, key)))
}

/// Attach the Archivo family + the condensed title face as obfuscated font
/// parts, a `fontTable.xml` mapping them onto their families, and the
/// `w:embedTrueTypeFonts` settings flag. No-op (returns `Ok`) if a weight is
/// not committed.
pub fn embed(docx: &mut WordprocessingDocument, main: MainDocumentPart) -> Result<(), ExportError> {
    use ooxmlsdk::parts::document_settings_part::DocumentSettingsPart;

    let font_table: FontTablePart = main.add_new_part_auto_id(docx)?;

    // ── Archivo family (body) ──────────────────────────────────────────────
    let mut archivo = w::Font {
        name: FONT.to_string(),
        ..Default::default()
    };
    for (idx, (path, slot)) in WEIGHTS.iter().enumerate() {
        let Some((rel_id, key)) =
            embed_part(docx, &font_table, path, format!("rIdFont{}", idx + 1))?
        else {
            return Ok(()); // a weight is missing — skip embedding entirely
        };
        match slot {
            Slot::Regular => {
                archivo.embed_regular_font = Some(w::EmbedRegularFont {
                    font_key: Some(key),
                    id: rel_id,
                    ..Default::default()
                })
            }
            Slot::Bold => {
                archivo.embed_bold_font = Some(w::EmbedBoldFont {
                    font_key: Some(key),
                    id: rel_id,
                    ..Default::default()
                })
            }
            Slot::Italic => {
                archivo.embed_italic_font = Some(w::EmbedItalicFont {
                    font_key: Some(key),
                    id: rel_id,
                    ..Default::default()
                })
            }
            Slot::BoldItalic => {
                archivo.embed_bold_italic_font = Some(w::EmbedBoldItalicFont {
                    font_key: Some(key),
                    id: rel_id,
                    ..Default::default()
                })
            }
        }
    }

    let mut fonts = vec![w::FontsChoice::Font(Box::new(archivo))];

    // ── Archivo Condensed ExtraBold (title) ────────────────────────────────
    // Its single face is wired as both regular and bold so `KETitle`'s `<w:b/>`
    // fallback resolves to the real ExtraBold cut rather than a synthesised one.
    if let Some((rel_id, key)) =
        embed_part(docx, &font_table, TITLE_WEIGHT, "rIdFont5".to_string())?
    {
        fonts.push(w::FontsChoice::Font(Box::new(w::Font {
            name: TITLE_FONT.to_string(),
            embed_regular_font: Some(w::EmbedRegularFont {
                font_key: Some(key.clone()),
                id: rel_id.clone(),
                ..Default::default()
            }),
            embed_bold_font: Some(w::EmbedBoldFont {
                font_key: Some(key),
                id: rel_id,
                ..Default::default()
            }),
            ..Default::default()
        })));
    }

    font_table.set_root_element(
        docx,
        w::Fonts {
            xmlns: namespaces(),
            xml_children: fonts,
            ..Default::default()
        },
    )?;

    let settings: DocumentSettingsPart = main.add_new_part_auto_id(docx)?;
    settings.set_root_element(
        docx,
        w::Settings {
            xmlns: vec![ooxmlsdk::common::XmlNamespace::known(
                ooxmlsdk::namespaces::XmlKnownNamespace::W,
            )],
            embed_true_type_fonts: Some(w::EmbedTrueTypeFonts::default()),
            save_subset_fonts: Some(w::SaveSubsetFonts {
                val: Some(ooxmlsdk::simple_type::OnOffValue::from(false)),
            }),
            ..Default::default()
        },
    )?;

    Ok(())
}

fn namespaces() -> Vec<ooxmlsdk::common::XmlNamespace> {
    use ooxmlsdk::common::XmlNamespace;
    use ooxmlsdk::namespaces::XmlKnownNamespace as N;
    vec![XmlNamespace::known(N::W), XmlNamespace::known(N::R)]
}
