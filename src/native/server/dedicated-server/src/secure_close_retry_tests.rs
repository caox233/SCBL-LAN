//! F-SRV-003: real PRUDP expiry -> main close callback -> migrated synthetic SQLite.
//! No socket is bound. Only time/owner admission are seeded at the transport seam.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use quazal::prudp::packet::{PacketType, QPacket, StreamHandlerRegistry};
use quazal::{ClientInfo, Context};
use sqlx::SqlitePool;

use super::{configure_secure_close_handler, NativeServer};
use crate::storage::Storage;

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}

fn fixture() -> (Arc<Storage>, SqlitePool) {
    let (storage, pool) = crate::storage::release_test_support::fixture();
    run(async {
        for user in [1000_i64, 1001] {
            sqlx::query("INSERT INTO users(id,username,password,ubi_id,last_api_seen_at) VALUES(?,?,?, ?,datetime('now','-60 seconds'))")
                .bind(user).bind(format!("close-fixture-{user}")).bind("synthetic-not-a-login-password")
                .bind(format!("close-id-{user}")).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO game_runs(user_id,instance,mode,source_ip,release_identity) VALUES(?,?,2,'127.0.0.1',?)")
                .bind(user).bind(format!("{user:032x}")).bind(crate::api::test_release_identity().storage_key())
                .execute(&pool).await.unwrap();
        }
    });
    for user in [1000, 1001] {
        storage.register_urls(user, vec![format!("prudp:/address=127.0.0.1;port={};sid=42;type=2", 13000 + user)]).unwrap();
    }
    (storage, pool)
}

fn count(pool: &SqlitePool, query: &str) -> i64 {
    run(sqlx::query_scalar(query).fetch_one(pool)).unwrap()
}

fn fault_online_transition(pool: &SqlitePool) {
    // DELETE already ran when this fails: the retained old row proves transaction rollback.
    run(sqlx::query("CREATE TRIGGER close_fault BEFORE UPDATE OF is_online ON users WHEN OLD.id=1000 AND NEW.is_online=0 BEGIN SELECT RAISE(ABORT,'F-SRV-003 injected close failure'); END").execute(pool)).unwrap();
}

fn recover(pool: &SqlitePool) {
    run(sqlx::query("DROP TRIGGER close_fault").execute(pool)).unwrap();
}

// An expected red assertion may unwind while the real SQLite writer is held.
// Always consume that pooled transaction INSIDE a Tokio context. Otherwise its
// PoolConnection destructor tries to spawn after run() has already dropped its
// temporary runtime, causing a second panic and aborting the entire test binary.
struct WriterBlocker {
    transaction: Option<sqlx::Transaction<'static, sqlx::Sqlite>>,
}
impl WriterBlocker {
    fn begin(pool: &SqlitePool) -> Self {
        Self { transaction: Some(run(pool.begin_with("BEGIN IMMEDIATE")).unwrap()) }
    }
    fn rollback(mut self) {
        run(self.transaction.take().expect("writer transaction").rollback()).unwrap();
    }
}
impl Drop for WriterBlocker {
    fn drop(&mut self) {
        if let Some(transaction) = self.transaction.take() {
            // Never turn an already failing assertion into a double panic. The
            // original assertion remains failed; cleanup errors remain visible.
            if let Err(error) = run(transaction.rollback()) {
                eprintln!("F-SRV-003 fixture writer rollback failed: {error}");
            }
        }
    }
}

fn client(storage: &Storage, user: u32, key_byte: u8, connected: bool) -> ClientInfo {
    let key = [key_byte; 16];
    storage.create_bound_user_session(user, &key, "127.0.0.1".parse().unwrap()).unwrap();
    let mut ci = ClientInfo::new("127.0.0.1:30500".parse().unwrap());
    ci.user_id = Some(user);
    ci.ticket_fingerprint = Some(quazal::kerberos::ticket_fingerprint(&key));
    if connected {
        assert!(storage.mark_secure_ticket(user, ci.ticket_fingerprint).unwrap());
    }
    ci
}

fn ticket_count(pool: &SqlitePool, fingerprint: [u8; 32]) -> i64 {
    run(sqlx::query_scalar("SELECT COUNT(*) FROM user_sessions WHERE ticket_hash=?")
        .bind(sodiumoxide::hex::encode(fingerprint)).fetch_one(pool)).unwrap()
}

fn server<'a>(ctx: &'a Context, logger: &'a slog::Logger, storage: &'a Storage) -> NativeServer<'a> {
    let mut server = NativeServer::new(logger.clone(), ctx, StreamHandlerRegistry::new(logger.clone()));
    configure_secure_close_handler(&mut server, logger, storage);
    server
}

#[test]
fn fsrv003_expiry_sql_failure_retains_owner_until_real_close_converges() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let old = client(&storage, 1000, 1, true);
    let fingerprint = old.ticket_fingerprint.unwrap();
    transport.maintenance_seed_connected(11, old, true);
    transport.maintenance_seed_connected(12, client(&storage, 1001, 2, true), false);
    fault_online_transition(&pool);
    transport.maintenance_tick();
    assert_eq!(ticket_count(&pool, fingerprint), 1, "SQL error must roll back old ticket deletion");
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
    println!("F-SRV-003 expiry fault: old_ticket=1 online=1 transport={:?}", transport.maintenance_counts());
    assert_eq!(transport.maintenance_counts(), (2, 2, 0, 1), "failed close must retain exact owner and index");
    assert!(!transport.maintenance_routable(11), "terminal owner must not receive new NAT/notification payloads");
    assert!(transport.maintenance_routable(12), "another active connection remains routable");
    recover(&pool);
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    transport.maintenance_tick();
    assert_eq!(ticket_count(&pool, fingerprint), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1001"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1001"), 1);
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
    transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
    println!("F-SRV-003 expiry recovered: old_ticket=0 online=0 own_urls=0 other_user_unchanged=true owner_retired=true");
}

#[test]
fn fsrv003_old_retry_preserves_new_ticket_on_already_admitted_connection() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let old = client(&storage, 1000, 3, true);
    let old_fp = old.ticket_fingerprint.unwrap();
    // Already PRUDP-connected before failure, but SecureConnection registration
    // has not marked the fresh ticket yet. Active owners may finish that path.
    let newer = client(&storage, 1000, 4, false);
    let new_fp = newer.ticket_fingerprint.unwrap();
    transport.maintenance_seed_connected(21, old, true);
    transport.maintenance_seed_connected(22, newer, false);
    transport.maintenance_seed_connected(23, client(&storage, 1001, 5, true), false);
    fault_online_transition(&pool);
    transport.maintenance_tick();
    assert_eq!(ticket_count(&pool, old_fp), 1);
    assert_eq!(transport.maintenance_counts(), (3, 3, 0, 1));
    recover(&pool);
    assert!(storage.mark_secure_ticket(1000, Some(new_fp)).unwrap());
    let new_owner = transport.maintenance_owner(22);
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    transport.maintenance_tick();
    assert_eq!(ticket_count(&pool, old_fp), 0, "retry must consume exact old fingerprint");
    assert_eq!(ticket_count(&pool, new_fp), 1, "retry must not close newest ticket by user id");
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 1);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1001"), 1);
    assert_eq!(transport.maintenance_owner(22), new_owner);
    assert_eq!(transport.maintenance_counts(), (2, 2, 0, 0));
    println!("F-SRV-003 old retry: old_ticket=0 new_ticket=1 online=1 urls=1 new_owner_unchanged=true other_user_unchanged=true");
}

#[test]
fn fsrv003_retry_tick_is_bounded_and_rotates_past_persistent_failures() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let seen = Rc::new(RefCell::new(BTreeMap::<u32, usize>::new()));
    let observed = Rc::clone(&seen);
    let mut actual_close = transport.close_handler.take().unwrap();
    transport.close_handler = Some(Box::new(move |ci, reason| {
        *observed.borrow_mut().entry(u32::from(ci.connection_id.unwrap())).or_default() += 1;
        actual_close(ci, reason)
    }));
    for number in 1_u8..=35 {
        transport.maintenance_seed_connected(100 + u32::from(number), client(&storage, 1000, number, true), true);
    }
    // Every callback uses real Storage; the counter wraps, rather than replaces it.
    run(sqlx::query("CREATE TRIGGER close_fault BEFORE DELETE ON user_sessions BEGIN SELECT RAISE(ABORT,'F-SRV-003 persistent close failure'); END").execute(&pool)).unwrap();
    for _ in 0..35 {
        let before = seen.borrow().values().sum::<usize>();
        transport.maintenance_tick();
        let after = seen.borrow().values().sum::<usize>();
        assert!(after - before <= 32, "one tick exceeded its operation budget");
        assert_eq!(transport.maintenance_counts(), (35, 35, 0, 35));
        if seen.borrow().len() == 35 { break; }
    }
    assert_eq!(seen.borrow().len(), 35, "first failing owner must not starve later owners");
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1000"), 35);
    recover(&pool);
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    for _ in 0..35 {
        transport.maintenance_tick();
        if transport.maintenance_counts().0 == 0 { break; }
    }
    assert_eq!(transport.maintenance_counts(), (0, 0, 0, 0));
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1000"), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
    println!("F-SRV-003 retry bound: <=32 attempts/tick; 35/35 owners reached; all converge after recovery");
}

#[test]
fn fsrv003_persistent_failure_obeys_per_owner_retry_backoff() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let calls = Rc::new(RefCell::new(0_usize));
    let observed = Rc::clone(&calls);
    let mut actual_close = transport.close_handler.take().unwrap();
    transport.close_handler = Some(Box::new(move |ci, reason| {
        *observed.borrow_mut() += 1;
        actual_close(ci, reason)
    }));
    transport.maintenance_seed_connected(41, client(&storage, 1000, 41, true), true);
    fault_online_transition(&pool);
    transport.maintenance_tick();
    assert_eq!(*calls.borrow(), 1);
    for expected_delay in [1_u64, 2, 4, 8, 16, 30, 30] {
        let before = *calls.borrow();
        transport.maintenance_tick();
        assert_eq!(*calls.borrow(), before, "immediate retry ignored deadline");
        if expected_delay > 1 {
            transport.maintenance_advance_retry_clock(Duration::from_secs(expected_delay - 1));
            transport.maintenance_tick();
            assert_eq!(*calls.borrow(), before, "retry fired before its exponential deadline");
        }
        transport.maintenance_advance_retry_clock(Duration::from_secs(1));
        transport.maintenance_tick();
        assert_eq!(*calls.borrow(), before + 1);
        assert_eq!(transport.maintenance_counts(), (1, 1, 0, 1));
    }
    recover(&pool);
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(), (0, 0, 0, 0));
    println!("F-SRV-003 retry deadlines observed: 1,2,4,8,16,30,30 seconds; immediate ticks issue no SQL close");
}

// v3-only extension: the six original v2 cases remain frozen in MODIFIED_FILE.rs.
// server() calls the real configure_secure_close_handler. Counting wraps that
// handler and always forwards to actual_close: never fake success or old Storage.
#[test]
fn fsrv003_v3_real_writer_lock_defers_quickly_then_exact_tickets_converge() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let calls = Rc::new(RefCell::new(0_usize));
    let observed = Rc::clone(&calls);
    let mut actual_close = transport.close_handler.take().unwrap();
    transport.close_handler = Some(Box::new(move |ci, reason| {
        *observed.borrow_mut() += 1;
        actual_close(ci, reason)
    }));
    let first = client(&storage, 1000, 61, true);
    let first_fp = first.ticket_fingerprint.unwrap();
    let second = client(&storage, 1000, 62, true);
    let second_fp = second.ticket_fingerprint.unwrap();
    transport.maintenance_seed_connected(61, first, true);
    transport.maintenance_seed_connected(62, second, true);
    transport.maintenance_seed_connected(63, client(&storage, 1001, 63, true), false);
    let blocker = WriterBlocker::begin(&pool);
    let started = Instant::now();
    transport.maintenance_tick();
    let elapsed = started.elapsed();
    let during = transport.maintenance_counts();
    let callback_calls = *calls.borrow();
    println!("F-SRV-003 V3_REAL_BUSY elapsed_ms={} actual_callback_calls={} retained={during:?}", elapsed.as_millis(), callback_calls);
    blocker.rollback(); // Before assertions too; RAII also covers earlier panics.
    assert!((1..=2).contains(&callback_calls), "a fast Deferred callback may validly try both owners");
    assert!(elapsed < Duration::from_secs(1), "must distinguish nonblocking deferral from old five-second busy waits, not a hard 25ms SLA");
    assert_eq!(during, (3, 3, 0, 2));
    assert_eq!(ticket_count(&pool, first_fp), 1);
    assert_eq!(ticket_count(&pool, second_fp), 1);
    assert!(!transport.maintenance_routable(61));
    assert!(!transport.maintenance_routable(62));
    assert!(transport.maintenance_routable(63));
    for _ in 0..4 {
        transport.maintenance_advance_retry_clock(Duration::from_secs(60));
        transport.maintenance_tick();
        if transport.maintenance_counts() == (1, 1, 0, 0) { break; }
    }
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
    assert_eq!(ticket_count(&pool, first_fp), 0);
    assert_eq!(ticket_count(&pool, second_fp), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1001"), 1);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
    println!("F-SRV-003 V3_REAL_BUSY recovered: exact_two_tickets=0 own_online=0 other_user=unchanged");
}

// Register a REAL queued Tokio writer while the caller holds a read guard.
// Polling once to Pending is the queue-admission witness; sleeps are not used
// to guess ordering. A bounded watchdog cancels it so the old baseline cannot
// deadlock this test forever while blocking_read waits behind that writer.
struct QueuedModeWriter {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<bool>>,
}
impl QueuedModeWriter {
    fn begin(storage: Arc<Storage>) -> Self {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || run(async move {
            let writer = storage.mode_gate.write();
            tokio::pin!(writer);
            std::future::poll_fn(|cx| {
                match std::future::Future::poll(writer.as_mut(), cx) {
                    std::task::Poll::Pending => std::task::Poll::Ready(()),
                    std::task::Poll::Ready(_) => panic!("queued-writer setup needs caller's held read guard"),
                }
            }).await;
            ready_tx.send(()).unwrap();
            // This owns its private runtime thread; the wait cannot stall the
            // caller or a production runtime. Future remains pinned and queued.
            release_rx.recv_timeout(Duration::from_secs(3)).is_err()
        }));
        let owner = Self { release: Some(release_tx), thread: Some(thread) };
        ready_rx.recv_timeout(Duration::from_secs(2)).expect("writer was polled Pending");
        owner
    }
    fn release(mut self) -> bool {
        if let Some(sender) = self.release.take() { let _ = sender.send(()); }
        self.thread.take().expect("writer thread").join().expect("queued writer survived")
    }
}
impl Drop for QueuedModeWriter {
    fn drop(&mut self) {
        if let Some(sender) = self.release.take() { let _ = sender.send(()); }
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() { eprintln!("F-SRV-003 fixture queued writer panicked during cleanup"); }
        }
    }
}

#[test]
fn fsrv003_v3_queued_mode_writer_defers_without_blocking_transport_or_losing_owner() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let old = client(&storage, 1000, 71, true);
    let fingerprint = old.ticket_fingerprint.unwrap();
    transport.maintenance_seed_connected(71, old, true);
    transport.maintenance_seed_connected(72, client(&storage, 1001, 72, true), false);
    let held_read = storage.mode_gate.blocking_read();
    let writer = QueuedModeWriter::begin(Arc::clone(&storage));
    let started = Instant::now();
    transport.maintenance_tick();
    let elapsed = started.elapsed();
    let during = transport.maintenance_counts();
    let deadline_cancelled_writer = writer.release();
    drop(held_read);
    println!("F-SRV-003 V3_QUEUED_MODE elapsed_ms={} retained={during:?} watchdog_cancelled={deadline_cancelled_writer}", elapsed.as_millis());
    assert!(!deadline_cancelled_writer, "old blocking_read waited for queued writer watchdog");
    assert!(elapsed < Duration::from_secs(1), "queued writer must cause immediate mode-gate deferral");
    assert_eq!(during, (2, 2, 0, 1));
    assert_eq!(ticket_count(&pool, fingerprint), 1);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
    assert!(!transport.maintenance_routable(71));
    assert!(transport.maintenance_routable(72));
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
    assert_eq!(ticket_count(&pool, fingerprint), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
    println!("F-SRV-003 V3_QUEUED_MODE recovered: exact_ticket=0 owner_retired=true other_user_unchanged=true");
}

#[test]
fn fsrv003_v3_real_statement_faults_rollback_all_prior_close_changes() {
    for (phase, trigger) in [
        ("before_ticket_delete", "CREATE TRIGGER close_fault BEFORE DELETE ON user_sessions WHEN OLD.user_id=1000 BEGIN SELECT RAISE(ABORT,'F-SRV-003 before ticket DELETE'); END"),
        ("after_ticket_delete", "CREATE TRIGGER close_fault BEFORE UPDATE OF is_online ON users WHEN OLD.id=1000 AND NEW.is_online=0 BEGIN SELECT RAISE(ABORT,'F-SRV-003 after ticket DELETE'); END"),
        ("after_online_update", "CREATE TRIGGER close_fault BEFORE DELETE ON station_urls WHEN OLD.user_id=1000 BEGIN SELECT RAISE(ABORT,'F-SRV-003 after online UPDATE'); END"),
    ] {
        let (storage, pool) = fixture();
        let ctx = Context::splinter_cell_blacklist();
        let logger = slog::Logger::root(slog::Discard, slog::o!());
        let mut transport = server(&ctx, &logger, &storage);
        let old = client(&storage, 1000, 81, true);
        let fingerprint = old.ticket_fingerprint.unwrap();
        transport.maintenance_seed_connected(81, old, true);
        transport.maintenance_seed_connected(82, client(&storage, 1001, 82, true), false);
        let owner = transport.maintenance_owner(81);
        run(sqlx::query(trigger).execute(&pool)).unwrap();
        transport.maintenance_tick();
        println!("F-SRV-003 V3_STATEMENT_FAULT phase={phase} retained={:?} ticket={} online={} urls={}", transport.maintenance_counts(),ticket_count(&pool,fingerprint),count(&pool,"SELECT is_online FROM users WHERE id=1000"),count(&pool,"SELECT COUNT(*) FROM station_urls WHERE user_id=1000"));
        assert_eq!(transport.maintenance_counts(), (2, 2, 0, 1), "phase={phase}");
        assert_eq!(transport.maintenance_owner(81), owner);
        assert_eq!(ticket_count(&pool, fingerprint), 1);
        assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 1);
        assert!(!transport.maintenance_routable(81));
        assert!(transport.maintenance_routable(82));
        recover(&pool);
        transport.maintenance_advance_retry_clock(Duration::from_secs(60));
        transport.maintenance_tick();
        assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
        assert_eq!(ticket_count(&pool, fingerprint), 0);
        assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 0);
        assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1001"), 1);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1001"), 1);
        println!("F-SRV-003 V3_STATEMENT_FAULT phase={phase} recovered=true other_user_unchanged=true");
    }
}

#[test]
fn fsrv003_v3_deferred_foreign_key_fails_at_commit_and_close_remains_retryable() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage);
    let old = client(&storage, 1000, 91, true);
    let fingerprint = old.ticket_fingerprint.unwrap();
    let hash = sodiumoxide::hex::encode(fingerprint);
    transport.maintenance_seed_connected(91, old, true);
    transport.maintenance_seed_connected(92, client(&storage, 1001, 92, true), false);
    run(async {
        sqlx::query("CREATE TABLE close_commit_child (session_id TEXT NOT NULL REFERENCES user_sessions(id) DEFERRABLE INITIALLY DEFERRED)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO close_commit_child(session_id) SELECT id FROM user_sessions WHERE user_id=1000 AND ticket_hash=?").bind(&hash).execute(&pool).await.unwrap();
        // Actual engine qualification: statement succeeds; only COMMIT fails.
        // This does NOT substitute for the production callback exercised below.
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("BEGIN IMMEDIATE").execute(&mut *connection).await.unwrap();
        let removed = sqlx::query("DELETE FROM user_sessions WHERE user_id=1000 AND ticket_hash=?").bind(&hash).execute(&mut *connection).await.unwrap().rows_affected();
        assert_eq!(removed, 1, "deferred FK must allow the DELETE statement");
        let error = sqlx::query("COMMIT").execute(&mut *connection).await.unwrap_err();
        assert!(matches!(&error, sqlx::Error::Database(db) if db.is_foreign_key_violation()), "commit must specifically report a deferred foreign key violation: {error}");
        sqlx::query("ROLLBACK").execute(&mut *connection).await.unwrap();
        println!("F-SRV-003 V3_COMMIT_QUALIFIED delete_rows={removed} actual_commit_error={error} explicit_rollback=true");
        connection.return_to_pool().await;
    });
    assert_eq!(ticket_count(&pool, fingerprint), 1);
    let owner = transport.maintenance_owner(91);
    transport.maintenance_tick(); // REAL configure_secure_close_handler -> storage.
    println!("F-SRV-003 V3_COMMIT_FAULT retained={:?} ticket={} online={} urls={}",transport.maintenance_counts(),ticket_count(&pool,fingerprint),count(&pool,"SELECT is_online FROM users WHERE id=1000"),count(&pool,"SELECT COUNT(*) FROM station_urls WHERE user_id=1000"));
    assert_eq!(transport.maintenance_counts(), (2, 2, 0, 1));
    assert_eq!(transport.maintenance_owner(91), owner);
    assert_eq!(ticket_count(&pool, fingerprint), 1);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM close_commit_child"), 1);
    assert!(!transport.maintenance_routable(91));
    assert!(transport.maintenance_routable(92));
    run(sqlx::query("DROP TABLE close_commit_child").execute(&pool)).unwrap();
    transport.maintenance_advance_retry_clock(Duration::from_secs(60));
    transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 0));
    assert_eq!(ticket_count(&pool, fingerprint), 0);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1000"), 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM station_urls WHERE user_id=1000"), 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM user_sessions WHERE user_id=1001"), 1);
    assert_eq!(count(&pool, "SELECT is_online FROM users WHERE id=1001"), 1);
    println!("F-SRV-003 V3_COMMIT_FAULT recovered: original_ticket=0 online=0 urls=0 other_user_unchanged=true");
}

// Real PRUDP wire envelope + real loopback ACK. No server receive loop or service.
use quazal::prudp::packet::{PacketFlag, StreamType, VPort};
// Input-side RC4 encoder copied from the pinned packet.rs, not a business-policy model.
mod fixture_wire_cipher {
pub(crate) fn crypt_key(key: &[u8], data: &[u8]) -> Vec<u8> {
    let rc4 = Rc4::new(key);
    rc4.zip(data).map(|(a, b)| a ^ b).collect()
}

/// Implements the RC4 cipher.
#[derive(Clone)]
pub(crate) struct Rc4 {
    i: u8,
    j: u8,
    state: [u8; 256],
}

impl Rc4 {
    /// Creates a new RC4 cipher with the given key.
    /// This initializes the S-box and the internal state (i, j) based on the key-scheduling algorithm (KSA).
    #[must_use]
    pub fn new(key: &[u8]) -> Rc4 {
        #![allow(clippy::cast_possible_truncation)]

        assert!(!key.is_empty() && key.len() <= 256);
        let mut rc4 = Rc4 { i: 0, j: 0, state: [0; 256] };
        for (i, x) in rc4.state.iter_mut().enumerate() {
            *x = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(rc4.state[i]).wrapping_add(key[i % key.len()]);
            rc4.state.swap(i, j as usize);
        }
        rc4
    }
}

impl Iterator for Rc4 {
    type Item = u8;

    /// Generates the next byte of the keystream using the pseudo-random generation algorithm (PRGA).
    fn next(&mut self) -> Option<Self::Item> {
        self.i = self.i.wrapping_add(1);
        self.j = self.j.wrapping_add(self.state[self.i as usize]);
        self.state.swap(self.i as usize, self.j as usize);
        let k = self.state[(self.state[self.i as usize].wrapping_add(self.state[self.j as usize])) as usize];
        Some(k)
    }
}

}
use fixture_wire_cipher::crypt_key;
use quazal::rmc::basic::{ReadStream, ToStream};
use quazal::kerberos::KerberosTicketInternal;
use sodiumoxide::crypto::secretbox;
use std::net::UdpSocket;

struct WirePeer { socket: UdpSocket }
impl WirePeer {
    fn new() -> Self {
        sodiumoxide::init().unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        Self { socket }
    }
    fn address(&self) -> std::net::SocketAddr { self.socket.local_addr().unwrap() }
    fn packet(kind: PacketType) -> QPacket {
        QPacket { source: VPort { port: 15, stream_type: StreamType::RVSec },
            destination: VPort { port: 1, stream_type: StreamType::RVSec },
            packet_type: kind, flags: PacketFlag::HasSize.into(), ..QPacket::default() }
    }
    fn dispatch(&self, transport: &mut NativeServer<'_>, ctx: &Context, packet: QPacket) {
        let wire = packet.to_bytes(ctx);
        let (decoded, _) = QPacket::from_bytes(ctx, &wire).unwrap();
        decoded.validate(ctx, &wire).unwrap();
        assert_eq!(decoded.payload, packet.payload);
        transport.maintenance_dispatch(decoded, self.address());
    }
    fn ack(&self, ctx: &Context, kind: PacketType) -> Option<QPacket> {
        let mut buffer = [0_u8; 4096];
        match self.socket.recv_from(&mut buffer) {
            Ok((n, source)) => {
                assert!(source.ip().is_loopback());
                let (ack, _) = QPacket::from_bytes(ctx, &buffer[..n]).unwrap();
                ack.validate(ctx, &buffer[..n]).unwrap();
                assert_eq!(ack.packet_type, kind);
                assert!(ack.flags.contains(PacketFlag::Ack));
                Some(ack)
            }
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => None,
            Err(error) => panic!("loopback ACK receive failed: {error}"),
        }
    }
    fn syn(&self, transport: &mut NativeServer<'_>, ctx: &Context) -> Option<u32> {
        self.dispatch(transport, ctx, QPacket { conn_signature: Some(0), ..Self::packet(PacketType::Syn) });
        self.ack(ctx, PacketType::Syn).map(|ack| ack.conn_signature.expect("real SYN ACK signature"))
    }
    fn connect(&self, transport: &mut NativeServer<'_>, ctx: &Context, signature: u32, user: u32, key_byte: u8) -> bool {
        let key = [key_byte; 16];
        let ticket = KerberosTicketInternal { principle_id: user, valid_until: u64::MAX, session_key: key };
        let nonce = secretbox::gen_nonce();
        let mut sealed = nonce.as_ref().to_vec();
        sealed.extend(secretbox::seal(&ticket.to_bytes(), &nonce, &ctx.ticket_key));
        let mut challenge = user.to_bytes(); challenge.extend(7_u32.to_bytes()); challenge.extend(u32::MAX.to_bytes());
        let encrypted = crypt_key(&key, &challenge);
        let mut payload = sealed.to_bytes(); payload.extend(encrypted.to_bytes());
        self.dispatch(transport, ctx, QPacket { signature, conn_signature: Some(0x1234), session_id: 7, sequence: 1, payload, ..Self::packet(PacketType::Connect) });
        let Some(ack) = self.ack(ctx, PacketType::Connect) else { return false; };
        let response: Vec<u8> = ReadStream::from_bytes(&ack.payload).read().unwrap();
        assert_eq!(response, 0_u32.to_bytes(), "genuine encrypted challenge ACK");
        let prefix = format!("{:?}|{:?}|", Some(user), Some(quazal::kerberos::ticket_fingerprint(&key)));
        assert!(transport.maintenance_owner(signature).unwrap().starts_with(&prefix));
        true
    }
    fn disconnect(&self, transport: &mut NativeServer<'_>, ctx: &Context, signature: u32) {
        self.dispatch(transport, ctx, QPacket { signature, session_id: 7, sequence: 2, ..Self::packet(PacketType::Disconnect) });
        assert!(self.ack(ctx, PacketType::Disconnect).is_some(), "real disconnect ACK");
    }
}

#[test]
fn fsrv003_closing_owner_rejects_payload_but_other_player_really_connects() {
    let (storage, pool) = fixture();
    let ctx = Context::splinter_cell_blacklist();
    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut transport = server(&ctx, &logger, &storage); transport.bind("127.0.0.1:0").unwrap();
    let old = client(&storage, 1000, 6, true); let old_fp = old.ticket_fingerprint.unwrap();
    transport.maintenance_seed_connected(31, old, true);
    fault_online_transition(&pool); transport.maintenance_tick();
    let owner = transport.maintenance_owner(31); assert!(owner.is_some());
    assert_eq!(transport.maintenance_counts(), (1, 1, 0, 1));
    for packet_type in [PacketType::Data, PacketType::Ping, PacketType::Disconnect, PacketType::Connect] {
        transport.maintenance_dispatch(QPacket { packet_type, signature:31, session_id:0, conn_signature:Some(999), fragment_id:Some(1), sequence:44, payload:vec![1,2,3], ..QPacket::default() }, "127.0.0.1:30500".parse().unwrap());
        assert_eq!(transport.maintenance_owner(31), owner);
        assert_eq!(transport.maintenance_counts(), (1, 1, 0, 1));
    }
    let fresh = client(&storage, 1001, 7, false); let fresh_fp = fresh.ticket_fingerprint.unwrap();
    let peer = WirePeer::new(); let signature = peer.syn(&mut transport, &ctx).expect("one failed owner must not block unrelated SYN");
    assert!(peer.connect(&mut transport,&ctx,signature,1001,7));
    assert!(storage.mark_secure_ticket(1001, Some(fresh_fp)).unwrap());
    assert_eq!(transport.maintenance_owner(31),owner); assert_eq!(ticket_count(&pool,old_fp),1);
    assert_eq!(transport.maintenance_counts(),(2,2,0,1));
    recover(&pool); transport.maintenance_advance_retry_clock(Duration::from_secs(60)); transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(),(1,1,0,0)); assert_eq!(ticket_count(&pool,old_fp),0);
    assert_eq!(ticket_count(&pool,fresh_fp),1); assert!(transport.maintenance_owner(signature).is_some());
    println!("F-SRV-003 V7_ADMISSION closing_payload_rejected=true unrelated_syn_connect_ack=true old_identity_unchanged=true retry_recovered=true");
}

#[test]
fn fsrv003_v7_pending_syn_capacity_rejects_only_new_reservations() {
    let (storage, _pool) = fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!());
    let mut transport=server(&ctx,&logger,&storage); transport.bind("127.0.0.1:0").unwrap();
    let (total,pending)=transport.maintenance_capacity_limits().unwrap(); assert_eq!((total,pending),(256,64),"approved explicit resource policy, not a room/player limit");
    let peer=WirePeer::new();
    for n in 0..pending { assert!(peer.syn(&mut transport,&ctx).is_some()); assert_eq!(transport.maintenance_counts(),(0,0,n+1,0)); }
    assert!(peer.syn(&mut transport,&ctx).is_none()); assert_eq!(transport.maintenance_counts(),(0,0,pending,0));
    println!("F-SRV-003 V7_PENDING pending={pending} full_real_syn_has_no_ack=true total_bound={total}");
}

#[test]
fn fsrv003_v7_total_capacity_allows_pending_promotion_and_retry_releases_slot() {
    let (storage,pool)=fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!());
    let mut transport=server(&ctx,&logger,&storage); transport.bind("127.0.0.1:0").unwrap();
    let (total,_)=transport.maintenance_capacity_limits().unwrap();
    transport.maintenance_seed_connected(401,client(&storage,1000,41,true),true);
    for i in 0..total-2 { transport.maintenance_seed_connected(0x10000+i as u32,ClientInfo::new("127.0.0.1:30501".parse().unwrap()),false); }
    fault_online_transition(&pool); transport.maintenance_tick(); assert_eq!(transport.maintenance_counts(),(total-1,total-1,0,1));
    let fresh=client(&storage,1001,42,false); let fp=fresh.ticket_fingerprint.unwrap(); let peer=WirePeer::new();
    let pending=peer.syn(&mut transport,&ctx).unwrap(); assert_eq!(transport.maintenance_counts(),(total-1,total-1,1,1));
    assert!(peer.syn(&mut transport,&ctx).is_none());
    assert!(peer.connect(&mut transport,&ctx,pending,1001,42),"full-total promotion consumes its already reserved slot");
    assert!(storage.mark_secure_ticket(1001,Some(fp)).unwrap()); assert_eq!(transport.maintenance_counts(),(total,total,0,1));
    assert!(peer.syn(&mut transport,&ctx).is_none());
    recover(&pool); transport.maintenance_advance_retry_clock(Duration::from_secs(60)); transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(),(total-1,total-1,0,0));
    assert!(peer.syn(&mut transport,&ctx).is_some()); assert_eq!(transport.maintenance_counts(),(total-1,total-1,1,0));
    assert_eq!(ticket_count(&pool,fp),1);
    println!("F-SRV-003 V7_TOTAL total={total} full_pending_promotes=true new_syn_rejected=true successful_close_releases_one=true");
}

#[test]
fn fsrv003_v7_actual_reservation_collision_budget_and_connect_vacancy_preserve_owners() {
    let (storage,_pool)=fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!());
    let mut transport=server(&ctx,&logger,&storage); transport.bind("127.0.0.1:0").unwrap(); let a=WirePeer::new(); let b=WirePeer::new();
    transport.maintenance_seed_connected(901,client(&storage,1000,90,true),false); let active=transport.maintenance_owner(901);
    assert_eq!(transport.maintenance_reserve_syn(a.address(),&[902]),Some(902));
    // Exact production reserve_syn_client also serves real handle_syn in BOTH snapshots.
    assert_eq!(transport.maintenance_reserve_syn(b.address(),&[901,902,903]),Some(903));
    assert_eq!(transport.maintenance_owner(901),active); assert_eq!(transport.maintenance_pending_address(902),Some(a.address()));
    assert_eq!(transport.maintenance_reserve_syn(b.address(),&[901,902,901,902,901,902,901,902,904]),None,"only eight actual candidate draws");
    assert_eq!(transport.maintenance_pending_address(904),None);
    // Deliberately model an already-existing overlap using the owner-seeding seam;
    // the CONNECT action itself is a real encrypted, serialized QPacket dispatch.
    transport.maintenance_seed_connected(902,client(&storage,1001,91,true),false); let owner=transport.maintenance_owner(902);
    let _=client(&storage,1000,92,false);
    assert!(!a.connect(&mut transport,&ctx,902,1000,92));
    assert_eq!(transport.maintenance_owner(902),owner); assert_eq!(transport.maintenance_pending_address(902),Some(a.address()));
    println!("F-SRV-003 V7_COLLISION active_and_pending_not_overwritten=true eight_draw_limit=true conflicting_connect_keeps_pending=true");
}

#[test]
fn fsrv003_v7_closing_same_ticket_rejected_but_unregistered_new_ticket_survives_old_retry() {
    let (storage,pool)=fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!());
    let mut transport=server(&ctx,&logger,&storage); transport.bind("127.0.0.1:0").unwrap();
    let old_fp=client(&storage,1000,111,false).ticket_fingerprint.unwrap(); let old=WirePeer::new(); let old_sig=old.syn(&mut transport,&ctx).unwrap();
    assert!(old.connect(&mut transport,&ctx,old_sig,1000,111)); assert!(storage.mark_secure_ticket(1000,Some(old_fp)).unwrap());
    fault_online_transition(&pool); old.disconnect(&mut transport,&ctx,old_sig); let owner=transport.maintenance_owner(old_sig); assert!(owner.is_some());
    let repeat=WirePeer::new(); let repeated=repeat.syn(&mut transport,&ctx).unwrap();
    assert!(!repeat.connect(&mut transport,&ctx,repeated,1000,111),"closing same-ticket must receive no success ACK");
    assert_eq!(transport.maintenance_owner(repeated),None); assert_eq!(transport.maintenance_owner(old_sig),owner);
    let other_fp=client(&storage,1001,113,false).ticket_fingerprint.unwrap(); let other=WirePeer::new(); let other_sig=other.syn(&mut transport,&ctx).unwrap();
    assert!(other.connect(&mut transport,&ctx,other_sig,1001,113)); assert!(storage.mark_secure_ticket(1001,Some(other_fp)).unwrap());
    let new_fp=client(&storage,1000,112,false).ticket_fingerprint.unwrap(); let new=WirePeer::new(); let new_sig=new.syn(&mut transport,&ctx).unwrap();
    assert!(new.connect(&mut transport,&ctx,new_sig,1000,112)); let new_owner=transport.maintenance_owner(new_sig);
    assert_eq!(count(&pool,"SELECT COUNT(*) FROM user_sessions WHERE user_id=1000 AND native_connected=0"),1,"new real CONNECT has deliberately NOT registered yet");
    recover(&pool); transport.maintenance_advance_retry_clock(Duration::from_secs(60)); transport.maintenance_tick();
    assert_eq!(ticket_count(&pool,old_fp),0); assert_eq!(ticket_count(&pool,new_fp),1,"exact old close must preserve new unregistered ticket");
    assert_eq!(transport.maintenance_owner(new_sig),new_owner); assert_eq!(transport.maintenance_counts(),(2,2,0,0));
    assert!(storage.mark_secure_ticket(1000,Some(new_fp)).unwrap(),"later real Register storage leaf must succeed");
    assert_eq!(ticket_count(&pool,other_fp),1); assert_eq!(count(&pool,"SELECT is_online FROM users WHERE id=1001"),1);
    println!("F-SRV-003 V7_TICKETS closing_same_fp_no_ack=true other_user_ack=true new_fp_unregistered_ack=true old_retry_preserves_new=true later_mark=true");
}

#[test]
fn fsrv003_v7_two_real_connects_share_ticket_only_last_owner_closes_database() {
    let (storage,pool)=fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!());
    let mut transport=server(&ctx,&logger,&storage); transport.bind("127.0.0.1:0").unwrap();
    let calls=Rc::new(RefCell::new(0_usize)); let observed=Rc::clone(&calls); let mut actual=transport.close_handler.take().unwrap();
    transport.close_handler=Some(Box::new(move|ci,reason|{*observed.borrow_mut()+=1;actual(ci,reason)}));
    let fp=client(&storage,1000,121,false).ticket_fingerprint.unwrap(); let a=WirePeer::new(); let b=WirePeer::new();
    let sa=a.syn(&mut transport,&ctx).unwrap(); assert!(a.connect(&mut transport,&ctx,sa,1000,121)); assert!(storage.mark_secure_ticket(1000,Some(fp)).unwrap());
    let sb=b.syn(&mut transport,&ctx).unwrap(); assert!(b.connect(&mut transport,&ctx,sb,1000,121));
    let survivor=transport.maintenance_owner(sb); a.disconnect(&mut transport,&ctx,sa);
    assert_eq!(*calls.borrow(),0,"live peer owns the same ticket: no application close"); assert_eq!(transport.maintenance_counts(),(1,1,0,0));
    assert_eq!(transport.maintenance_owner(sb),survivor); assert_eq!(ticket_count(&pool,fp),1); assert!(storage.mark_secure_ticket(1000,Some(fp)).unwrap());
    fault_online_transition(&pool); b.disconnect(&mut transport,&ctx,sb); assert_eq!(*calls.borrow(),1);
    assert_eq!(transport.maintenance_counts(),(1,1,0,1)); assert_eq!(ticket_count(&pool,fp),1);
    recover(&pool); transport.maintenance_advance_retry_clock(Duration::from_secs(60)); transport.maintenance_tick();
    assert_eq!(*calls.borrow(),2); assert_eq!(transport.maintenance_counts(),(0,0,0,0)); assert_eq!(ticket_count(&pool,fp),0);
    println!("F-SRV-003 V7_SHARED two_real_connect_ack=true first_disconnect_db_calls=0 last_fault_retained=true retry_db_closed=true");
}

#[test]
fn fsrv003_v7_simultaneously_expired_shared_ticket_owners_do_not_mutually_preserve() {
    let (storage,pool)=fixture(); let ctx=Context::splinter_cell_blacklist(); let logger=slog::Logger::root(slog::Discard,slog::o!()); let mut transport=server(&ctx,&logger,&storage);
    let first=client(&storage,1000,122,true); let fp=first.ticket_fingerprint.unwrap();
    let mut second=ClientInfo::new("127.0.0.1:30501".parse().unwrap()); second.user_id=Some(1000); second.ticket_fingerprint=Some(fp);
    transport.maintenance_seed_connected(1221,first,true); transport.maintenance_seed_connected(1222,second,true);
    // Age alone is seeded; actual expiry scan, shared-owner decision, close SQL
    // and registry retirement run unchanged. Real dual CONNECT covered above.
    transport.maintenance_tick();
    assert_eq!(transport.maintenance_counts(),(0,0,0,0)); assert_eq!(ticket_count(&pool,fp),0);
    assert_eq!(count(&pool,"SELECT is_online FROM users WHERE id=1000"),0);
    println!("F-SRV-003 V7_EXPIRE two_aged_same_ticket_owners=retired no_permanent_mutual_preservation=true");
}

include!("host_presence_pending_ticket_tests.rs");
