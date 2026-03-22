use crate::config::Config;
use crate::keystore::Keystore;

pub struct AppState {
    pub config: Config,
    pub keystore: Keystore,
    pub master_password: String,
    pub auth_token: String,
}
