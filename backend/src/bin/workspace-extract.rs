//! Reads one file from stdin and writes the text found in it to stdout. Run by the sync worker
//! as a restricted child process; see `workspace_cloud::extract_process`.

fn main() {
    workspace_cloud::extract_process::run_child();
}
