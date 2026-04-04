use std::sync::Arc;

use super::AppState;

pub fn authenticate(state: &Arc<AppState>, api_key: Option<&str>) -> bool {
    match api_key {
        Some(key) if !key.is_empty() => key == state.api_key,
        _ => false,
    }
}
