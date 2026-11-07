mod cloud;
mod commands;
mod db;
mod entities;
mod email_draft;
mod error;
mod export;
mod helper_draft;
mod merge;
mod model_catalog;
pub mod models;
pub mod parser;
pub mod parser_ipc;
mod provider;
mod state;
mod workspaces;

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
            commands::attach_file,
            commands::remove_attachment,
            commands::answer_question,
            commands::cloud_status,
            commands::cloud_sign_in,
            commands::cloud_connectors,
            commands::cloud_connect,
            commands::cloud_disconnect,
            commands::cloud_set_scope,
            commands::cloud_sync_now,
            commands::cloud_sign_out,
            commands::set_cloud_enabled,
            commands::document_preview,
            commands::open_document_link,
            commands::open_prepared_link,
            commands::open_calendar_file,
            commands::export_report,
            commands::draft_helper,
            commands::ask_web,
            commands::draft_email,
            commands::cloud_send_email,
            commands::cloud_zoom_meeting,
            commands::cloud_zoom_meetings,
            commands::cloud_zoom_reschedule,
            commands::cloud_zoom_cancel,
            workspaces::workspace_list,
            workspaces::workspace_create,
            workspaces::workspace_update,
            workspaces::workspace_delete,
            workspaces::workspace_files,
            workspaces::workspace_add_files,
            workspaces::workspace_add_folder,
            workspaces::workspace_add_cloud,
            workspaces::workspace_remove_file,
            workspaces::workspace_chats,
            workspaces::workspace_chat,
            workspaces::workspace_save_chat,
            workspaces::workspace_delete_chat,
            commands::cloud_drive_save,
            commands::cloud_connection_files,
            commands::cloud_agents_call,
            commands::cloud_search_browse,
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
            commands::remove_root,
            commands::clear_local_data,
            commands::install_sample,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Private AI Connector Workspace");
}

pub fn run_parser_sidecar() -> i32 {
    parser_ipc::run_stdio()
}
