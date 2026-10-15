pub use hooks_addresses::Addresses;
use windows::Win32::Foundation::HMODULE;

use crate::get_executable;
use crate::macros::fatal_error;

pub fn get() -> Addresses {
    let Some(path) = get_executable(HMODULE::default()) else {
        fatal_error!("Couldn't find host process. Please start the game with SCBL.");
    };

    tracing::info!("Host process path: {:?}", path);

    match hooks_addresses::hash_file(&path) {
        Ok(hash) => {
            tracing::info!("Host process SHA256: {}", hash_to_hex(hash));
        }
        Err(e) => {
            tracing::error!("Failed to calculate host process SHA256: {e}");
        }
    }

    match hooks_addresses::get_from_path(&path) {
        Ok(a) => {
            tracing::info!("Game address detection succeeded.");
            a
        }
        Err(e) => {
            tracing::error!("Game address detection failed: {e}");
            fatal_error!(
                "SCBL LAN could not identify this game executable.\n\n\
                 Please send bl-tracing.log to the maintainer.\n\n\
                 Error: {e}"
            );
        }
    }
}

fn hash_to_hex(data: [u8; 32]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join("")
}
