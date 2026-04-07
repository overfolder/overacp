use std::sync::Arc;

use crate::api::ProviderRegistry;
use crate::store::SessionStore;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn SessionStore>,
    pub providers: Arc<ProviderRegistry>,
}

impl AppState {
    pub fn new(store: Arc<dyn SessionStore>, providers: Arc<ProviderRegistry>) -> Self {
        Self { store, providers }
    }
}
