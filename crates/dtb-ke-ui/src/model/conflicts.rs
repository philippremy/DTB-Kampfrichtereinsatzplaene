//! Detecting a judge assigned to more than one position within a single phase.
//!
//! Qualification and finale run in separate time blocks, so a judge working
//! both is fine — but the same name in two places *within* one phase (two
//! tables, two roles of one table, or a table and the reserve list) is a clash
//! the plan should surface. Matching is case-insensitive on the trimmed name.

use std::collections::BTreeMap;

use crate::model::JudgingTable;
use crate::model::roles;

/// Where a judge's name was found within a phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A judging table: its index in the phase, its label, and the role slot.
    Table {
        table: usize,
        label: String,
        role: &'static str,
    },
    /// The phase's reserve-judge list.
    Spare,
}

impl Placement {
    fn describe(&self) -> String {
        match self {
            Placement::Table { label, role, .. } if !label.trim().is_empty() => {
                format!("{} · {}", label.trim(), role)
            }
            Placement::Table { role, .. } => format!("Kampfgericht · {role}"),
            Placement::Spare => "Ersatzkampfrichter".to_owned(),
        }
    }
}

/// A judge who appears in two or more places in one phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The judge's name as first written (trimmed).
    pub judge: String,
    pub placements: Vec<Placement>,
}

impl Conflict {
    /// One-line German summary, e.g. `"Anna Müller: Gerät 1 · OK, Gerät 3 · SK 1"`.
    pub fn summary(&self) -> String {
        let places: Vec<String> = self.placements.iter().map(Placement::describe).collect();
        format!("{}: {}", self.judge, places.join(", "))
    }
}

/// Every clash found within one phase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhaseConflicts {
    conflicts: Vec<Conflict>,
}

impl PhaseConflicts {
    pub fn is_empty(&self) -> bool {
        self.conflicts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.conflicts.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Conflict> {
        self.conflicts.iter()
    }

    /// The role slots of table `table` that are part of some conflict.
    pub fn table_roles(&self, table: usize) -> Vec<&'static str> {
        let mut roles = Vec::new();
        for conflict in &self.conflicts {
            for placement in &conflict.placements {
                if let Placement::Table { table: t, role, .. } = placement
                    && *t == table
                    && !roles.contains(role)
                {
                    roles.push(*role);
                }
            }
        }
        roles
    }

    /// Whether table `table` participates in any conflict.
    pub fn table_flagged(&self, table: usize) -> bool {
        self.conflicts.iter().any(|conflict| {
            conflict
                .placements
                .iter()
                .any(|p| matches!(p, Placement::Table { table: t, .. } if *t == table))
        })
    }

    /// Normalised names that are conflicted *and* listed as a reserve judge.
    pub fn spare_names(&self) -> Vec<String> {
        self.conflicts
            .iter()
            .filter(|c| c.placements.iter().any(|p| matches!(p, Placement::Spare)))
            .map(|c| normalise(&c.judge))
            .collect()
    }
}

fn normalise(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Detect judges assigned to more than one position within a single phase.
pub fn detect_phase(tables: &[JudgingTable], spare: &[String]) -> PhaseConflicts {
    // normalised name → (name as first seen, every placement)
    let mut seen: BTreeMap<String, (String, Vec<Placement>)> = BTreeMap::new();

    {
        let mut record = |name: &str, placement: Placement| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return;
            }
            seen.entry(normalise(trimmed))
                .or_insert_with(|| (trimmed.to_owned(), Vec::new()))
                .1
                .push(placement);
        };

        for (index, table) in tables.iter().enumerate() {
            for slot in roles::slots(&table.kind) {
                record(
                    &slot.value,
                    Placement::Table {
                        table: index,
                        label: table.label.clone(),
                        role: slot.label,
                    },
                );
            }
        }
        for name in spare {
            record(name, Placement::Spare);
        }
    }

    PhaseConflicts {
        conflicts: seen
            .into_values()
            .filter(|(_, placements)| placements.len() >= 2)
            .map(|(judge, placements)| Conflict { judge, placements })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::roles::Discipline;

    /// A table of `discipline` with `names` filled into its slots in order.
    fn table(label: &str, discipline: Discipline, names: &[&str]) -> JudgingTable {
        let values: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        JudgingTable {
            label: label.to_owned(),
            kind: roles::apply(&discipline.blank_kind(), &values),
        }
    }

    #[test]
    fn no_conflict_when_every_name_is_unique() {
        let tables = [
            table(
                "Gerät 1",
                Discipline::GymStl,
                &["A", "B", "C", "D", "E", "F", "G"],
            ),
            table(
                "Gerät 2",
                Discipline::GymStl,
                &["H", "I", "J", "K", "L", "M", "N"],
            ),
        ];
        assert!(detect_phase(&tables, &["O".into()]).is_empty());
    }

    #[test]
    fn same_name_in_two_tables_is_one_conflict_with_two_placements() {
        let tables = [
            table(
                "Gerät 1",
                Discipline::GymStl,
                &["Anna", "b", "c", "d", "e", "f", "g"],
            ),
            table(
                "Gerät 2",
                Discipline::GymStl,
                &["h", "i", "j", "Anna", "k", "l", "m"],
            ),
        ];
        let found = detect_phase(&tables, &[]);
        assert_eq!(found.len(), 1);
        let conflict = found.iter().next().unwrap();
        assert_eq!(conflict.judge, "Anna");
        assert_eq!(conflict.placements.len(), 2);
        assert_eq!(found.table_roles(0), ["OK"]);
        assert_eq!(found.table_roles(1), ["AK 1"]);
        assert!(found.table_flagged(0) && found.table_flagged(1));
    }

    #[test]
    fn matching_is_case_and_whitespace_insensitive() {
        let tables = [table(
            "T",
            Discipline::GymStl,
            &["  Max Muster ", "b", "c", "d", "e", "f", "g"],
        )];
        let found = detect_phase(&tables, &["max muster".into()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found.spare_names(), ["max muster"]);
    }

    #[test]
    fn empty_slots_never_conflict() {
        let tables = [
            table("A", Discipline::GymStl, &["", "", "", "", "", "", ""]),
            table("B", Discipline::GymStl, &["", "", "", "", "", "", ""]),
        ];
        assert!(detect_phase(&tables, &["".into(), "  ".into()]).is_empty());
    }
}
