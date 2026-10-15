//! Independent synthetic-only migration/backout proof. Root copies this under
//! src/storage and registers #[cfg(test)] mod native_release_migration_review_tests.
//! No deployed database or service path is read, opened, or changed.
use std::{fs, time::{SystemTime, UNIX_EPOCH}};
use sqlx::{migrate::{Migrator, MigrateError}, sqlite::SqlitePoolOptions};

#[test]
fn independent_native_release_migration_refuses_source_only_downgrade_and_restores_exact_backup() {
    let temp = std::env::temp_dir().join(format!("scbl-independent-migration-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&temp).unwrap();
    let old = temp.join("freeze03-migrations"); fs::create_dir(&old).unwrap();
    for (name, sql) in [
        ("20260908000000_lan_initial.sql", include_str!("migrations/20260908000000_lan_initial.sql")),
        ("20261005000000_gameplay_modes.sql", include_str!("migrations/20261005000000_gameplay_modes.sql")),
        ("20261008000000_release_admission.sql", include_str!("migrations/20261008000000_release_admission.sql")),
    ] { fs::write(old.join(name),sql).unwrap(); }
    let database = temp.join("synthetic.sqlite");
    let backup = temp.join("pristine.sqlite");
    let url = format!("sqlite://{}?mode=rwc",database.display().to_string().replace('\\',"/"));
    super::run(async {
        let legacy = Migrator::new(old.as_path()).await.unwrap();
        let pool = SqlitePoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        legacy.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(1000,'synthetic','synthetic','synthetic')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip) VALUES(1000,'11111111111111111111111111111111',1,'127.0.0.1')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO game_sessions(id,type_id,creator_id,attributes) VALUES(77,1,1000,'113 => 0')").execute(&pool).await.unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await.unwrap();
        pool.close().await;
        let pristine=fs::read(&database).unwrap();fs::write(&backup,&pristine).unwrap();
        let pristine_hash=sodiumoxide::hex::encode(sodiumoxide::crypto::hash::sha256::hash(&pristine).0);
        let pool=SqlitePoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        sqlx::migrate!("src/storage/migrations").run(&pool).await.unwrap();
        let legacy_run:Option<String>=sqlx::query_scalar("SELECT release_identity FROM game_runs WHERE user_id=1000").fetch_one(&pool).await.unwrap();
        let legacy_room:Option<String>=sqlx::query_scalar("SELECT release_identity FROM game_sessions WHERE id=77").fetch_one(&pool).await.unwrap();
        assert!(legacy_run.is_none() && legacy_room.is_none(),"migration relabeled unknown old identity");
        let refusal=legacy.run(&pool).await.unwrap_err();
        assert!(matches!(refusal,MigrateError::VersionMissing(_)),"unexpected old-code refusal: {refusal:?}");
        println!("OLD-CODE REFUSAL: {refusal:?}; legacy run=NULL; legacy room=NULL");
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&pool).await.unwrap();
        pool.close().await;
        assert_ne!(fs::read(&database).unwrap(),pristine,"migration did not change schema");
        fs::copy(&backup,&database).unwrap();
        assert_eq!(fs::read(&database).unwrap(),pristine,"backup restoration was not exact bytes");
        let restored_hash=sodiumoxide::hex::encode(sodiumoxide::crypto::hash::sha256::hash(&fs::read(&database).unwrap()).0);
        let pool=SqlitePoolOptions::new().max_connections(1).connect(&url).await.unwrap();
        legacy.run(&pool).await.unwrap();
        let room:i64=sqlx::query_scalar("SELECT COUNT(*) FROM game_sessions WHERE id=77 AND creator_id=1000").fetch_one(&pool).await.unwrap();
        assert_eq!(room,1);
        let integrity:String=sqlx::query_scalar("PRAGMA integrity_check").fetch_one(&pool).await.unwrap();assert_eq!(integrity,"ok");
        pool.close().await;
        println!("BACKOUT: exact backup bytes restored; baseline_sha256={pristine_hash}; restored_sha256={restored_hash}; legacy sqlx migrations accepted; integrity_check=ok; fixture={}",temp.display());
    }).unwrap();
}
