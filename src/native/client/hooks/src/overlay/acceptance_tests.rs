use super::*;
fn event(id: i64) -> InviteEvent {
    InviteEvent {
        id,
        ..Default::default()
    }
}
fn active(id: i64) -> (AcceptanceQueue, crossbeam_channel::Sender<Reply>) {
    let (tx, reply) = crossbeam_channel::bounded(1);
    (
        AcceptanceQueue {
            waiting: VecDeque::new(),
            active: Some(Pending {
                event: event(id),
                reply,
            }),
        },
        tx,
    )
}
#[test]
fn queue_satisfies_render_thread_send_sync_contract() {
    fn check<T: Send + Sync>() {}
    check::<AcceptanceQueue>();
}
#[test]
fn pending_duplicate_id_is_not_queued_twice() {
    let (mut queue, _tx) = active(17);
    queue.submit(event(17));
    assert!(queue.waiting.is_empty());
}
#[test]
fn queued_duplicate_is_ignored_but_new_generation_is_retained() {
    let (mut queue, _tx) = active(17);
    queue.submit(event(18));
    queue.submit(event(18));
    queue.submit(event(19));
    assert_eq!(
        queue.waiting.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![18, 19]
    );
}
#[test]
fn pending_reply_returns_without_consuming_active_state() {
    let (mut queue, _tx) = active(17);
    assert!(queue.poll().is_none());
    assert_eq!(queue.active.as_ref().unwrap().event.id, 17);
}
#[test]
fn completed_reply_preserves_local_id_and_waiting_order() {
    let (mut queue, tx) = active(17);
    queue.submit(event(18));
    tx.send(Ok((true, 7))).unwrap();
    let (event, response) = queue.poll().unwrap();
    assert_eq!(event.id, 17);
    assert_eq!(response.unwrap(), (true, 7));
    assert!(queue.active.is_none());
    assert_eq!(queue.waiting.front().unwrap().id, 18);
}
#[test]
fn disconnected_worker_becomes_retryable_error_instead_of_stuck_pending() {
    let (mut queue, tx) = active(17);
    drop(tx);
    let (event, response) = queue.poll().unwrap();
    assert_eq!(event.id, 17);
    assert!(
        matches!(response,Err(Error::IO(error)) if error.kind()==std::io::ErrorKind::BrokenPipe)
    );
    assert!(queue.active.is_none());
}
