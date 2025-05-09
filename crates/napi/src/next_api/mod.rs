pub mod endpoint;
pub mod project;
pub mod utils;

use std::sync::{LazyLock, Mutex};

// Unfortunately, napi-rs doesn't let us attach structured data to thrown errors. We store error
// locations here so they can be separately requested by JS.
pub static LAST_ERROR_LOCATION: LazyLock<Mutex<Option<String>>> =
    LazyLock::new(|| Mutex::new(None));

#[napi]
pub fn get_last_turbopack_error_location() -> napi::Result<Option<String>> {
    let lock = LAST_ERROR_LOCATION.lock().unwrap();
    Ok(lock.clone())
}
