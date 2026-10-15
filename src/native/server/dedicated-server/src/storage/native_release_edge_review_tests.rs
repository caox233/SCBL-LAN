//! Independent edge probes; synthetic fixtures only. Insert as a distinct child
//! module of storage in root's existing overlay; never the canonical source.
use super::*;
fn fixture()->(std::sync::Arc<Storage>,SqlitePool) {
    let(s,p)=release_test_support::fixture();
    run(async {for user in 1000..1004 {sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(?,?,?,?)").bind(user).bind(format!("review-{user}")).bind("synthetic").bind(format!("review-{user}")).execute(&p).await.unwrap();}}).unwrap();(s,p)
}
fn foreign()->server_api::release_contract::ReleaseIdentity {
    server_api::release_contract::ReleaseIdentity::new(1,"foreign-release","fixture-compat-1",&"b".repeat(64)).unwrap()
}
fn seed(p:&SqlitePool,user:u32,key:String)->i64 {
    run(sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(?,?,1,'127.0.0.1',?)").bind(user).bind(format!("{user:032x}")).bind(key).execute(p)).unwrap().unwrap().last_insert_rowid()
}
#[test]
fn independent_native_storage_rejects_unapproved_begin_touch_and_current_identity() {
    let(s,p)=fixture();let wrong=foreign();let ip="127.0.0.1".parse().unwrap();let nonce="11111111111111111111111111111111";
    assert!(run(s.begin_game_run_with_release(1000,nonce,1,ip,&wrong)).unwrap().is_err());
    let count:i64=run(sqlx::query_scalar("SELECT COUNT(*) FROM game_runs WHERE user_id=1000").fetch_one(&p)).unwrap().unwrap();assert_eq!(count,0,"foreign begin created state");
    seed(&p,1000,wrong.storage_key());
    let old_nonce=format!("{:032x}",1000);
    assert!(!run(s.touch_game_run_with_release(1000,&old_nonce,1,ip,&wrong)).unwrap().unwrap());
    assert!(!run(s.game_run_is_current(1000,&old_nonce,1,ip,&wrong)).unwrap().unwrap());
}
#[test]
fn independent_native_retired_ticket_never_rebinds_to_latest_approved_run() {
    let(s,p)=fixture();let key=crate::api::test_release_identity().storage_key();let original=seed(&p,1000,key.clone());
    let ticket=[0x39;16];let fp=quazal::kerberos::ticket_fingerprint(&ticket);let ip="127.0.0.1".parse().unwrap();
    s.create_bound_user_session(1000,&ticket,ip).unwrap();assert!(s.mark_secure_ticket(1000,Some(fp)).unwrap());
    run(sqlx::query("UPDATE game_runs SET retired_at=CURRENT_TIMESTAMP WHERE id=?").bind(original).execute(&p)).unwrap().unwrap();
    run(sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(1000,'22222222222222222222222222222222',1,'127.0.0.1',?)").bind(&key).execute(&p)).unwrap().unwrap();
    assert!(!s.mark_secure_ticket(1000,Some(fp)).unwrap());assert!(s.native_game_run(1000,Some(fp),ip).unwrap().is_none());
    let pinned:Option<i64>=run(sqlx::query_scalar("SELECT game_run_id FROM user_sessions WHERE user_id=1000").fetch_one(&p)).unwrap().unwrap();assert_eq!(pinned,Some(original));
    let stored:Option<String>=run(sqlx::query_scalar("SELECT release_identity FROM game_runs WHERE id=?").bind(original).fetch_one(&p)).unwrap().unwrap();assert_eq!(stored,Some(key));
    println!("STALE TICKET: secure=false native=None original binding unchanged");
}
#[test]
fn independent_native_consistent_foreign_room_is_not_approved_or_relabelable() {
    let(s,p)=fixture();let wrong=foreign().storage_key();seed(&p,1000,wrong.clone());
    run(sqlx::query("INSERT INTO game_sessions(id,type_id,creator_id,attributes,release_identity,destroyed_at) VALUES(77,1,1000,'113 => 1',?,CURRENT_TIMESTAMP)").bind(&wrong).execute(&p)).unwrap().unwrap();
    assert!(!s.room_matches_mode(1,77,1).unwrap());assert!(!s.restore_abandoned_game_session_for_split(1000,1,77).unwrap());
    let approved=crate::api::test_release_identity().storage_key();
    assert!(run(sqlx::query("UPDATE game_sessions SET release_identity=? WHERE id=77").bind(&approved).execute(&p)).unwrap().is_err());
    assert!(run(sqlx::query("UPDATE game_runs SET release_identity=? WHERE user_id=1000").bind(&approved).execute(&p)).unwrap().is_err());
}
