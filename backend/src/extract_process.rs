//! Runs file extraction in a separate, restricted process.
//!
//! A hostile PDF or Office file can make a parser crash, hang or eat memory, and a flaw in a
//! parser can be exploited. So the reading happens in its own short-lived process, `workspace-extract`:
//!   - it starts with an empty environment (no tokens, no database address) and an empty working folder,
//!   - it lowers its own limits on CPU time, file writes, open files and (on Linux) memory,
//!   - the parent gives up on it after a time limit and caps how much it will read back,
//!   - it receives the file on stdin and answers on stdout, nothing else.
//!
//! What this does not do: it does not cut the process off from the network or from the files
//! its user can read. That has to be enforced where it is deployed (a container with no network
//! and a read-only file system, ideally under a different user). See backend/README.md.

use std::{
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::extract::{ExtractError, Extracted, Limits, extract};

/// Marks the start of the real answer, so stray output from a library cannot be mistaken for it.
const MARKER: &[u8] = b"\n@@workspace-extract-result@@\n";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Extracted text that is larger than this is refused. Far above any real document.
const DEFAULT_MAX_OUTPUT: usize = 1024 * 1024 * 1024;
/// Every two megabytes of file add a second to the time allowed, so a very large document is not
/// failed for being large, while a file that hangs is still stopped.
const BYTES_PER_EXTRA_SECOND: u64 = 2 * 1024 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Serialize, Deserialize)]
struct Header {
    name: String,
    mime: String,
    /// Processor time the program may use. It follows the time the worker allows, so a very large
    /// file is not stopped early. Missing means the default.
    #[serde(default)]
    cpu_secs: Option<u64>,
}

pub type Outcome = Result<Extracted, ExtractError>;

#[async_trait]
pub trait Extractor: Send + Sync {
    async fn extract(&self, bytes: Vec<u8>, name: &str, mime: &str) -> Outcome;
}

/// Reads the file inside this process, with no isolation at all. For tests only.
pub struct UnsandboxedExtractor;

#[async_trait]
impl Extractor for UnsandboxedExtractor {
    async fn extract(&self, bytes: Vec<u8>, name: &str, mime: &str) -> Outcome {
        let (name, mime) = (name.to_string(), mime.to_string());
        tokio::task::spawn_blocking(move || extract(&bytes, &name, &mime))
            .await
            .map_err(|_| ExtractError::Crashed("the reader panicked".into()))?
    }
}

pub struct SubprocessExtractor {
    exe: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    max_output: usize,
}

impl SubprocessExtractor {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        Self {
            exe: exe.into(),
            args: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            max_output: DEFAULT_MAX_OUTPUT,
        }
    }

    /// The `workspace-extract` program: EXTRACTOR_PATH if set, otherwise next to this program.
    pub fn locate() -> Result<Self, ExtractError> {
        if let Ok(path) = std::env::var("EXTRACTOR_PATH") {
            return Ok(Self::new(path));
        }
        let me = std::env::current_exe()
            .map_err(|e| ExtractError::Crashed(format!("cannot locate the reader: {e}")))?;
        let name = if cfg!(windows) {
            "workspace-extract.exe"
        } else {
            "workspace-extract"
        };
        let path = me.with_file_name(name);
        if path.exists() {
            Ok(Self::new(path))
        } else {
            Err(ExtractError::Crashed(format!(
                "the reader program is missing: {}",
                path.display()
            )))
        }
    }

    pub fn with_limits(mut self, timeout: Duration, max_output: usize) -> Self {
        self.timeout = timeout;
        self.max_output = max_output;
        self
    }

    #[doc(hidden)]
    pub fn with_args(mut self, args: &[&str]) -> Self {
        self.args = args.iter().map(|a| a.to_string()).collect();
        self
    }

    /// Runs the program with `input` on stdin and returns what it wrote to stdout.
    #[doc(hidden)]
    pub async fn run_raw(&self, input: Vec<u8>) -> Result<Vec<u8>, ExtractError> {
        self.run_raw_for(input, self.timeout).await
    }

    async fn run_raw_for(
        &self,
        input: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<u8>, ExtractError> {
        let crashed = |why: String| ExtractError::Crashed(why);
        let workdir =
            tempfile::tempdir().map_err(|e| crashed(format!("no working folder: {e}")))?;
        let mut child = tokio::process::Command::new(&self.exe)
            .args(&self.args)
            .env_clear()
            .current_dir(workdir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| crashed(format!("could not start the reader: {e}")))?;
        let mut stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let cap = self.max_output;

        let io = async {
            // The program may exit before reading everything; that is its answer, not an error here.
            // Dropping `stdin` at the end is what closes the pipe and tells the reader the file is
            // complete; `shutdown` alone does not.
            let write = async move {
                let _ = stdin.write_all(&input).await;
                drop(stdin);
            };
            let read = async {
                let mut buffer = Vec::new();
                stdout
                    .take(cap as u64 + 1)
                    .read_to_end(&mut buffer)
                    .await
                    .map(|_| buffer)
            };
            tokio::join!(write, read).1
        };
        let output = match tokio::time::timeout(timeout, io).await {
            Err(_) => {
                let _ = child.kill().await;
                return Err(ExtractError::TimedOut);
            }
            Ok(Err(e)) => return Err(crashed(format!("could not read the answer: {e}"))),
            Ok(Ok(buffer)) => buffer,
        };
        if output.len() > cap {
            let _ = child.kill().await;
            return Err(crashed("the reader produced too much output".into()));
        }
        let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => status,
            _ => {
                let _ = child.kill().await;
                return Err(crashed("the reader did not exit".into()));
            }
        };
        if !status.success() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return Err(crashed(format!(
                        "the reader was stopped by signal {signal}"
                    )));
                }
            }
            return Err(crashed(format!("the reader exited with {status}")));
        }
        Ok(output)
    }
}

#[async_trait]
impl Extractor for SubprocessExtractor {
    async fn extract(&self, bytes: Vec<u8>, name: &str, mime: &str) -> Outcome {
        let extra = Duration::from_secs(bytes.len() as u64 / BYTES_PER_EXTRA_SECOND);
        let timeout = (self.timeout + extra).min(MAX_TIMEOUT.max(self.timeout));
        let header = serde_json::to_vec(&Header {
            name: name.into(),
            mime: mime.into(),
            cpu_secs: Some(timeout.as_secs() + 10),
        })
        .map_err(|e| ExtractError::Crashed(e.to_string()))?;
        let mut input = header;
        input.push(b'\n');
        input.extend_from_slice(&bytes);
        parse_answer(&self.run_raw_for(input, timeout).await?)
    }
}

fn parse_answer(output: &[u8]) -> Outcome {
    let start = output
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
        .ok_or_else(|| ExtractError::Crashed("the reader gave no answer".into()))?
        + MARKER.len();
    let rest = &output[start..];
    let length = rest
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or_else(|| ExtractError::Crashed("the reader's answer was cut short".into()))?;
    let body = rest
        .get(4..4 + length)
        .ok_or_else(|| ExtractError::Crashed("the reader's answer was cut short".into()))?;
    serde_json::from_slice::<Outcome>(body)
        .map_err(|_| ExtractError::Crashed("the reader's answer was garbled".into()))?
}

// ---- The restricted program ----

/// Lowers this process's own limits. Best effort: a limit that cannot be set is skipped.
#[cfg(unix)]
fn apply_limits() {
    fn lower(resource: impl TryInto<libc::c_int>, soft: u64, hard: u64) {
        let Ok(resource) = resource.try_into() else {
            return;
        };
        let limit = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: setrlimit reads the struct we pass and touches nothing else.
        unsafe { libc::setrlimit(resource as _, &limit) };
    }
    // Until the header says how long this file may take, allow the most; it is tightened then.
    lower(libc::RLIMIT_CPU, MAX_CPU_SECS, MAX_CPU_SECS + 10);
    lower(libc::RLIMIT_FSIZE, 0, 0); // no writing to files
    lower(libc::RLIMIT_CORE, 0, 0); // no crash dumps, which could contain file text
    lower(libc::RLIMIT_NOFILE, 64, 64);
    #[cfg(target_os = "linux")]
    // Room for a very large file and the copies the readers make of it.
    lower(libc::RLIMIT_AS, 6 << 30, 6 << 30);
}

#[cfg(not(unix))]
fn apply_limits() {}

/// The most processor time the program is ever given, in seconds.
const MAX_CPU_SECS: u64 = 3_600;

/// Tightens the processor-time limit to what this file is allowed. Lowering never needs privilege.
#[cfg(unix)]
fn limit_cpu(secs: u64) {
    let secs = secs.clamp(10, MAX_CPU_SECS);
    let limit = libc::rlimit {
        rlim_cur: secs as libc::rlim_t,
        rlim_max: (secs + 10) as libc::rlim_t,
    };
    // SAFETY: setrlimit reads the struct we pass and touches nothing else.
    unsafe { libc::setrlimit(libc::RLIMIT_CPU as _, &limit) };
}

#[cfg(not(unix))]
fn limit_cpu(_: u64) {}

/// The whole `workspace-extract` program: read one file from stdin, write one answer to stdout.
pub fn run_child() -> ! {
    apply_limits();
    let limits = Limits::default();
    let cap = limits.max_input_bytes + 8192;
    let mut input = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(cap as u64 + 1)
        .read_to_end(&mut input);
    let outcome: Outcome = match read {
        Err(_) => Err(ExtractError::Unreadable("could not read the input".into())),
        Ok(_) if input.len() > cap => Err(ExtractError::TooLarge),
        Ok(_) => match input.iter().position(|&b| b == b'\n') {
            None => Err(ExtractError::Unreadable("no header".into())),
            Some(end) => match serde_json::from_slice::<Header>(&input[..end]) {
                Err(_) => Err(ExtractError::Unreadable("bad header".into())),
                Ok(header) => {
                    limit_cpu(header.cpu_secs.unwrap_or(60));
                    let bytes = &input[end + 1..];
                    std::panic::catch_unwind(|| extract(bytes, &header.name, &header.mime))
                        .unwrap_or_else(|_| {
                            Err(ExtractError::Unreadable("could not read this file".into()))
                        })
                }
            },
        },
    };
    let body = serde_json::to_vec(&outcome).expect("answers can be written");
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(MARKER);
    let _ = out.write_all(&(body.len() as u32).to_be_bytes());
    let _ = out.write_all(&body);
    let _ = out.flush();
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut out = MARKER.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn the_answer_is_found_after_any_noise_and_cut_off_answers_are_refused() {
        let good = serde_json::to_vec(&Outcome::Ok(Extracted::default())).unwrap();
        let mut noisy = b"library chatter\n".to_vec();
        noisy.extend(frame(&good));
        assert_eq!(parse_answer(&noisy), Ok(Extracted::default()));

        // Chatter printed after the answer, or a forged earlier answer, does not win.
        let mut forged =
            frame(&serde_json::to_vec(&Outcome::Err(ExtractError::Unsupported)).unwrap());
        forged.extend(frame(&good));
        assert_eq!(parse_answer(&forged), Ok(Extracted::default()));

        assert!(matches!(
            parse_answer(b"nothing here"),
            Err(ExtractError::Crashed(_))
        ));
        let mut short = frame(&good);
        short.truncate(short.len() - 3);
        assert!(matches!(
            parse_answer(&short),
            Err(ExtractError::Crashed(_))
        ));
        assert!(matches!(
            parse_answer(&frame(b"not json")),
            Err(ExtractError::Crashed(_))
        ));
    }
}
