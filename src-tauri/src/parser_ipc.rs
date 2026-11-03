use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    models::{ParseRequest, ParseResult},
    parser::parse_request,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ParserResponse {
    pub ok: bool,
    pub result: Option<ParseResult>,
    pub error: Option<String>,
}

pub fn parse_isolated(request: ParseRequest, cancelled: &AtomicBool) -> AppResult<ParseResult> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(AppError::Cancelled);
    }
    let executable = std::env::current_exe()
        .map_err(|error| AppError::Parser(format!("cannot locate parser executable: {error}")))?;
    let payload = serde_json::to_vec(&request)
        .map_err(|error| AppError::Parser(format!("cannot encode parser request: {error}")))?;
    let mut command = Command::new(executable);
    command.arg("--parser-sidecar");
    let output = run_child(&mut command, payload, cancelled)?;
    if output.len() < 4 {
        return Err(AppError::Parser(
            "parser returned an incomplete frame".into(),
        ));
    }
    let response_length = u32::from_be_bytes(
        output[..4]
            .try_into()
            .map_err(|_| AppError::Parser("parser returned an invalid frame".into()))?,
    ) as usize;
    if output.len() != response_length + 4 {
        return Err(AppError::Parser("parser returned a malformed frame".into()));
    }
    let response: ParserResponse = serde_json::from_slice(&output[4..])
        .map_err(|error| AppError::Parser(format!("invalid parser response: {error}")))?;
    if response.ok {
        response
            .result
            .ok_or_else(|| AppError::Parser("parser returned no result".into()))
    } else {
        Err(AppError::Parser(
            response.error.unwrap_or_else(|| "parser failed".into()),
        ))
    }
}

fn run_child(
    command: &mut Command,
    payload: Vec<u8>,
    cancelled: &AtomicBool,
) -> AppResult<Vec<u8>> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(AppError::Cancelled);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| AppError::Parser(format!("cannot start parser process: {error}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Parser("parser stdin is unavailable".into()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Parser("parser stdout is unavailable".into()))?;
    let writer = thread::spawn(move || {
        stdin
            .write_all(&(payload.len() as u32).to_be_bytes())
            .and_then(|_| stdin.write_all(&payload))
    });
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).map(|_| output)
    });
    let status = loop {
        if cancelled.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            break Err(AppError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(AppError::Parser(format!("parser process failed: {error}")));
            }
        }
    };
    let write_result = writer.join();
    let read_result = reader.join();
    let status = status?;
    if !status.success() {
        return Err(AppError::Parser(format!(
            "parser exited with status {status}"
        )));
    }
    write_result
        .map_err(|_| AppError::Parser("parser writer failed".into()))?
        .map_err(|error| AppError::Parser(format!("cannot send parser request: {error}")))?;
    read_result
        .map_err(|_| AppError::Parser("parser reader failed".into()))?
        .map_err(|error| AppError::Parser(format!("cannot read parser response: {error}")))
}

pub fn run_stdio() -> i32 {
    const MAX_MESSAGE_BYTES: usize = 72 * 1024 * 1024;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::BufWriter::new(std::io::stdout());
    loop {
        let mut length_bytes = [0_u8; 4];
        match stdin.read_exact(&mut length_bytes) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(_) => return 1,
        }
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > MAX_MESSAGE_BYTES {
            return 1;
        }
        let mut payload = vec![0_u8; length];
        if stdin.read_exact(&mut payload).is_err() {
            return 1;
        }
        let response = match serde_json::from_slice::<ParseRequest>(&payload) {
            Ok(request) => match parse_request(request) {
                Ok(result) => ParserResponse {
                    ok: true,
                    result: Some(result),
                    error: None,
                },
                Err(error) => ParserResponse {
                    ok: false,
                    result: None,
                    error: Some(error.to_string()),
                },
            },
            Err(error) => ParserResponse {
                ok: false,
                result: None,
                error: Some(format!("invalid request: {error}")),
            },
        };
        let payload = match serde_json::to_vec(&response) {
            Ok(payload) => payload,
            Err(_) => return 1,
        };
        if stdout
            .write_all(&(payload.len() as u32).to_be_bytes())
            .and_then(|_| stdout.write_all(&payload))
            .and_then(|_| stdout.flush())
            .is_err()
        {
            return 1;
        }
    }
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Instant};

    #[test]
    fn cancellation_terminates_a_stalled_parser() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancelled);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            signal.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = run_child(
            Command::new("/bin/sleep").arg("30"),
            vec![0; 1024 * 1024],
            &cancelled,
        );
        trigger.join().unwrap();
        assert!(matches!(result, Err(AppError::Cancelled)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
