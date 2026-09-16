use chrono::{NaiveDate, NaiveTime, Utc};
use dtb_ke_persist::{
    Db, list_summaries, list_trashed, load_competition, purge_all_trashed, purge_competition,
    restore_competition, save_competition, soft_delete_competition, soft_delete_competitions,
};
use dtb_ke_types::{
    CompetitionDTO, CyrWheelArtisticTableDTO, CyrWheelTableDTO, GymWheelSTLTableDTO,
    GymWheelTableDTO, JudgingTableDTO, JudgingTableKindDTO, JudgingTablesDTO, MeetingTimeDTO,
    OrganizationDTO, RichParagraphDTO, RichRunDTO, RichTextDTO, SpareJudgesDTO,
};
use uuid::Uuid;

fn sample(id: Uuid, name: &str, year: i32) -> CompetitionDTO {
    CompetitionDTO {
        id,
        name: name.to_owned(),
        organization: OrganizationDTO::DTB,
        location: "Musterhalle".to_owned(),
        date: NaiveDate::from_ymd_opt(year, 6, 15).unwrap(),
        meeting_times: MeetingTimeDTO::Split {
            qualification: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            finale: NaiveTime::from_hms_opt(13, 30, 0).unwrap(),
        },
        responsible_persons: vec!["Alice".into(), "Bob".into()],
        spare_judges: SpareJudgesDTO {
            qualification: vec!["Carol".into()],
            finale: vec![],
        },
        judging_tables: JudgingTablesDTO {
            qualification: vec![JudgingTableDTO {
                label: "Gerät 1".into(),
                kind: JudgingTableKindDTO::Cyr(CyrWheelTableDTO::Artistic(
                    CyrWheelArtisticTableDTO {
                        head: "Dana".into(),
                        ..Default::default()
                    },
                )),
            }],
            finale: vec![JudgingTableDTO {
                label: "Finale".into(),
                kind: JudgingTableKindDTO::Gym(GymWheelTableDTO::STL(GymWheelSTLTableDTO {
                    head: "Erin".into(),
                    ..Default::default()
                })),
            }],
        },
        additional_remarks: RichTextDTO {
            paragraphs: vec![RichParagraphDTO {
                runs: vec![RichRunDTO {
                    text: "Hinweis".into(),
                    bold: true,
                    ..Default::default()
                }],
            }],
        },
    }
}

#[test]
fn save_list_load_delete_roundtrip() {
    pollster::block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("PersistedSessions.bin"))
            .await
            .unwrap();
        let conn = db.connection();

        let a = sample(Uuid::new_v4(), "Alpha Cup", 2024);
        let b = sample(Uuid::new_v4(), "Beta Cup", 2026);
        save_competition(&conn, &a).await.unwrap();
        save_competition(&conn, &b).await.unwrap();

        // Sorted newest date first.
        let summaries = list_summaries(&conn).await.unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, b.id);
        assert_eq!(summaries[0].year(), 2026);
        assert_eq!(summaries[1].name, "Alpha Cup");

        // Blob round-trips.
        let loaded = load_competition(&conn, a.id).await.unwrap().unwrap();
        assert_eq!(loaded.name, "Alpha Cup");
        assert_eq!(loaded.responsible_persons, vec!["Alice", "Bob"]);
        assert_eq!(loaded.judging_tables.qualification[0].label, "Gerät 1");
        assert!(loaded.additional_remarks.paragraphs[0].runs[0].bold);

        // Update in place.
        let mut a2 = a.clone();
        a2.name = "Alpha Cup (rev)".into();
        save_competition(&conn, &a2).await.unwrap();
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 2);
        assert_eq!(
            load_competition(&conn, a.id).await.unwrap().unwrap().name,
            "Alpha Cup (rev)"
        );

        // Delete → trashed, not gone: still loadable by id, absent from the
        // normal listing, present in the trash listing.
        soft_delete_competition(&conn, a.id, Utc::now())
            .await
            .unwrap();
        assert!(load_competition(&conn, a.id).await.unwrap().is_some());
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 1);
        let trashed = list_trashed(&conn).await.unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].id, a.id);
        assert_eq!(trashed[0].name, "Alpha Cup (rev)");

        // Purge → actually gone.
        purge_competition(&conn, a.id).await.unwrap();
        assert!(load_competition(&conn, a.id).await.unwrap().is_none());
        assert!(list_trashed(&conn).await.unwrap().is_empty());
    });
}

/// A single `UPDATE … WHERE id IN (…)` for several ids at once — the fix for
/// the bulk-select "delete" path, which used to spawn one `delete_competition`
/// per id and hit turso's "concurrent use forbidden" guard once enough of
/// those concurrent single-row statements were in flight against clones of
/// the same `Connection`.
#[test]
fn soft_delete_competitions_trashes_every_given_id_in_one_statement() {
    pollster::block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("PersistedSessions.bin"))
            .await
            .unwrap();
        let conn = db.connection();

        let a = sample(Uuid::new_v4(), "Alpha Cup", 2024);
        let b = sample(Uuid::new_v4(), "Beta Cup", 2025);
        let c = sample(Uuid::new_v4(), "Gamma Cup", 2026);
        for dto in [&a, &b, &c] {
            save_competition(&conn, dto).await.unwrap();
        }
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 3);

        // A no-op unknown id rides along — it's just silently skipped, same
        // as the singular `soft_delete_competition`.
        let unknown = Uuid::new_v4();
        soft_delete_competitions(&conn, &[a.id, c.id, unknown], Utc::now())
            .await
            .unwrap();

        let remaining = list_summaries(&conn).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
        let trashed = list_trashed(&conn).await.unwrap();
        assert_eq!(trashed.len(), 2);
        assert!(trashed.iter().any(|t| t.id == a.id));
        assert!(trashed.iter().any(|t| t.id == c.id));

        // An empty slice is a no-op, not a malformed `IN ()`.
        soft_delete_competitions(&conn, &[], Utc::now())
            .await
            .unwrap();
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 1);
    });
}

#[test]
fn restore_moves_a_trashed_competition_back_to_the_normal_listing() {
    pollster::block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("PersistedSessions.bin"))
            .await
            .unwrap();
        let conn = db.connection();

        let a = sample(Uuid::new_v4(), "Alpha Cup", 2024);
        save_competition(&conn, &a).await.unwrap();
        soft_delete_competition(&conn, a.id, Utc::now())
            .await
            .unwrap();
        assert!(list_summaries(&conn).await.unwrap().is_empty());
        assert_eq!(list_trashed(&conn).await.unwrap().len(), 1);

        restore_competition(&conn, a.id).await.unwrap();
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 1);
        assert!(list_trashed(&conn).await.unwrap().is_empty());
    });
}

#[test]
fn purge_all_trashed_empties_the_trash_and_only_the_trash() {
    pollster::block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("PersistedSessions.bin"))
            .await
            .unwrap();
        let conn = db.connection();

        let a = sample(Uuid::new_v4(), "Alpha Cup", 2024);
        let b = sample(Uuid::new_v4(), "Beta Cup", 2025);
        let c = sample(Uuid::new_v4(), "Gamma Cup", 2026);
        for dto in [&a, &b, &c] {
            save_competition(&conn, dto).await.unwrap();
        }
        soft_delete_competitions(&conn, &[a.id, b.id], Utc::now())
            .await
            .unwrap();

        let removed = purge_all_trashed(&conn).await.unwrap();
        assert_eq!(removed, 2);
        assert!(list_trashed(&conn).await.unwrap().is_empty());
        // The one competition never trashed is untouched.
        let remaining = list_summaries(&conn).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, c.id);
        assert!(load_competition(&conn, a.id).await.unwrap().is_none());
        assert!(load_competition(&conn, b.id).await.unwrap().is_none());
    });
}
