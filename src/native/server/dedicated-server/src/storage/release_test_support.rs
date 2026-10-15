use super::*;
pub(crate) fn fixture() -> (Arc<Storage>, SqlitePool) {
    fixture_with_logger(Logger::root(slog::Discard,slog::o!()))
}
pub(crate) fn fixture_with_logger(logger: Logger) -> (Arc<Storage>, SqlitePool) {
    let unique=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir=std::env::var_os("SCBL_RELEASE_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path=dir.join(format!("release-admission-{}-{unique}.db",std::process::id()));
    let url=format!("sqlite://{}?mode=rwc",path.display());
    let storage=Storage::init_with_database_url(logger,&url).unwrap();
    let pool=storage.pool.clone();(Arc::new(storage),pool)
}
use std::sync::Arc;
