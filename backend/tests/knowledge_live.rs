//! The real model reading a made-up document. Costs a fraction of a cent, so it only runs when
//! asked: ANTHROPIC_API_KEY=... cargo test --test knowledge_live -- --ignored --nocapture

use workspace_cloud::knowledge::{Analyzer, ClaudeAnalyzer, ProfileInput, clean};

const LETTER: &str = "ENGAGEMENT LETTER\nMaple Roofing LLC will replace the roof at 14 Elm Street for Dana Whitfield. The price is $18,400, half due on signing. Late invoices accrue interest at 1.5% a month after 30 days. Crew lead Marcus Reyes manages the job. Materials are supplied by Summit Supply Co. Work begins April 7 and takes about two weeks. Change orders must be approved in writing by the customer.";

#[tokio::test]
#[ignore = "calls the Claude API"]
async fn the_real_model_reads_a_document_and_every_quote_is_real() {
    let analyzer = ClaudeAnalyzer::from_env().expect("set ANTHROPIC_API_KEY and ALLOW_INSECURE=1");
    let raw = analyzer
        .analyze("Engagement letter - Whitfield.docx", LETTER)
        .await
        .unwrap();
    let cleaned = clean(LETTER, raw.clone());
    eprintln!("{cleaned:#?}");
    assert_eq!(cleaned.doc_type, "contract");
    let names: Vec<&str> = cleaned.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"Maple Roofing LLC") && names.contains(&"Dana Whitfield"),
        "{names:?}"
    );
    assert!(
        cleaned.facts.iter().any(|f| f.statement.contains("18,400")),
        "the price is a fact"
    );
    assert!(
        cleaned
            .facts
            .iter()
            .any(|f| f.statement.to_lowercase().contains("change order"))
    );
    // The cleaning step drops quotes that are not in the document; the model should rarely need it.
    let dropped = raw.entities.len() + raw.facts.len() + raw.relationships.len()
        - cleaned.entities.len()
        - cleaned.facts.len()
        - cleaned.relationships.len();
    eprintln!("dropped by cleaning: {dropped}");
    assert!(dropped <= 3, "the model invented too much: {dropped}");
}

#[tokio::test]
#[ignore = "calls the Claude API"]
async fn the_real_model_describes_a_business_from_notes() {
    let analyzer = ClaudeAnalyzer::from_env().expect("set ANTHROPIC_API_KEY and ALLOW_INSECURE=1");
    let input = ProfileInput {
        documents: vec![
            (
                "Engagement letter - Whitfield".into(),
                "contract".into(),
                "Roof replacement at 14 Elm Street for $18,400.".into(),
            ),
            (
                "Invoice 1042".into(),
                "invoice".into(),
                "Invoice for shingles and labour on a roof repair.".into(),
            ),
            (
                "Change order form".into(),
                "form".into(),
                "A CO changes the price or scope of a roofing job.".into(),
            ),
        ],
        entities: vec![
            ("Maple Roofing LLC".into(), "organization".into(), 3),
            ("Summit Supply Co.".into(), "organization".into(), 2),
        ],
        facts: vec!["Change orders must be approved in writing by the customer.".into()],
    };
    let draft = analyzer.profile(&input).await.unwrap();
    eprintln!("{draft:#?}");
    assert!(draft.summary.to_lowercase().contains("roof"));
    assert!(!draft.services.is_empty());
}
