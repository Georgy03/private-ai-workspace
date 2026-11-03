fn main() {
    if std::env::args().any(|argument| argument == "--parser-sidecar") {
        std::process::exit(private_ai_connector_workspace_lib::run_parser_sidecar());
    }
    private_ai_connector_workspace_lib::run();
}
