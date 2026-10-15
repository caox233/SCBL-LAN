//! Real Storage native admission regressions, synthetic SQLite only.
use super::*;
const IP: &str = "127.0.0.1";
fn fixture() -> (std::sync::Arc<Storage>, SqlitePool) {
    let (s,p)=release_test_support::fixture();
    run(async {for user in 1000..=1012 {sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(?,?,?,?)").bind(user).bind(format!("native-{user}")).bind("fixture-only").bind(format!("native-id-{user}")).execute(&p).await.unwrap();}}).unwrap();(s,p)
}
fn seed_run(p:&SqlitePool,user:u32,mode:i64,key:Option<String>) -> i64 {
    run(sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(?,?,?,?,?)").bind(user).bind(format!("{user:032x}")).bind(mode).bind(IP).bind(key).execute(p)).unwrap().unwrap().last_insert_rowid()
}
fn approved() -> String {crate::api::test_release_identity().storage_key()}
fn wrong() -> String {server_api::release_contract::ReleaseIdentity::new(1,"wrong-release","fixture-compat-1",&"b".repeat(64)).unwrap().storage_key()}
fn count(p:&SqlitePool,sql:&str)->i64 {run(sqlx::query_scalar(sql).fetch_one(p)).unwrap().unwrap()}
#[test]
fn native_release_missing_run_cannot_issue_game_ticket() {
    let(s,p)=fixture();assert!(s.create_bound_user_session(1000,&[1;16],IP.parse().unwrap()).is_err(),"missing run issued native ticket");
    assert_eq!(count(&p,"SELECT COUNT(*) FROM user_sessions"),0);assert_eq!(count(&p,"SELECT is_online FROM users WHERE id=1000"),0);
}
#[test]
fn native_release_null_and_wrong_run_cannot_issue_game_ticket() {
    for key in [None,Some(wrong())] {let(s,p)=fixture();seed_run(&p,1000,1,key);assert!(s.create_bound_user_session(1000,&[2;16],IP.parse().unwrap()).is_err(),"unapproved run issued native ticket");assert_eq!(count(&p,"SELECT COUNT(*) FROM user_sessions"),0);}
}
#[test]
fn native_release_secure_reconnect_and_query_recheck_bound_run() {
    for key in [None,Some(wrong())] {let(s,p)=fixture();let id=seed_run(&p,1000,2,key);let fp=quazal::kerberos::ticket_fingerprint(&[3;16]);
        run(sqlx::query("INSERT INTO user_sessions(id,user_id,game_run_id,ticket_hash,native_connected) VALUES('native-fixture',1000,?,?,0)").bind(id).bind(sodiumoxide::hex::encode(fp)).execute(&p)).unwrap().unwrap();
        assert!(!s.mark_secure_ticket(1000,Some(fp)).unwrap(),"unapproved ticket set secure presence");assert_eq!(count(&p,"SELECT is_online FROM users WHERE id=1000"),0);
        run(sqlx::query("UPDATE user_sessions SET native_connected=1").execute(&p)).unwrap().unwrap();assert!(s.native_game_run(1000,Some(fp),IP.parse().unwrap()).unwrap().is_none());
    }
}
#[test]
fn native_release_legacy_room_cannot_restore_or_match_search() {
    let(s,p)=fixture();seed_run(&p,1000,1,Some(approved()));
    run(sqlx::query("INSERT INTO game_sessions(id,type_id,creator_id,attributes,destroyed_at) VALUES(77,1,1000,'113 => 1;3 => 12;4 => 0',CURRENT_TIMESTAMP)").execute(&p)).unwrap().unwrap();
    assert!(!s.room_matches_mode(1,77,1).unwrap(),"legacy room passed shared keyed/search gate");
    assert!(!s.restore_abandoned_game_session_for_split(1000,1,77).unwrap(),"legacy room restored from caller's new run");
    assert_eq!(count(&p,"SELECT COUNT(*) FROM game_sessions WHERE id=77 AND destroyed_at IS NOT NULL"),1);
}
#[test]
fn native_release_add_peer_and_participant_guard_reject_other_release() {
    let(s,p)=fixture();seed_run(&p,1000,1,Some(approved()));seed_run(&p,1001,1,Some(wrong()));
    let room=s.create_game_session(1000,1,"113 => 0;3 => 12;4 => 0".into()).unwrap();
    assert_eq!(run(s.current_game_mode(1001)).unwrap().unwrap(),None,"AddParticipants peer predicate admitted another release");
    assert!(run(sqlx::query("INSERT INTO participants(game_id,user_id) VALUES(?,1001)").bind(room).execute(&p)).unwrap().is_err());
}
#[test]
fn native_release_party_and_invite_reject_other_release() {
    let(s,p)=fixture();seed_run(&p,1000,2,Some(approved()));seed_run(&p,1001,2,Some(wrong()));let room=s.create_game_session(1000,1,"113 => 0;3 => 12;4 => 0".into()).unwrap();
    run(sqlx::query("INSERT INTO parties(id,leader_id) VALUES(9,1000)").execute(&p)).unwrap().unwrap();
    assert!(run(sqlx::query("INSERT INTO party_members(user_id,party_id) VALUES(1001,9)").execute(&p)).unwrap().is_err(),"Party accepted wrong-release member");
    run(sqlx::query("INSERT INTO invites(id,sender,receiver,session_type,session_id) VALUES(5,1000,1001,1,?)").bind(room).execute(&p)).unwrap().unwrap();
    assert!(!run(s.invite_matches_mode(1001,5,2)).unwrap().unwrap());
}
#[test]
fn native_release_exact_official_mod_ticket_and_twelve_player_restore_still_work() {
    for mode in [1,2] {let(s,p)=fixture();for u in 1000..1012 {seed_run(&p,u,mode,Some(approved()));}
        let key=[4;16];let fp=quazal::kerberos::ticket_fingerprint(&key);s.create_bound_user_session(1000,&key,IP.parse().unwrap()).unwrap();assert!(s.mark_secure_ticket(1000,Some(fp)).unwrap());assert_eq!(s.native_game_run(1000,Some(fp),IP.parse().unwrap()).unwrap().unwrap().mode,mode);
        assert!(s.native_game_run(1000,Some(fp),"127.0.0.2".parse().unwrap()).unwrap().is_none());
        let room=s.create_game_session(1000,1,"113 => 0;3 => 12;4 => 0".into()).unwrap();for u in 1001..1012 {assert!(s.join_game_session_unbounded(u,1,room).unwrap());}
        assert_eq!(count(&p,&format!("SELECT COUNT(*) FROM participants WHERE game_id={room}")),12);assert!(s.room_matches_mode(1,room,mode).unwrap());assert!(!s.room_matches_mode(1,room,3-mode).unwrap());
        run(sqlx::query("UPDATE game_sessions SET destroyed_at=CURRENT_TIMESTAMP WHERE id=?").bind(room).execute(&p)).unwrap().unwrap();assert!(s.restore_abandoned_game_session_for_split(1000,1,room).unwrap());assert!(s.room_matches_mode(1,room,mode).unwrap());
        println!("NATIVE_RELEASE mode={mode} exact_ticket=true source_guard=true twelve_players=true same_release_restore=true");
    }
}
fn client(s:&Storage,user:u32)->quazal::ClientInfo<()> {
    let key:[u8;16]=rand::random();s.create_bound_user_session(user,&key,IP.parse().unwrap()).unwrap();
    let mut ci=quazal::ClientInfo::new("127.0.0.1:30500".parse().unwrap());ci.user_id=Some(user);ci.ticket_fingerprint=Some(quazal::kerberos::ticket_fingerprint(&key));assert!(s.mark_secure_ticket(user,ci.ticket_fingerprint).unwrap());ci
}
#[test]
fn native_release_real_add_and_search_protocols_hide_foreign_and_null_rooms() {
    use quazal::rmc::basic::{FromStream,ToStream};use quazal::rmc::{Request,Error};use quazal::rmc::types::{QList,Property};
    use crate::protocols::game_session_ex_service::game_session_ex_protocol::{SearchSessionsRequest,SearchSessionsResponse};
    use crate::protocols::game_session_service::game_session_protocol::AddParticipantsRequest;
    use crate::protocols::game_session_service::types::{GameSessionKey,GameSessionQuery};
    let(s,p)=fixture();seed_run(&p,1000,1,Some(approved()));seed_run(&p,1001,1,Some(approved()));seed_run(&p,1002,1,Some(wrong()));seed_run(&p,1003,1,None);
    for u in 1000..=1003 {s.register_urls(u,vec![format!("prudp:/address=127.0.0.1;port={};sid=42;type=2",13000+u)]).unwrap();}
    let attrs="113 => 0;3 => 12;4 => 0;103 => 2165463540;105 => 0;112 => 8";
    let good=s.create_game_session(1000,1,attrs.into()).unwrap();
    // Synthetic historical foreign room: bypass only creation API, not any admission check.
    let foreign=run(sqlx::query("INSERT INTO game_sessions(type_id,creator_id,attributes,release_identity) VALUES(1,1002,?,?)").bind(attrs).bind(wrong()).execute(&p)).unwrap().unwrap().last_insert_rowid() as u32;
    run(sqlx::query("INSERT INTO participants(game_id,user_id) VALUES(?,1002)").bind(foreign).execute(&p)).unwrap().unwrap();
    let legacy=run(sqlx::query("INSERT INTO game_sessions(type_id,creator_id,attributes) VALUES(1,1003,?)").bind(attrs).execute(&p)).unwrap().unwrap().last_insert_rowid() as u32;
    let log=Logger::root(slog::Discard,slog::o!());let ctx=quazal::Context::splinter_cell_blacklist();let clients=quazal::prudp::ClientRegistry::default();let socket=std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let native=crate::game_session::new_protocol::<()>(s.clone());let search=crate::game_session_ex::new_protocol::<()>(s.clone());let mut ci=client(&s,1000);
    for peer in [1002,1003] {let req=AddParticipantsRequest{game_session_key:GameSessionKey{type_id:1,session_id:good},public_participant_ids:QList(vec![peer]),private_participant_ids:QList(vec![])};
        assert!(matches!(native.handle(&log,&ctx,&mut ci,&Request{protocol_id:42,call_id:1,method_id:8,parameters:req.to_bytes()},&clients,&socket),Err(Error::AccessDenied)));
    }
    let request=SearchSessionsRequest{game_session_query:GameSessionQuery{type_id:1,query_id:0,parameters:QList(vec![Property{id:103,value:2165463540},Property{id:113,value:0},Property{id:105,value:0},Property{id:112,value:2}])}};
    let mut ci=client(&s,1001);let bytes=search.handle(&log,&ctx,&mut ci,&Request{protocol_id:123,call_id:2,method_id:1,parameters:request.to_bytes()},&clients,&socket).unwrap();
    let ids=SearchSessionsResponse::from_bytes(&bytes).unwrap().search_results.0.into_iter().map(|r|r.game_session_search_result.session_key.session_id).collect::<Vec<_>>();assert_eq!(ids,vec![good]);assert!(!ids.contains(&foreign)&&!ids.contains(&legacy));
    assert!(run(s.search_sessions_with_participants_async(1,&[1002])).unwrap().unwrap().is_empty());
    println!("NATIVE_RELEASE actual_add_foreign_null_denied=true actual_search_exact_visible=true foreign_null_hidden=true");
}
#[test]
fn native_release_policy_restart_cannot_relabel_restore_or_migrate_old_room() {
    use server_api::release_contract::ReleaseIdentity;
    let unique=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();let dir=std::env::var_os("SCBL_RELEASE_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path=dir.join(format!("native-release-policy-{}-{unique}.db",std::process::id()));let url=format!("sqlite://{}?mode=rwc",path.display());let log=||Logger::root(slog::Discard,slog::o!());
    let old=Storage::init_with_database_url(log(),&url).unwrap();let p=old.pool.clone();run(sqlx::query("INSERT INTO users(id,username,password,ubi_id) VALUES(1000,'restart-native','fixture','restart-native-id')").execute(&p)).unwrap().unwrap();seed_run(&p,1000,1,Some(approved()));let room=old.create_game_session(1000,1,"113 => 1;3 => 12;4 => 0".into()).unwrap();old.invalidate_sessions().unwrap();
    let next=ReleaseIdentity::new(1,"wrong-release","fixture-compat-1",&"b".repeat(64)).unwrap();let new=Storage::init_with_database_url_and_release(log(),&url,next.clone()).unwrap();run(new.begin_game_run_with_release(1000,"22222222222222222222222222222222",1,IP.parse().unwrap(),&next)).unwrap().unwrap();
    assert!(!new.room_matches_mode(1,room,1).unwrap());assert!(!new.restore_abandoned_game_session_for_split(1000,1,room).unwrap());assert!(!new.migrate_game_session_host(1000,1,room).unwrap());
    assert!(run(sqlx::query("UPDATE game_sessions SET release_identity=? WHERE id=?").bind(next.storage_key()).bind(room).execute(&p)).unwrap().is_err());
    assert!(run(sqlx::query("UPDATE game_sessions SET destroyed_at=NULL WHERE id=?").bind(room).execute(&p)).unwrap().is_err());
    assert_eq!(count(&p,"SELECT COUNT(*) FROM game_sessions WHERE destroyed_at IS NOT NULL"),1);
    println!("NATIVE_RELEASE policy_restart_old_room_retired=true immutable_provenance=true storage_split_and_sql_guard=true");
}
#[test]
fn native_release_storage_policy_and_lease_rejections_preserve_presence() {
    let(s,p)=fixture();let foreign=server_api::release_contract::ReleaseIdentity::new(1,"wrong-release","fixture-compat-1",&"b".repeat(64)).unwrap();
    assert!(run(s.begin_game_run_with_release(1000,"11111111111111111111111111111111",1,IP.parse().unwrap(),&foreign)).unwrap().is_err());assert_eq!(count(&p,"SELECT COUNT(*) FROM game_runs"),0);
    seed_run(&p,1000,1,Some(approved()));let key=[5;16];let fp=quazal::kerberos::ticket_fingerprint(&key);s.create_bound_user_session(1000,&key,IP.parse().unwrap()).unwrap();
    run(sqlx::query("UPDATE game_runs SET seen_at=datetime('now','-91 seconds')").execute(&p)).unwrap().unwrap();assert!(!s.mark_secure_ticket(1000,Some(fp)).unwrap());assert_eq!(count(&p,"SELECT is_online FROM users WHERE id=1000"),0);
    assert!(!run(s.touch_game_run_with_release(1000,&format!("{:032x}",1000),1,IP.parse().unwrap(),&foreign)).unwrap().unwrap());
    assert_eq!(count(&p,"SELECT COUNT(*) FROM game_runs WHERE seen_at<datetime('now','-90 seconds')"),1);
}
