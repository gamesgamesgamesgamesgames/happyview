//! A replay job's sweep and retry rows on HappyView's migrated databases
//! (SQLite, and Postgres when `TEST_DATABASE_URL` is set).

use happyview_source_jetstream::backfill::replay_sweeps::*;

use crate::db::adapt_sql;

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("app.test.c{i:03}")).collect()
}

/// The job system over `state`.
fn jobs(state: &crate::AppState) -> happyview_source_jetstream::backfill::JobState {
    super::jobs(state)
}

#[tokio::test]
async fn sweeps_round_trip_on_both_backends() {
    let mut states = vec![crate::test_support::test_state_with_pool(
        crate::test_support::migrated_memory_pool().await,
    )];
    if let Some(state) = crate::test_support::test_state_from_env().await {
        states.push(state);
    }
    for state in states {
        let job = format!("sweeps-{}", uuid::Uuid::new_v4());
        happyview_provider::db::query(&adapt_sql(
            "INSERT INTO happyview_backfill_jobs (id, scope, status, stage, created_at) \
             VALUES (?, 'network', 'running', 'pending', '2026-10-12T00:00:00+00:00')",
            state.db_backend,
        ))
        .bind(&job)
        .execute(&state.db)
        .await
        .unwrap();
        let mut sweeps = initial_sweeps(&names(2), Some(&["did:plc:x".to_string()]), 5);
        insert(&jobs(&state), &job, &sweeps).await.unwrap();
        sweeps[0].cursor_seq = 40_000_000_000;
        sweeps[0].tip_seq = Some(50_000_000_000);
        sweeps[0].done = true;
        save(&jobs(&state), &job, &sweeps[0]).await.unwrap();
        assert_eq!(load(&jobs(&state), &job).await.unwrap(), sweeps);
        assert_eq!(
            add_counters(&jobs(&state), &job, 1 << 40, 2).await.unwrap(),
            (1 << 40, 2)
        );
        assert_eq!(
            add_counters(&jobs(&state), &job, 10, 1).await.unwrap(),
            ((1 << 40) + 10, 3)
        );
        assert_eq!(
            counters(&jobs(&state), &job).await.unwrap(),
            ((1 << 40) + 10, 3),
            "a read changes nothing"
        );
        add_records(&jobs(&state), &job, 4).await.unwrap();
        add_records(&jobs(&state), &job, 4).await.unwrap();
        assert_eq!(records(&jobs(&state), &job).await.unwrap(), 8);
    }
}

#[tokio::test]
async fn an_unreadable_sweep_row_is_an_error_not_an_empty_list() {
    let mut states = vec![crate::test_support::test_state_with_pool(
        crate::test_support::migrated_memory_pool().await,
    )];
    if let Some(state) = crate::test_support::test_state_from_env().await {
        states.push(state);
    }
    let cases = [
        ("commits", "not json", None),
        ("commits", "[]", None),
        ("markers", "[]", Some("{oops")),
        ("markers", "[]", None),
        ("bogus", "[\"app.test.c\"]", None),
    ];
    for state in states {
        for (kind, collections, dids) in cases {
            let job = format!("corrupt-{}", uuid::Uuid::new_v4());
            happyview_provider::db::query(&adapt_sql(
                "INSERT INTO happyview_backfill_jobs (id, scope, status, stage, created_at) \
                 VALUES (?, 'network', 'running', 'pending', '2026-10-12T00:00:00+00:00')",
                state.db_backend,
            ))
            .bind(&job)
            .execute(&state.db)
            .await
            .unwrap();
            happyview_provider::db::query(&adapt_sql(
                "INSERT INTO happyview_backfill_replay_sweeps \
                 (job_id, sweep, kind, collections, dids, after_seq, cursor_seq) \
                 VALUES (?, 0, ?, ?, ?, 0, 0)",
                state.db_backend,
            ))
            .bind(&job)
            .bind(kind)
            .bind(collections)
            .bind(dids)
            .execute(&state.db)
            .await
            .unwrap();
            let error = load(&jobs(&state), &job).await.expect_err(kind).to_string();
            assert!(error.contains("replay sweep 0"), "{error}");
        }
    }
}
