//! Nash AI Engine desktop application (Tauri v2).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use std::sync::Arc;

use state::AppState;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let app_state = AppState::load().expect("failed to initialise Nash application state");
    run(app_state);
}

fn run(app_state: Arc<AppState>) {
    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::get_paths,
            commands::get_config,
            commands::save_config,
            commands::list_providers,
            commands::list_models,
            commands::test_provider,
            commands::test_all,
            commands::chat,
            commands::chat_stream,
            commands::cancel_chat,
            commands::list_secrets,
            commands::set_secret,
            commands::remove_secret,
            commands::set_provider_key,
            commands::list_sessions,
            commands::get_messages,
            commands::create_session,
            commands::delete_session,
            commands::rename_session,
            commands::set_session_pinned,
            commands::agent_run,
            commands::resolve_approval,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Nash desktop application");
}
