//! Network maintenance is process-owned; the overlay is only an optional sink.
use crossbeam_channel::Sender;
use server_api::misc::InviteEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tracing::{info, warn};

type PollEvent = Result<Option<InviteEvent>, crate::api::Error>;

#[derive(Default)]
struct Service {
    running: AtomicBool,
    sink: Mutex<Option<Sender<PollEvent>>>,
}

impl Service {
    fn attach(&self, sink: Option<Sender<PollEvent>>) {
        *self.sink.lock().unwrap_or_else(|p| p.into_inner()) = sink;
    }

    fn claim(&self) -> bool {
        self.running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn release(&self) {
        self.running.store(false, Ordering::Release);
    }

    fn publish(&self, event: PollEvent) {
        let mut sink = self.sink.lock().unwrap_or_else(|p| p.into_inner());
        if sink.as_ref().is_some_and(|tx| tx.send(event).is_err()) {
            // Delivery never acknowledges an invitation; the server keeps its
            // normal redelivery/expiry contract if the presentation disappears.
            *sink = None;
            warn!("API overlay receiver unavailable; network maintenance continues");
        }
    }

    fn finish_poll(&self, failures: &mut u32, event: PollEvent) -> bool {
        let reconnect = super::observe_event_poll(failures, &event);
        self.publish(event);
        reconnect
    }

    async fn poll_once(&self, failures: &mut u32) {
        let event = match crate::api::event().await {
            Ok(response) => {
                super::party::observe_authoritative_party_roster(response.party.as_ref());
                Ok(response.invite)
            }
            Err(error) => Err(error),
        };
        if self.finish_poll(failures, event) && crate::api::reconnect_and_relogin().await {
            *failures = 0;
        }
    }

    async fn run(&self) {
        let mut failures = 0;
        loop {
            // Preserve existing cadence and first/every-tenth-failure recovery.
            tokio::time::sleep(Duration::from_secs(1)).await;
            self.poll_once(&mut failures).await;
        }
    }
}

struct WorkerClaim(Arc<Service>);
impl Drop for WorkerClaim {
    fn drop(&mut self) {
        self.0.release();
    }
}
static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();

pub(super) fn start(sink: Option<Sender<PollEvent>>) {
    let service = SERVICE.get_or_init(|| Arc::new(Service::default())).clone();
    // Repeated Startup without a new overlay must not detach its live sink.
    if sink.is_some() {
        service.attach(sink);
    }
    if !service.claim() {
        return;
    }
    let worker = service.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("updates-thread".into())
        .spawn(move || {
            let _claim = WorkerClaim(worker.clone());
            match crate::api::runtime() {
                Ok(runtime) => runtime.block_on(worker.run()),
                Err(error) => warn!("API maintenance runtime unavailable: {error}"),
            }
        })
    {
        service.release();
        warn!("API maintenance thread unavailable: {error}");
    } else {
        info!("API maintenance started independently of overlay");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_receiver_does_not_stop_reconnect_or_reset_errors() {
        let service = Service::default();
        let mut failures = 0;
        assert!(service.finish_poll(&mut failures, Err(crate::api::Error::LoginFailure)));
        assert_eq!(failures, 1);
        assert!(!service.finish_poll(&mut failures, Err(crate::api::Error::LoginFailure)));
        assert_eq!(failures, 2);
        assert!(!service.finish_poll(&mut failures, Ok(None)));
        assert_eq!(failures, 0);
        assert!(service.finish_poll(&mut failures, Err(crate::api::Error::LoginFailure)));
    }
    #[test]
    fn dropped_receiver_detaches_without_releasing_worker() {
        let service = Service::default();
        assert!(service.claim());
        let (tx, rx) = crossbeam_channel::unbounded();
        service.attach(Some(tx));
        drop(rx);
        assert!(service.finish_poll(&mut 0, Err(crate::api::Error::LoginFailure)));
        assert!(service.sink.lock().unwrap().is_none());
        assert!(!service.claim());
        service.publish(Ok(None));
    }
    #[test]
    fn live_sink_receives_error_then_recovery_and_unchanged_invitation() {
        let service = Service::default();
        let (tx, rx) = crossbeam_channel::unbounded();
        service.attach(Some(tx));
        service.publish(Err(crate::api::Error::LoginFailure));
        assert!(rx.try_recv().unwrap().is_err());
        service.publish(Ok(None));
        assert!(rx.try_recv().unwrap().unwrap().is_none());
        let invite = InviteEvent {
            id: 42,
            force_join: true,
            kind: 5,
            ..Default::default()
        };
        service.publish(Ok(Some(invite.clone())));
        assert_eq!(rx.try_recv().unwrap().unwrap(), Some(invite));
    }
    #[test]
    fn replacement_sink_does_not_create_a_second_worker() {
        let service = Service::default();
        assert!(service.claim());
        let (tx1, rx1) = crossbeam_channel::unbounded();
        service.attach(Some(tx1));
        drop(rx1);
        service.publish(Ok(None));
        let (tx2, rx2) = crossbeam_channel::unbounded();
        service.attach(Some(tx2));
        assert!(!service.claim());
        service.publish(Ok(None));
        assert!(rx2.try_recv().unwrap().unwrap().is_none());
    }
    #[test]
    fn concurrent_start_has_one_owner_and_failed_start_can_retry() {
        let service = Arc::new(Service::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let s = service.clone();
                std::thread::spawn(move || s.claim())
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            1
        );
        drop(WorkerClaim(service.clone()));
        assert!(service.claim());
    }
}
