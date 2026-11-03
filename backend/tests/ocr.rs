//! OCR with the real Tesseract and Poppler. These tests skip themselves on a machine that does not
//! have them, so they never fail for want of a tool; CI installs both.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use workspace_cloud::{
    extract::{ExtractError, Extracted, Section, testdata},
    extract_process::{Extractor, UnsandboxedExtractor},
    ocr::{Ocr, OcrExtractor, TesseractOcr},
};

const SCANNED_PDF: &[u8] = include_bytes!("fixtures/scan_two_pages.pdf");
const NOTICE_PNG: &[u8] = include_bytes!("fixtures/scan_notice.png");
const BLANK_PNG: &[u8] = include_bytes!("fixtures/blank.png");

fn engine() -> Option<TesseractOcr> {
    let engine = TesseractOcr::detect();
    if engine.as_ref().is_none_or(|engine| !engine.reads_pdfs()) {
        eprintln!("skipped: Tesseract and Poppler are not installed");
        return None;
    }
    engine
}

fn all_text(sections: &[Section]) -> String {
    sections
        .iter()
        .map(|section| section.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::test]
async fn a_scanned_pdf_is_read_page_by_page_with_page_labels() {
    let Some(ocr) = engine() else { return };
    let sections = ocr.read_pdf(SCANNED_PDF).await.unwrap();
    assert_eq!(sections.len(), 2, "{sections:?}");
    assert_eq!(sections[0].label.as_deref(), Some("Page 1 (scanned)"));
    assert!(
        sections[0]
            .text
            .to_lowercase()
            .contains("retainer is due within thirty days")
    );
    assert_eq!(sections[1].label.as_deref(), Some("Page 2 (scanned)"));
    assert!(sections[1].text.to_lowercase().contains("hourly rate"));
}

#[tokio::test]
async fn a_picture_of_a_notice_is_read() {
    let Some(ocr) = engine() else { return };
    let sections = ocr.read_image(NOTICE_PNG, "png").await.unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].label.as_deref(), Some("Scanned image"));
    assert!(all_text(&sections).contains("partners will meet on tuesday"));
}

#[tokio::test]
async fn a_picture_with_no_text_gives_nothing_and_damaged_files_do_not_crash_the_reader() {
    let Some(ocr) = engine() else { return };
    assert!(ocr.read_image(BLANK_PNG, "png").await.unwrap().is_empty());
    // Not a picture at all, a truncated picture, and an unknown format: an error or nothing,
    // never a hang or a crash of this process.
    for (bytes, extension) in [
        (b"this is not an image".to_vec(), "png"),
        (NOTICE_PNG[..200].to_vec(), "png"),
    ] {
        let outcome = ocr.read_image(&bytes, extension).await;
        assert!(outcome.map_or(true, |sections| sections.is_empty()));
    }
    assert_eq!(
        ocr.read_image(NOTICE_PNG, "exe").await,
        Err(ExtractError::Unsupported)
    );
    // A PDF that is not one.
    let outcome = ocr.read_pdf(b"%PDF-1.4 garbage").await;
    assert!(
        matches!(outcome, Err(ExtractError::Unreadable(_))),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn a_pdf_with_a_text_layer_is_never_sent_to_ocr_but_a_scan_is() {
    let Some(real) = engine() else { return };
    let reader = OcrExtractor::new(UnsandboxedExtractor, Some(Arc::new(real) as Arc<dyn Ocr>));
    let text_pdf = reader
        .extract(
            testdata::pdf(&["Exact text from the text layer"]),
            "a.pdf",
            "",
        )
        .await
        .unwrap();
    assert_eq!(
        text_pdf.sections[0].label.as_deref(),
        Some("Page 1"),
        "a page label with no 'scanned'"
    );
    assert!(text_pdf.sections[0].text.contains("Exact text"));

    let scan = reader
        .extract(SCANNED_PDF.to_vec(), "scan.pdf", "application/pdf")
        .await
        .unwrap();
    assert!(all_text(&scan.sections).contains("senior partner"));
    let picture = reader
        .extract(NOTICE_PNG.to_vec(), "notice.PNG", "image/png")
        .await
        .unwrap();
    assert!(all_text(&picture.sections).contains("conference room"));
}

/// An engine that records what it was asked to read.
struct Spy(Mutex<Vec<String>>);

#[async_trait]
impl Ocr for Spy {
    async fn read_pdf(&self, _: &[u8]) -> Result<Vec<Section>, ExtractError> {
        self.0.lock().unwrap().push("pdf".into());
        Ok(vec![Section {
            label: Some("Page 1 (scanned)".into()),
            text: "from the scan".into(),
        }])
    }
    async fn read_image(&self, _: &[u8], extension: &str) -> Result<Vec<Section>, ExtractError> {
        self.0.lock().unwrap().push(format!("image:{extension}"));
        Err(ExtractError::TimedOut)
    }
}

#[tokio::test]
async fn ocr_is_used_only_for_scans_and_pictures_and_its_failures_are_passed_on() {
    let spy = Arc::new(Spy(Mutex::new(Vec::new())));
    let reader = OcrExtractor::new(UnsandboxedExtractor, Some(spy.clone() as Arc<dyn Ocr>));

    // A text PDF and a Word file never reach it.
    reader
        .extract(testdata::pdf(&["real text"]), "a.pdf", "")
        .await
        .unwrap();
    reader
        .extract(testdata::docx(&[&["real text"]]), "a.docx", "")
        .await
        .unwrap();
    assert!(spy.0.lock().unwrap().is_empty());

    // A PDF with no text layer does.
    let blank_pdf = testdata::pdf(&[""]);
    let scanned = reader.extract(blank_pdf, "scan.pdf", "").await.unwrap();
    assert_eq!(scanned.sections[0].text, "from the scan");

    // A picture does; a tiny one (an icon) does not; a timeout comes back as a timeout, which the
    // sync retries.
    let picture = vec![0u8; 8 * 1024];
    assert_eq!(
        reader.extract(picture, "scan.jpeg", "").await,
        Err(ExtractError::TimedOut)
    );
    assert_eq!(
        reader.extract(vec![0u8; 100], "icon.png", "").await,
        Ok(Extracted::default())
    );
    assert_eq!(spy.0.lock().unwrap().as_slice(), ["pdf", "image:jpeg"]);

    // With no engine it behaves exactly like the plain reader.
    let plain = OcrExtractor::new(UnsandboxedExtractor, None);
    assert_eq!(
        plain.extract(NOTICE_PNG.to_vec(), "notice.png", "").await,
        Ok(Extracted::default())
    );
}
