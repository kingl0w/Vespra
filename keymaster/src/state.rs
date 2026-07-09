use zeroize::Zeroizing;

use crate::config::Config;
use crate::keystore::Keystore;

pub struct AppState {
    pub config: Config,
    pub keystore: Keystore,
    /// Wrapped so it is wiped from memory when AppState drops (e.g. shutdown).
    pub master_password: Zeroizing<String>,
    pub auth_token: String,
    /// When true, /swap refuses any request without a non-zero min_amount_out_wei,
    /// turning silently-unprotected swaps into loud rejections. Off by default.
    pub require_min_out: bool,
}
