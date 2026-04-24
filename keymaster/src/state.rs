use crate::config::Config;
use crate::keystore::Keystore;
use crate::kill_switch::KillSwitch;

pub struct AppState {
    pub config: Config,
    pub keystore: Keystore,
    pub master_password: String,
    pub auth_token: String,
    pub kill_switch: KillSwitch,
}
