//! The real meaning-search model. It is downloaded the first time (about 130 MB). On a machine
//! that cannot download it, set SKIP_MODEL_TESTS=1; otherwise failing to load it fails the test,
//! so a skipped check is never mistaken for a passing one.

use workspace_cloud::embed::{DIMENSIONS, Embedder, LocalEmbedder};

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn model() -> Option<LocalEmbedder> {
    match LocalEmbedder::load() {
        Ok(model) => Some(model),
        Err(error) if std::env::var("SKIP_MODEL_TESTS").is_ok_and(|v| v == "1") => {
            eprintln!("skipped: {error}");
            None
        }
        Err(error) => panic!("the model could not be loaded: {error}"),
    }
}

#[tokio::test]
async fn a_question_is_closest_to_the_passage_that_means_the_same_thing() {
    let Some(model) = model() else { return };
    let question = model
        .embed_query("what happens if the client pays late")
        .await
        .unwrap();
    assert_eq!(question.len(), DIMENSIONS);
    assert!(
        (dot(&question, &question) - 1.0).abs() < 1e-3,
        "vectors have length one"
    );

    let passages = vec![
        "Late fees of 1.5 percent a month start after thirty days from the invoice date."
            .to_string(),
        "Interest accrues on unpaid balances after thirty days.".to_string(),
        "The staff picnic is at the lake on Saturday, bring a folding chair.".to_string(),
        "The engagement covers preparation of the 2026 federal income tax return.".to_string(),
    ];
    let vectors = model.embed_passages(passages.clone()).await.unwrap();
    let similarity: Vec<f32> = vectors.iter().map(|v| dot(&question, v)).collect();
    for (text, score) in passages.iter().zip(&similarity) {
        eprintln!("{score:.3}  {text}");
    }
    // Neither related passage shares a word with the question, yet both beat the unrelated ones.
    let lowest_related = similarity[0].min(similarity[1]);
    let highest_unrelated = similarity[2].max(similarity[3]);
    assert!(lowest_related > highest_unrelated, "{similarity:?}");
}

#[tokio::test]
async fn embedding_is_repeatable_and_handles_empty_and_long_text() {
    let Some(model) = model() else { return };
    let a = model
        .embed_passages(vec!["The retainer is due on signing.".into()])
        .await
        .unwrap();
    let b = model
        .embed_passages(vec!["The retainer is due on signing.".into()])
        .await
        .unwrap();
    assert!(
        dot(&a[0], &b[0]) > 0.999,
        "the same text gives the same vector"
    );
    let long = "word ".repeat(5_000);
    let vectors = model
        .embed_passages(vec![String::new(), long])
        .await
        .unwrap();
    assert_eq!(vectors.len(), 2);
    assert!(
        vectors
            .iter()
            .all(|v| v.len() == DIMENSIONS && v.iter().all(|x| x.is_finite()))
    );
}
