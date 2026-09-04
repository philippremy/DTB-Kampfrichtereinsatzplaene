//! The `numbering.xml` part — one bullet-list definition, referenced by the
//! `KEBullet` style so the dress-code list is a real Word enumeration (its
//! "–" markers come from here, not from text typed into each line).

use ooxmlsdk::schemas::schemas_openxmlformats_org_wordprocessingml_2006_main as w;
use ooxmlsdk::units::SignedTwipsMeasureValue;

use super::styles::{BULLET_NUM_ID, FONT};

const ABSTRACT_ID: i32 = 0;

pub fn build() -> w::Numbering {
    let level = w::Level {
        level_index: 0,
        numbering_format: Some(w::NumberingFormat {
            val: w::NumberFormatValues::Bullet,
            ..Default::default()
        }),
        // en-dash marker, matching the Typst template's `list(marker: [--])`.
        level_text: Some(w::LevelText {
            val: Some("–".to_string()),
            ..Default::default()
        }),
        level_justification: Some(w::LevelJustification {
            w_val: w::LevelJustificationValues::Left,
        }),
        previous_paragraph_properties: Some(Box::new(w::PreviousParagraphProperties {
            indentation: Some(w::Indentation {
                left: Some(SignedTwipsMeasureValue::Twips(340)),
                hanging: Some(SignedTwipsMeasureValue::Twips(340)),
                ..Default::default()
            }),
            ..Default::default()
        })),
        numbering_symbol_run_properties: Some(Box::new(w::NumberingSymbolRunProperties {
            run_fonts: vec![w::RunFonts {
                ascii: Some(FONT.to_string()),
                high_ansi: Some(FONT.to_string()),
                ..Default::default()
            }],
            ..Default::default()
        })),
        ..Default::default()
    };

    w::Numbering {
        xmlns: vec![ooxmlsdk::common::XmlNamespace::known(
            ooxmlsdk::namespaces::XmlKnownNamespace::W,
        )],
        abstract_num: vec![w::AbstractNum {
            abstract_number_id: ABSTRACT_ID,
            multi_level_type: Some(w::MultiLevelType {
                val: w::MultiLevelValues::SingleLevel,
            }),
            level: vec![level],
            ..Default::default()
        }],
        numbering_instance: vec![w::NumberingInstance {
            number_id: BULLET_NUM_ID,
            abstract_num_id: w::AbstractNumId { val: ABSTRACT_ID },
            ..Default::default()
        }],
        ..Default::default()
    }
}
