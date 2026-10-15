//! Local game-session publication: desired state, serialized RPC, and scheduled ownership.
//! Public callers retain the parent API entry points.

use super::{create_channel, runtime, Error, TOKEN};
#[cfg(test)]
use super::event;
use server_api::misc::misc_client::MiscClient;
use server_api::misc::{ClearGameSessionRequest, SetGameSessionRequest, UplayGameSession};
use std::sync::{Mutex, OnceLock};
use tracing::info;

#[derive(Debug, Default)]
struct GameSessionPublishState {
    desired: Option<UplayGameSession>,
    desired_generation: u64,
    applied_generation: u64,
    worker_scheduled: bool,
}

impl GameSessionPublishState {
    fn update(&mut self, desired: Option<UplayGameSession>) {
        self.desired = desired;
        self.desired_generation = self.desired_generation.saturating_add(1);
    }

    fn force_republish(&mut self) {
        if self.desired.is_some() {
            self.desired_generation = self.desired_generation.saturating_add(1);
        }
    }

    fn pending(&self) -> Option<(u64, Option<UplayGameSession>)> {
        (self.applied_generation != self.desired_generation).then(|| (self.desired_generation, self.desired.clone()))
    }

    fn mark_applied(&mut self, generation: u64) {
        self.applied_generation = self.applied_generation.max(generation);
    }

    fn reserve_worker(&mut self) -> bool {
        if self.worker_scheduled || self.applied_generation == self.desired_generation {
            return false;
        }
        self.worker_scheduled = true;
        true
    }

    fn finish_worker_pass(&mut self, failed_generation: Option<u64>) -> bool {
        if self.applied_generation != self.desired_generation
            && Some(self.desired_generation) != failed_generation
        {
            return false;
        }
        // Check pending work and release its owner under the same state lock:
        // an update either belongs to this worker or can reserve the next one.
        self.worker_scheduled = false;
        true
    }
}

static GAME_SESSION_PUBLISH_STATE: Mutex<GameSessionPublishState> = Mutex::new(GameSessionPublishState {
    desired: None,
    desired_generation: 0,
    applied_generation: 0,
    worker_scheduled: false,
});
static GAME_SESSION_PUBLISH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

// Constructed before spawning, so dropping an unpolled/cancelled future also
// releases the reservation. Drop never starts tasks or performs network work.
struct PublicationWorker {
    armed: bool,
}

impl PublicationWorker {
    fn reserve() -> Option<Self> {
        let reserved = GAME_SESSION_PUBLISH_STATE.lock().unwrap().reserve_worker();
        reserved.then(|| Self { armed: true })
    }

    fn finish_pass(&mut self, failed_generation: Option<u64>) -> bool {
        let mut state = GAME_SESSION_PUBLISH_STATE.lock().unwrap();
        if state.finish_worker_pass(failed_generation) {
            self.armed = false;
            true
        } else {
            false
        }
    }
}

impl Drop for PublicationWorker {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(mut state) = GAME_SESSION_PUBLISH_STATE.lock() {
                state.worker_scheduled = false;
            }
        }
    }
}

async fn publish_game_session_remote(session: UplayGameSession) -> Result<(), Error> {
    let mut client = connect!(MiscClient);
    client
        .set_game_session(tonic::Request::new(SetGameSessionRequest {
            id: session.id,
            data: session.data,
            flags: session.flags,
            invite_only: session.invite_only,
        }))
        .await?;
    Ok(())
}

async fn clear_game_session_remote() -> Result<(), Error> {
    let mut client = connect!(MiscClient);
    client.clear_game_session(tonic::Request::new(ClearGameSessionRequest {})).await?;
    Ok(())
}

async fn synchronize_game_session(reason: &'static str) {
    let publish_lock = GAME_SESSION_PUBLISH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = publish_lock.lock().await;
    let _ = synchronize_game_session_locked(reason).await;
}

async fn synchronize_game_session_locked(reason: &'static str) -> Option<u64> {
    loop {
        let Some((generation, desired)) = GAME_SESSION_PUBLISH_STATE.lock().unwrap().pending() else {
            return None;
        };
        let result = match desired {
            Some(session) => publish_game_session_remote(session).await,
            None => clear_game_session_remote().await,
        };
        match result {
            Ok(()) => {
                GAME_SESSION_PUBLISH_STATE.lock().unwrap().mark_applied(generation);
                info!("UplayGameSessionSyncSucceeded generation={generation} reason={reason}");
            }
            Err(error) => {
                tracing::warn!("UplayGameSessionSyncDeferred generation={generation} reason={reason} error={error}");
                return Some(generation);
            }
        }
    }
}

async fn run_publication_worker(reason: &'static str, mut owner: PublicationWorker) {
    let publish_lock = GAME_SESSION_PUBLISH_LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    let _guard = publish_lock.lock().await;
    loop {
        let failed_generation = synchronize_game_session_locked(reason).await;
        if owner.finish_pass(failed_generation) {
            return;
        }
        // A newer update arrived at completion. Continue with this owner; do
        // not spin on an unchanged failed generation or drop the newer update.
    }
}

// Native updates and healthy polls share one reservation, including the time
// before the executor first polls it. Direct login synchronization still waits
// on the same RPC lock; no queue of one spawned task per native update.
pub(super) fn retry_pending_game_session() {
    schedule_game_session_sync("poll-retry");
}

fn schedule_game_session_sync(reason: &'static str) {
    let Some(owner) = PublicationWorker::reserve() else { return; };
    match runtime() {
        Ok(runtime) => {
            runtime.spawn(run_publication_worker(reason, owner));
        }
        Err(error) => tracing::warn!("UplayGameSessionSyncRuntimeUnavailable reason={reason} error={error}"),
    }
}

pub fn publish_game_session(id: u64, data: Vec<u8>, flags: u32, invite_only: bool) {
    let session = UplayGameSession { id, data, flags, invite_only };
    GAME_SESSION_PUBLISH_STATE.lock().unwrap().update(Some(session));
    schedule_game_session_sync("set");
}

pub fn clear_game_session() {
    GAME_SESSION_PUBLISH_STATE.lock().unwrap().update(None);
    schedule_game_session_sync("clear");
}

pub(super) async fn republish_local_game_session() {
    GAME_SESSION_PUBLISH_STATE.lock().unwrap().force_republish();
    synchronize_game_session("login").await;
}

#[cfg(test)]
mod tests {
    use super::{GameSessionPublishState, UplayGameSession};

    #[test]
    fn set_clear_set_coalesces_to_the_latest_session_generation() {
        let mut state = GameSessionPublishState::default();
        state.update(Some(UplayGameSession {
            id: 10,
            data: vec![1],
            flags: 2,
            invite_only: false,
        }));
        state.update(None);
        state.update(Some(UplayGameSession {
            id: 11,
            data: vec![3],
            flags: 4,
            invite_only: true,
        }));

        let (generation, desired) = state.pending().unwrap();
        assert_eq!(generation, 3);
        assert_eq!(desired.unwrap().id, 11);
    }

    #[test]
    fn completing_an_older_request_does_not_hide_a_newer_desired_state() {
        let mut state = GameSessionPublishState::default();
        state.update(Some(UplayGameSession {
            id: 10,
            data: Vec::new(),
            flags: 0,
            invite_only: false,
        }));
        let old_generation = state.pending().unwrap().0;
        state.update(None);
        state.mark_applied(old_generation);

        let (pending_generation, desired) = state.pending().unwrap();
        assert!(pending_generation > old_generation);
        assert!(desired.is_none());
    }

    #[test]
    fn relogin_republishes_only_when_a_local_session_exists() {
        let mut empty = GameSessionPublishState::default();
        empty.force_republish();
        assert!(empty.pending().is_none());

        let mut populated = GameSessionPublishState::default();
        populated.update(Some(UplayGameSession {
            id: 10,
            data: Vec::new(),
            flags: 0,
            invite_only: false,
        }));
        let first_generation = populated.pending().unwrap().0;
        populated.mark_applied(first_generation);
        populated.force_republish();
        assert!(populated.pending().unwrap().0 > first_generation);
    }
}

#[cfg(test)]
mod publication_owner_tests {
    use super::GameSessionPublishState;

    #[test]
    fn empty_state_never_reserves_a_worker() {
        let mut state = GameSessionPublishState::default();
        assert!(!state.reserve_worker());
    }

    #[test]
    fn all_updates_share_reserved_worker() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        for _ in 0..100 {
            state.update(None);
            assert!(!state.reserve_worker());
        }
        assert!(!state.finish_worker_pass(None));
        assert!(state.worker_scheduled);
    }

    #[test]
    fn completed_generation_releases_owner() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        state.mark_applied(state.desired_generation);
        assert!(state.finish_worker_pass(None));
        assert!(!state.worker_scheduled);
    }

    #[test]
    fn unchanged_failure_waits_for_external_retry() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        assert!(state.finish_worker_pass(Some(state.desired_generation)));
        assert!(state.pending().is_some());
        assert!(state.reserve_worker());
    }

    #[test]
    fn newer_update_before_completion_keeps_owner() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        state.mark_applied(state.desired_generation);
        state.update(None);
        assert!(!state.finish_worker_pass(None));
        assert!(!state.reserve_worker());
    }

    #[test]
    fn newer_update_after_completion_gets_new_owner() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        state.mark_applied(state.desired_generation);
        assert!(state.finish_worker_pass(None));
        state.update(None);
        assert!(state.reserve_worker());
    }

    #[test]
    fn failure_of_old_generation_does_not_drop_new_work() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        let failed = state.desired_generation;
        state.update(None);
        assert!(!state.finish_worker_pass(Some(failed)));
        assert!(state.worker_scheduled);
    }

    #[test]
    fn no_pending_state_releases_even_after_old_failure() {
        let mut state = GameSessionPublishState::default();
        state.update(None);
        assert!(state.reserve_worker());
        let failed = state.desired_generation;
        state.mark_applied(failed);
        assert!(state.finish_worker_pass(Some(failed)));
        assert!(!state.worker_scheduled);
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod maintenance_publication_rpc;
