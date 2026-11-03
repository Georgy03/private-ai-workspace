mod cloud;
mod commands;
mod db;
mod entities;
mod error;
mod merge;
mod model_catalog;
pub mod models;
pub mod parser;
pub mod parser_ipc;
mod provider;
mod state;

use std::sync::Arc;

use db::Database;
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let database = Database::open_default().expect("encrypted workspace database must open");
    let cloud = cloud::Cloud::load(&db::workspace_dir().expect("workspace folder"));
    let state = Arc::new(AppState::new(database, cloud));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_workspace,
            commands::import_model_catalog,
            commands::local_model_runtime_status,
            commands::install_verified_model,
            commands::start_index,
            commands::cancel_index,
            commands::search_documents,
            commands::answer_question,
            commands::cloud_status,
            commands::cloud_sign_in,
            commands::cloud_connectors,
            commands::cloud_connect,
            commands::cloud_disconnect,
            commands::cloud_sync_now,
            commands::cloud_sign_out,
            commands::set_cloud_enabled,
            commands::document_preview,
            commands::open_document_link,
            commands::cloud_connection_files,
            commands::cloud_agents_call,
            commands::cloud_knowledge,
            commands::cloud_knowledge_enable,
            commands::cloud_knowledge_list,
            commands::cloud_knowledge_review,
            commands::cloud_knowledge_profile_review,
            commands::cloud_usage,
            commands::cloud_set_usage_limit,
            commands::list_entities,
            commands::save_entity,
            commands::remove_entity,
            commands::clear_local_data,
            commands::install_sample,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Private AI Connector Workspace");
}

pub fn run_parser_sidecar() -> i32 {
    parser_ipc::run_stdio()
}
