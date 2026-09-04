//! Flattening a [`JudgingTableKindDTO`] into an ordered list of editable judge
//! slots, and rebuilding it from edited values.

use dtb_ke_types::{
    CyrWheelArtisticTableDTO, CyrWheelTableDTO, CyrWheelTechnicalTableDTO, GymWheelSPITableDTO,
    GymWheelSTLMTableDTO, GymWheelSTLTableDTO, GymWheelTableDTO, GymWheelVLTTableDTO,
    JudgingTableKindDTO,
};

/// One editable judge position: a short role label + its current value.
pub struct Slot {
    pub label: &'static str,
    pub value: String,
}

macro_rules! read_slots {
    ($t:expr, $($field:ident => $label:literal),+ $(,)?) => {
        vec![$(Slot { label: $label, value: $t.$field.clone() }),+]
    };
}

macro_rules! write_slots {
    ($t:expr, $values:expr, $($field:ident),+ $(,)?) => {{
        let mut table = $t.clone();
        let mut values = $values.iter().cloned();
        $(table.$field = values.next().unwrap_or_default();)+
        table
    }};
}

/// The judge slots of a table, in display order.
pub fn slots(kind: &JudgingTableKindDTO) -> Vec<Slot> {
    match kind {
        JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Artistic(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            art1 => "AIK 1", art2 => "AIK 2", art3 => "AIK 3", art4 => "AIK 4"),
        JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Technical(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            exec1 => "AK 1", exec2 => "AK 2", exec3 => "AK 3", exec4 => "AK 4"),
        JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            exec1 => "AK 1", exec2 => "AK 2", exec3 => "AK 3", exec4 => "AK 4"),
        JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            exec1 => "AK 1", exec2 => "AK 2", exec3 => "AK 3", exec4 => "AK 4",
            art1 => "AIK 1", art2 => "AIK 2", art3 => "AIK 3", art4 => "AIK 4"),
        JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            exec1 => "AK 1", exec2 => "AK 2", exec3 => "AK 3", exec4 => "AK 4"),
        JudgingTableKindDTO::Gym(GymWheelTableDTO::VLT(t)) => read_slots!(t,
            head => "OK", diff1 => "SK 1", diff2 => "SK 2",
            exec1 => "AK 1", exec2 => "AK 2", exec3 => "AK 3", exec4 => "AK 4"),
    }
}

/// Rebuild the same table-kind variant with `values` applied in slot order.
pub fn apply(kind: &JudgingTableKindDTO, values: &[String]) -> JudgingTableKindDTO {
    use CyrWheelTableDTO as Cyr;
    use GymWheelTableDTO as Gym;
    use JudgingTableKindDTO as K;
    match kind {
        K::Cyr(Cyr::Artistic(t)) => K::Cyr(Cyr::Artistic(write_slots!(
            t, values, head, diff1, diff2, art1, art2, art3, art4
        ))),
        K::Cyr(Cyr::Technical(t)) => K::Cyr(Cyr::Technical(write_slots!(
            t, values, head, diff1, diff2, exec1, exec2, exec3, exec4
        ))),
        K::Gym(Gym::STL(t)) => K::Gym(Gym::STL(write_slots!(
            t, values, head, diff1, diff2, exec1, exec2, exec3, exec4
        ))),
        K::Gym(Gym::STLM(t)) => K::Gym(Gym::STLM(write_slots!(
            t, values, head, diff1, diff2, exec1, exec2, exec3, exec4, art1, art2, art3, art4
        ))),
        K::Gym(Gym::SPI(t)) => K::Gym(Gym::SPI(write_slots!(
            t, values, head, diff1, diff2, exec1, exec2, exec3, exec4
        ))),
        K::Gym(Gym::VLT(t)) => K::Gym(Gym::VLT(write_slots!(
            t, values, head, diff1, diff2, exec1, exec2, exec3, exec4
        ))),
    }
}

/// A one-line label for the discipline of a table, for the card's chip.
pub fn discipline_label(kind: &JudgingTableKindDTO) -> &'static str {
    Discipline::of(kind).label()
}

/// Re-cast a populated table to a different discipline, carrying over every
/// judge whose role label exists in both variants (`OK` / `SK n` are shared by
/// all; `AK n` by every technical/execution table; `AIK n` by the artistic
/// ones). Roles that don't exist in the target are dropped; roles new to the
/// target start empty.
pub fn change_discipline(kind: &JudgingTableKindDTO, target: Discipline) -> JudgingTableKindDTO {
    let existing = slots(kind);
    let blank = target.blank_kind();
    let values: Vec<String> = slots(&blank)
        .into_iter()
        .map(|slot| {
            existing
                .iter()
                .find(|s| s.label == slot.label)
                .map(|s| s.value.clone())
                .unwrap_or_default()
        })
        .collect();
    apply(&blank, &values)
}

/// The judging disciplines a table can be created as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discipline {
    CyrArtistic,
    CyrTechnical,
    GymStl,
    GymStlm,
    GymSpi,
    GymVlt,
}

impl Discipline {
    pub const ALL: [Discipline; 6] = [
        Discipline::CyrArtistic,
        Discipline::CyrTechnical,
        Discipline::GymStl,
        Discipline::GymStlm,
        Discipline::GymSpi,
        Discipline::GymVlt,
    ];

    /// The discipline a table currently is.
    pub fn of(kind: &JudgingTableKindDTO) -> Self {
        match kind {
            JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Artistic(_)) => Discipline::CyrArtistic,
            JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Technical(_)) => Discipline::CyrTechnical,
            JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(_)) => Discipline::GymStl,
            JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(_)) => Discipline::GymStlm,
            JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(_)) => Discipline::GymSpi,
            JudgingTableKindDTO::Gym(GymWheelTableDTO::VLT(_)) => Discipline::GymVlt,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Discipline::CyrArtistic => "Cyr · Artistik",
            Discipline::CyrTechnical => "Cyr · Technik",
            Discipline::GymStl => "Rhönrad · Gerade",
            Discipline::GymStlm => "Rhönrad · Gerade m. Musik",
            Discipline::GymSpi => "Rhönrad · Spirale",
            Discipline::GymVlt => "Rhönrad · Sprung",
        }
    }

    /// Short label for the "add table" buttons.
    pub fn short(self) -> &'static str {
        match self {
            Discipline::CyrArtistic => "Cyr Art",
            Discipline::CyrTechnical => "Cyr Tech",
            Discipline::GymStl => "STL",
            Discipline::GymStlm => "STLM",
            Discipline::GymSpi => "SPI",
            Discipline::GymVlt => "VLT",
        }
    }

    #[cfg(test)]
    fn expected_slot_count(self) -> usize {
        match self {
            Discipline::GymStlm => 11,
            _ => 7,
        }
    }

    /// A blank table of this discipline.
    pub fn blank_kind(self) -> JudgingTableKindDTO {
        match self {
            Discipline::CyrArtistic => JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Artistic(
                CyrWheelArtisticTableDTO::default(),
            )),
            Discipline::CyrTechnical => JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Technical(
                CyrWheelTechnicalTableDTO::default(),
            )),
            Discipline::GymStl => {
                JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(GymWheelSTLTableDTO::default()))
            }
            Discipline::GymStlm => {
                JudgingTableKindDTO::Gym(GymWheelTableDTO::STLM(GymWheelSTLMTableDTO::default()))
            }
            Discipline::GymSpi => {
                JudgingTableKindDTO::Gym(GymWheelTableDTO::SPI(GymWheelSPITableDTO::default()))
            }
            Discipline::GymVlt => {
                JudgingTableKindDTO::Gym(GymWheelTableDTO::VLT(GymWheelVLTTableDTO::default()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_apply_round_trip_every_discipline() {
        for discipline in Discipline::ALL {
            let kind = discipline.blank_kind();

            let labels: Vec<&str> = slots(&kind).iter().map(|slot| slot.label).collect();
            assert_eq!(
                labels.len(),
                discipline.expected_slot_count(),
                "{discipline:?} slot count"
            );

            let values: Vec<String> = (0..labels.len())
                .map(|i| format!("Kampfrichter {i}"))
                .collect();
            let updated = apply(&kind, &values);

            let read_back: Vec<String> =
                slots(&updated).into_iter().map(|slot| slot.value).collect();
            assert_eq!(read_back, values, "{discipline:?} round-trip");
            assert_eq!(
                discipline_label(&updated),
                discipline.label(),
                "{discipline:?} discipline preserved"
            );
        }
    }

    #[test]
    fn change_discipline_carries_shared_roles_and_drops_the_rest() {
        // STLM (11 slots) fully populated → STL (7 slots): OK/SK/AK survive,
        // the four AIK slots are dropped.
        let stlm = apply(
            &Discipline::GymStlm.blank_kind(),
            &(0..11).map(|i| format!("J{i}")).collect::<Vec<_>>(),
        );
        let stl = change_discipline(&stlm, Discipline::GymStl);
        let carried: Vec<String> = slots(&stl).into_iter().map(|s| s.value).collect();
        assert_eq!(carried, vec!["J0", "J1", "J2", "J3", "J4", "J5", "J6"]);

        // STL → STLM: the seven names stay, the new AIK slots start empty.
        let back = change_discipline(&stl, Discipline::GymStlm);
        let names: Vec<(&str, String)> = slots(&back)
            .into_iter()
            .map(|s| (s.label, s.value))
            .collect();
        assert_eq!(names[0], ("OK", "J0".to_owned()));
        assert_eq!(names[6], ("AK 4", "J6".to_owned()));
        assert_eq!(names[7], ("AIK 1", String::new()));

        // Cyr Artistik → Cyr Technik: OK/SK shared, AIK → AK is not a match.
        let art = apply(
            &Discipline::CyrArtistic.blank_kind(),
            &(0..7).map(|i| format!("A{i}")).collect::<Vec<_>>(),
        );
        let tech = change_discipline(&art, Discipline::CyrTechnical);
        let vals: Vec<String> = slots(&tech).into_iter().map(|s| s.value).collect();
        assert_eq!(vals, vec!["A0", "A1", "A2", "", "", "", ""]);
    }
}
