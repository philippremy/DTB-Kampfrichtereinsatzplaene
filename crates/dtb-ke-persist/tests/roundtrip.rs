use chrono::{NaiveDate, NaiveTime};
use dtb_ke_persist::{
    Db, delete_competition, delete_competitions, list_summaries, load_competition,
    save_competition,
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

        // Delete.
        delete_competition(&conn, a.id).await.unwrap();
        assert!(load_competition(&conn, a.id).await.unwrap().is_none());
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 1);
    });
}

/// A single `DELETE … WHERE id IN (…)` for several ids at once — the fix for
/// the bulk-select "delete" path, which used to spawn one `delete_competition`
/// per id and hit turso's "concurrent use forbidden" guard once enough of
/// those concurrent single-row deletes were in flight against clones of the
/// same `Connection`.
#[test]
fn delete_competitions_removes_every_given_id_in_one_statement() {
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
        // as the singular `delete_competition`.
        let unknown = Uuid::new_v4();
        delete_competitions(&conn, &[a.id, c.id, unknown])
            .await
            .unwrap();

        let remaining = list_summaries(&conn).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b.id);
        assert!(load_competition(&conn, a.id).await.unwrap().is_none());
        assert!(load_competition(&conn, c.id).await.unwrap().is_none());
        assert!(load_competition(&conn, b.id).await.unwrap().is_some());

        // An empty slice is a no-op, not a malformed `IN ()`.
        delete_competitions(&conn, &[]).await.unwrap();
        assert_eq!(list_summaries(&conn).await.unwrap().len(), 1);
    });
}
