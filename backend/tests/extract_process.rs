//! The restricted reader program, run for real. Run with scripts/test.sh (or cargo test).

use std::time::{Duration, Instant};

use workspace_cloud::{
    extract::{ExtractError, testdata},
    extract_process::{Extractor, SubprocessExtractor},
};

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

fn real() -> SubprocessExtractor {
    SubprocessExtractor::new(env!("CARGO_BIN_EXE_workspace-extract"))
}

#[tokio::test]
async fn the_restricted_program_reads_each_kind_of_file() {
    let reader = real();
    let word = reader
        .extract(
            testdata::docx(&[&["The retainer ", "is due"]]),
            "Letter.docx",
            DOCX,
        )
        .await
        .unwrap();
    assert_eq!(word.sections[0].text, "The retainer is due");

    let pdf = reader
        .extract(
            testdata::pdf(&["Interest accrues monthly."]),
            "Terms.pdf",
            "application/pdf",
        )
        .await
        .unwrap();
    assert_eq!(pdf.sections[0].label.as_deref(), Some("Page 1"));
    assert!(pdf.sections[0].text.contains("Interest accrues monthly"));

    let sheet = reader
        .extract(
            testdata::xlsx("Fees", &[&["Acme", "5000"]]),
            "Fees.xlsx",
            "",
        )
        .await
        .unwrap();
    assert!(sheet.sections[0].text.contains("Acme | 5000"));

    let deck = reader
        .extract(testdata::pptx(&[(1, "Kickoff agenda")]), "Deck.pptx", "")
        .await
        .unwrap();
    assert_eq!(deck.sections[0].label.as_deref(), Some("Slide 1"));
}

#[tokio::test]
async fn damaged_or_unsupported_files_get_an_error_and_the_next_file_still_works() {
    let reader = real();
    assert!(matches!(
        reader
            .extract(b"%PDF-1.7 garbage garbage".to_vec(), "a.pdf", "")
            .await,
        Err(ExtractError::Unreadable(_))
    ));
    assert!(matches!(
        reader
            .extract(b"PK\x03\x04 broken".to_vec(), "a.docx", "")
            .await,
        Err(ExtractError::Unreadable(_))
    ));
    assert_eq!(
        reader
            .extract(b"x".to_vec(), "Old.doc", "application/msword")
            .await,
        Err(ExtractError::Unsupported)
    );
    // An expansion bomb is refused by the program, which stays well within its limits.
    let bomb = testdata::zip_bomb("word/document.xml", 200 * 1024 * 1024);
    assert!(bomb.len() < 1024 * 1024);
    assert!(matches!(
        reader.extract(bomb, "bomb.docx", "").await,
        Err(ExtractError::Unreadable(_))
    ));

    let ok = reader
        .extract(testdata::docx(&[&["still fine"]]), "ok.docx", "")
        .await
        .unwrap();
    assert_eq!(ok.sections[0].text, "still fine");
}

#[tokio::test]
async fn a_reader_that_never_finishes_is_stopped_and_really_killed() {
    let reader = SubprocessExtractor::new("/bin/sh")
        .with_args(&["-c", "exec sleep 29.4217"])
        .with_limits(Duration::from_millis(500), 1024);
    let started = Instant::now();
    assert_eq!(
        reader.extract(b"x".to_vec(), "a.pdf", "").await,
        Err(ExtractError::TimedOut)
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still_running = std::process::Command::new("pgrep")
        .args(["-f", "sleep 29.4217"])
        .output()
        .unwrap();
    assert!(
        still_running.stdout.is_empty(),
        "the stopped reader must not keep running"
    );
}

#[tokio::test]
async fn a_reader_that_floods_its_output_is_cut_off() {
    let reader =
        SubprocessExtractor::new("/usr/bin/yes").with_limits(Duration::from_secs(10), 1_000_000);
    let started = Instant::now();
    let result = reader.extract(b"x".to_vec(), "a.pdf", "").await;
    assert!(
        matches!(&result, Err(ExtractError::Crashed(why)) if why.contains("too much output")),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn a_reader_that_crashes_or_cannot_start_is_reported_as_a_transient_failure() {
    let crash = SubprocessExtractor::new("/bin/sh").with_args(&["-c", "kill -SEGV $$"]);
    let result = crash.extract(b"x".to_vec(), "a.pdf", "").await;
    assert!(
        matches!(&result, Err(ExtractError::Crashed(why)) if why.contains("signal")),
        "{result:?}"
    );
    assert!(result.unwrap_err().is_transient());

    let missing = SubprocessExtractor::new("/no/such/program");
    assert!(matches!(
        missing.extract(b"x".to_vec(), "a.pdf", "").await,
        Err(ExtractError::Crashed(_))
    ));

    let silent = SubprocessExtractor::new("/usr/bin/true");
    assert!(matches!(
        silent.extract(b"x".to_vec(), "a.pdf", "").await,
        Err(ExtractError::Crashed(_))
    ));
}

#[tokio::test]
async fn the_reader_starts_with_an_empty_environment_and_an_empty_folder() {
    // SAFETY: this test is the only one that touches this variable.
    unsafe { std::env::set_var("EXTRACT_TEST_SECRET", "hunter2") };
    let env = SubprocessExtractor::new("/usr/bin/env");
    let printed = env.run_raw(Vec::new()).await.unwrap();
    assert!(
        printed.is_empty(),
        "the reader saw: {}",
        String::from_utf8_lossy(&printed)
    );

    let list = SubprocessExtractor::new("/bin/ls").with_args(&["-A"]);
    assert!(
        list.run_raw(Vec::new()).await.unwrap().is_empty(),
        "the working folder must start empty"
    );
}
