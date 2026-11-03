//! Reading text out of scanned pages and pictures, so a scanned contract or a photographed letter
//! can be searched by what it says. The work is done by Tesseract (and Poppler, which turns PDF
//! pages into pictures), run as separate programs with the same kind of limits as the file reader:
//! an empty environment, an empty working folder, and caps on time, memory and file size. Nothing
//! is sent to any other service.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;

use crate::{
    extract::{ExtractError, Extracted, Kind, Section, kind_for},
    extract_process::{Extractor, Outcome},
};

/// Pages are drawn at most this many pixels on the long side: sharp enough for small print on an
/// A4 page (about 250 dots per inch), and a poster-sized page cannot use gigabytes of memory.
const PAGE_PIXELS: u32 = 3000;
/// Most pages of one PDF that are read. Reading is slow, so a very long scan is read in part.
const MAX_PAGES: usize = 500;
/// Time allowed for one page or picture.
const STEP_TIMEOUT: Duration = Duration::from_secs(90);
/// Most text taken from one page.
const MAX_PAGE_TEXT: usize = 1024 * 1024;
/// Pictures smaller than this are icons and logos, not documents.
const MIN_IMAGE_BYTES: usize = 5 * 1024;
/// Fewer readable characters than this means the page had no text worth keeping.
const MIN_TEXT_CHARS: usize = 3;
/// Where programs are looked for when they are not on the search path.
const COMMON_FOLDERS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// Something that can read the text in scanned pages and pictures.
#[async_trait]
pub trait Ocr: Send + Sync {
    /// A PDF whose pages are pictures. Pages with no readable text are left out.
    async fn read_pdf(&self, bytes: &[u8]) -> Result<Vec<Section>, ExtractError>;
    /// A picture file. `extension` says which kind (png, jpg, ...).
    async fn read_image(&self, bytes: &[u8], extension: &str)
    -> Result<Vec<Section>, ExtractError>;
}

/// OCR with Tesseract and Poppler.
pub struct TesseractOcr {
    tesseract: PathBuf,
    /// `pdftoppm` and `pdfinfo`, which are needed for PDFs but not for pictures.
    pdf_tools: Option<(PathBuf, PathBuf)>,
    languages: String,
}

impl TesseractOcr {
    /// Looks for the programs. `OCR_TESSERACT`, `OCR_PDFTOPPM` and `OCR_PDFINFO` name them
    /// explicitly, `OCR_LANGUAGES` picks the languages (default English), and `OCR_DISABLED=1`
    /// turns reading scans off. None when Tesseract is not installed.
    pub fn detect() -> Option<Self> {
        if std::env::var("OCR_DISABLED").is_ok_and(|value| value == "1") {
            return None;
        }
        let tesseract = find_program("OCR_TESSERACT", "tesseract")?;
        let pdf_tools =
            find_program("OCR_PDFTOPPM", "pdftoppm").zip(find_program("OCR_PDFINFO", "pdfinfo"));
        let languages = std::env::var("OCR_LANGUAGES")
            .ok()
            .filter(|value| {
                !value.is_empty()
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_')
            })
            .unwrap_or_else(|| "eng".into());
        Some(Self {
            tesseract,
            pdf_tools,
            languages,
        })
    }

    pub fn reads_pdfs(&self) -> bool {
        self.pdf_tools.is_some()
    }

    /// Reads one picture file that is already in `dir`.
    async fn recognise(&self, dir: &Path, file: &str) -> Result<String, ExtractError> {
        let args = [
            file.to_string(),
            "stdout".into(),
            "-l".into(),
            self.languages.clone(),
            "--psm".into(),
            "3".into(),
        ];
        let text = run_limited(&self.tesseract, &args, dir, STEP_TIMEOUT, MAX_PAGE_TEXT).await?;
        Ok(String::from_utf8_lossy(&text).replace('\0', ""))
    }
}

#[async_trait]
impl Ocr for TesseractOcr {
    async fn read_image(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<Vec<Section>, ExtractError> {
        let extension = match extension.to_ascii_lowercase().as_str() {
            "jpeg" => "jpg".to_string(),
            "png" | "jpg" | "tif" | "tiff" | "bmp" | "gif" | "webp" => {
                extension.to_ascii_lowercase()
            }
            _ => return Err(ExtractError::Unsupported),
        };
        let dir = tempfile::tempdir().map_err(|e| crashed(format!("no working folder: {e}")))?;
        let file = format!("picture.{extension}");
        std::fs::write(dir.path().join(&file), bytes)
            .map_err(|e| crashed(format!("could not write the picture: {e}")))?;
        let text = self.recognise(dir.path(), &file).await?;
        Ok(keep_if_readable(Some("Scanned image".into()), text)
            .into_iter()
            .collect())
    }

    async fn read_pdf(&self, bytes: &[u8]) -> Result<Vec<Section>, ExtractError> {
        let Some((pdftoppm, pdfinfo)) = &self.pdf_tools else {
            return Err(ExtractError::Unsupported);
        };
        let dir = tempfile::tempdir().map_err(|e| crashed(format!("no working folder: {e}")))?;
        std::fs::write(dir.path().join("in.pdf"), bytes)
            .map_err(|e| crashed(format!("could not write the PDF: {e}")))?;

        let info = run_limited(
            pdfinfo,
            &["in.pdf".to_string()],
            dir.path(),
            STEP_TIMEOUT,
            64 * 1024,
        )
        .await
        .map_err(|error| match error {
            ExtractError::TimedOut => error,
            _ => ExtractError::Unreadable("could not open this PDF".into()),
        })?;
        let pages = page_count(&String::from_utf8_lossy(&info))
            .ok_or_else(|| ExtractError::Unreadable("could not count the pages".into()))?;

        let mut sections = Vec::new();
        let mut timed_out = false;
        for page in 1..=pages.min(MAX_PAGES) {
            let args = [
                "-f".to_string(),
                page.to_string(),
                "-l".into(),
                page.to_string(),
                "-png".into(),
                "-scale-to".into(),
                PAGE_PIXELS.to_string(),
                "in.pdf".into(),
                "page".into(),
            ];
            match run_limited(pdftoppm, &args, dir.path(), STEP_TIMEOUT, 64 * 1024).await {
                Ok(_) => {}
                Err(ExtractError::TimedOut) => {
                    timed_out = true;
                    continue;
                }
                // A page that cannot be drawn is skipped; the others may be fine.
                Err(_) => continue,
            }
            let Some(picture) = first_picture(dir.path()) else {
                continue;
            };
            let text = match self.recognise(dir.path(), &picture).await {
                Ok(text) => text,
                Err(ExtractError::TimedOut) => {
                    timed_out = true;
                    String::new()
                }
                Err(_) => String::new(),
            };
            let _ = std::fs::remove_file(dir.path().join(&picture));
            sections.extend(keep_if_readable(
                Some(format!("Page {page} (scanned)")),
                text,
            ));
        }
        if sections.is_empty() && timed_out {
            return Err(ExtractError::TimedOut);
        }
        Ok(sections)
    }
}

fn crashed(why: String) -> ExtractError {
    ExtractError::Crashed(why)
}

/// A section for the text, unless the page held nothing worth keeping.
fn keep_if_readable(label: Option<String>, text: String) -> Option<Section> {
    let readable = text.chars().filter(|c| !c.is_whitespace()).count();
    (readable >= MIN_TEXT_CHARS).then(|| Section {
        label,
        text: text.trim().to_string(),
    })
}

/// The page count from `pdfinfo`'s report ("Pages:          12").
fn page_count(report: &str) -> Option<usize> {
    report
        .lines()
        .find_map(|line| line.strip_prefix("Pages:"))
        .and_then(|rest| rest.trim().parse().ok())
        .filter(|pages| *pages > 0)
}

/// The picture `pdftoppm` just drew (`page-1.png`, `page-01.png`, ...).
fn first_picture(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("page-") && name.ends_with(".png"))
}

fn find_program(variable: &str, name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(variable) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain(COMMON_FOLDERS.iter().map(PathBuf::from))
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

/// Runs one program in an empty folder with an empty environment and hard limits, and returns what
/// it printed. The programs read files that come from other people's accounts, so a damaged or
/// hostile file may crash or hang them; that must cost no more than this one step.
async fn run_limited(
    program: &Path,
    args: &[String],
    dir: &Path,
    timeout: Duration,
    max_output: usize,
) -> Result<Vec<u8>, ExtractError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env_clear()
        // One thread each: a page read should not take over the machine.
        .env("OMP_THREAD_LIMIT", "1")
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Ok(prefix) = std::env::var("TESSDATA_PREFIX") {
        command.env("TESSDATA_PREFIX", prefix);
    }
    #[cfg(unix)]
    {
        let cpu = timeout.as_secs() + 10;
        // SAFETY: the closure only calls setrlimit, which is safe to call between fork and exec.
        unsafe {
            command.pre_exec(move || {
                limit(libc::RLIMIT_CPU as _, cpu);
                limit(libc::RLIMIT_CORE as _, 0);
                limit(libc::RLIMIT_NOFILE as _, 256);
                limit(libc::RLIMIT_FSIZE as _, 1 << 30);
                #[cfg(target_os = "linux")]
                limit(libc::RLIMIT_AS as _, 4 << 30);
                Ok(())
            });
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| crashed(format!("could not start {}: {e}", program.display())))?;
    let mut stdout = child.stdout.take().expect("piped");
    let run = async {
        let mut buffer = Vec::new();
        let read = (&mut stdout)
            .take(max_output as u64 + 1)
            .read_to_end(&mut buffer)
            .await;
        let status = child.wait().await;
        (read, status, buffer)
    };
    match tokio::time::timeout(timeout, run).await {
        Err(_) => Err(ExtractError::TimedOut),
        Ok((Err(e), _, _)) => Err(crashed(format!("could not read the answer: {e}"))),
        Ok((_, Err(e), _)) => Err(crashed(format!("the program did not finish: {e}"))),
        Ok((_, Ok(status), mut buffer)) => {
            if !status.success() {
                return Err(crashed(format!(
                    "{} exited with {status}",
                    program.display()
                )));
            }
            // Text beyond the cap is dropped rather than refused: the rest of the page is still useful.
            buffer.truncate(max_output);
            Ok(buffer)
        }
    }
}

#[cfg(unix)]
fn limit(resource: libc::c_int, value: u64) {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    // SAFETY: setrlimit reads the struct we pass and touches nothing else.
    unsafe { libc::setrlimit(resource as _, &limit) };
}

/// Wraps the file reader so that scanned PDFs and pictures are read with OCR. A PDF that has a
/// text layer is never sent to OCR, and without an OCR engine this behaves exactly like the reader
/// it wraps.
pub struct OcrExtractor<E> {
    inner: E,
    ocr: Option<Arc<dyn Ocr>>,
}

impl<E> OcrExtractor<E> {
    pub fn new(inner: E, ocr: Option<Arc<dyn Ocr>>) -> Self {
        Self { inner, ocr }
    }
}

#[async_trait]
impl<E: Extractor> Extractor for OcrExtractor<E> {
    async fn extract(&self, bytes: Vec<u8>, name: &str, mime: &str) -> Outcome {
        let kind = kind_for(name, mime);
        let Some(ocr) = self
            .ocr
            .as_ref()
            .filter(|_| matches!(kind, Some(Kind::Pdf | Kind::Image)))
        else {
            return self.inner.extract(bytes, name, mime).await;
        };
        if kind == Some(Kind::Image) {
            if bytes.len() < MIN_IMAGE_BYTES {
                return Ok(Extracted::default());
            }
            let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
            // Reported types with no extension ("image/png") still name the format.
            let extension = if extension.is_empty() {
                mime.strip_prefix("image/").unwrap_or("png")
            } else {
                extension
            };
            return ocr
                .read_image(&bytes, extension)
                .await
                .map(|sections| Extracted {
                    sections,
                    truncated: false,
                });
        }
        // A PDF: the normal reader first, because a text layer is exact and OCR is not.
        let first = self.inner.extract(bytes.clone(), name, mime).await?;
        if !first.sections.is_empty() {
            return Ok(first);
        }
        ocr.read_pdf(&bytes).await.map(|sections| Extracted {
            sections,
            truncated: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_count_comes_from_the_report() {
        assert_eq!(
            page_count("Title: x\nPages:           12\nEncrypted: no"),
            Some(12)
        );
        assert_eq!(page_count("Pages: 0"), None);
        assert_eq!(page_count("nothing useful"), None);
    }

    #[test]
    fn pages_with_almost_no_text_are_dropped() {
        assert!(keep_if_readable(None, "  \n ".into()).is_none());
        assert!(keep_if_readable(None, "a".into()).is_none());
        let kept = keep_if_readable(
            Some("Page 2 (scanned)".into()),
            "  The retainer is due.  \n".into(),
        )
        .unwrap();
        assert_eq!(kept.text, "The retainer is due.");
        assert_eq!(kept.label.as_deref(), Some("Page 2 (scanned)"));
    }
}
