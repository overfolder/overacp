use std::sync::Arc;

use crate::store::SessionStore;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn SessionStore>,
}

impl AppState {
    pub fn new(store: Arc<dyn SessionStore>) -> Self {
        Self { store }
    }
}
