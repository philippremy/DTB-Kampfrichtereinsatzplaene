//! Presentation metadata for a judging table: the canonical discipline name
//! shown as the italic sub-heading, and the ordered role rows (short
//! abbreviation + assigned judge) exactly as they appear in the document.
//!
//! This is document-side vocabulary — deliberately separate from
//! `dtb-ke-ui`'s `model::roles` (which uses spaced field labels like `"SK 1"`
//! for the editor). Here the abbreviations are the compact `"SK1"` form the
//! printed plan uses.

use dtb_ke_types::{CyrWheelTableDTO, GymWheelTableDTO, JudgingTableKindDTO};

/// The canonical German discipline name (italic sub-heading under the
/// user-defined table label).
pub fn canonical_name(kind: &JudgingTableKindDTO) -> &'static str {
    use CyrWheelTableDTO as C;
    use GymWheelTableDTO as G;
    match kind {
        JudgingTableKindDTO::Cyr(C::Artistic(_)) => "Cyr-Wheel – Artistik",
        JudgingTableKindDTO::Cyr(C::Technical(_)) => "Cyr-Wheel – Technik",
        JudgingTableKindDTO::Gym(G::STL(_)) => "Geradeturnen",
        JudgingTableKindDTO::Gym(G::STLM(_)) => "Geradeturnen auf Musik",
        JudgingTableKindDTO::Gym(G::SPI(_)) => "Spiraleturnen",
        JudgingTableKindDTO::Gym(G::VLT(_)) => "Sprung",
    }
}

/// The judge rows of a table, in print order: `(role abbreviation, judge name)`.
///
/// `OK` = Oberkampfrichter, `SKn` = Schwierigkeitskampfrichter,
/// `AKn` = Ausführungskampfrichter, `AIKn` = Kampfrichter für den
/// Künstlerischen Ausdruck.
pub fn role_rows(kind: &JudgingTableKindDTO) -> Vec<(&'static str, &str)> {
    use CyrWheelTableDTO as C;
    use GymWheelTableDTO as G;
    use JudgingTableKindDTO as K;

    match kind {
        K::Cyr(C::Artistic(t)) => vec![
            ("OK", t.head.as_str()),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AIK1", &t.art1),
            ("AIK2", &t.art2),
            ("AIK3", &t.art3),
            ("AIK4", &t.art4),
        ],
        K::Cyr(C::Technical(t)) => vec![
            ("OK", &t.head),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AK1", &t.exec1),
            ("AK2", &t.exec2),
            ("AK3", &t.exec3),
            ("AK4", &t.exec4),
        ],
        K::Gym(G::STL(t)) => vec![
            ("OK", &t.head),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AK1", &t.exec1),
            ("AK2", &t.exec2),
            ("AK3", &t.exec3),
            ("AK4", &t.exec4),
        ],
        K::Gym(G::STLM(t)) => vec![
            ("OK", &t.head),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AK1", &t.exec1),
            ("AK2", &t.exec2),
            ("AK3", &t.exec3),
            ("AK4", &t.exec4),
            ("AIK1", &t.art1),
            ("AIK2", &t.art2),
            ("AIK3", &t.art3),
            ("AIK4", &t.art4),
        ],
        K::Gym(G::SPI(t)) => vec![
            ("OK", &t.head),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AK1", &t.exec1),
            ("AK2", &t.exec2),
            ("AK3", &t.exec3),
            ("AK4", &t.exec4),
        ],
        K::Gym(G::VLT(t)) => vec![
            ("OK", &t.head),
            ("SK1", &t.diff1),
            ("SK2", &t.diff2),
            ("AK1", &t.exec1),
            ("AK2", &t.exec2),
            ("AK3", &t.exec3),
            ("AK4", &t.exec4),
        ],
    }
}

/// The tallest table variant (STL-M) has this many judge rows. The template's
/// pagination is tuned so two of these fit beside each other on page one.
#[allow(dead_code)]
pub const MAX_ROWS: usize = 11;

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds() -> Vec<JudgingTableKindDTO> {
        use CyrWheelTableDTO as C;
        use GymWheelTableDTO as G;
        vec![
            JudgingTableKindDTO::Cyr(C::Artistic(Default::default())),
            JudgingTableKindDTO::Cyr(C::Technical(Default::default())),
            JudgingTableKindDTO::Gym(G::STL(Default::default())),
            JudgingTableKindDTO::Gym(G::STLM(Default::default())),
            JudgingTableKindDTO::Gym(G::SPI(Default::default())),
            JudgingTableKindDTO::Gym(G::VLT(Default::default())),
        ]
    }

    #[test]
    fn every_discipline_has_a_name_and_rows() {
        for kind in all_kinds() {
            assert!(!canonical_name(&kind).is_empty());
            let rows = role_rows(&kind);
            assert!(rows.len() == 7 || rows.len() == 11);
            assert_eq!(rows[0].0, "OK");
        }
    }

    #[test]
    fn stlm_is_the_max() {
        let stlm = JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(Default::default()));
        assert_eq!(role_rows(&stlm).len(), MAX_ROWS);
    }
}
