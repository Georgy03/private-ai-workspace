//! Company knowledge against a real Postgres, with a scripted stand-in for the model. Run with
//! scripts/test.sh. They cover what matters most: nothing is read until switched on, nothing is
//! invented, a person only sees what a document they may open supports, and changes are noticed.

mod common;

use std::collections::HashMap;

use async_trait::async_trait;
use common::{Env, Firm};
use uuid::Uuid;
use workspace_cloud::{
    db::{NewDocument, Passage, TenantTx},
    knowledge::{
        AnalyzeError, Analyzer, ExtractedEntity, ExtractedFact, ExtractedRelationship, Extraction,
        ProfileDraft, ProfileInput, Term,
    },
    knowledge_store as store,
    knowledge_worker::analyse_pass,
    queue,
};

/// Says what it was told to say about each document name, and can be told to fail.
struct Scripted {
    answers: std::sync::Mutex<HashMap<String, Extraction>>,
    failing: std::sync::Mutex<Vec<String>>,
    calls: std::sync::atomic::AtomicUsize,
    profiles: std::sync::atomic::AtomicUsize,
}

impl Scripted {
    fn new() -> Self {
        Self {
            answers: Default::default(),
            failing: Default::default(),
            calls: Default::default(),
            profiles: Default::default(),
        }
    }
    fn says(&self, name: &str, extraction: Extraction) {
        self.answers.lock().unwrap().insert(name.into(), extraction);
    }
    fn fails_on(&self, name: &str) {
        self.failing.lock().unwrap().push(name.into());
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl Analyzer for Scripted {
    fn model(&self) -> &str {
        "scripted"
    }
    async fn analyze(&self, name: &str, _: &str) -> Result<Extraction, AnalyzeError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.failing.lock().unwrap().iter().any(|n| n == name) {
            return Err(AnalyzeError {
                message: "the model is down".into(),
            });
        }
        Ok(self
            .answers
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }
    async fn profile(&self, _: &ProfileInput) -> Result<ProfileDraft, AnalyzeError> {
        self.profiles
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ProfileDraft {
            summary: "A roofing company.".into(),
            services: vec!["Roof replacement".into()],
            terminology: vec![Term {
                term: "CO".into(),
                meaning: "change order".into(),
            }],
        })
    }
}

fn entity(name: &str, kind: &str, role: &str, quote: &str) -> ExtractedEntity {
    ExtractedEntity {
        name: name.into(),
        kind: kind.into(),
        role: role.into(),
        quote: quote.into(),
    }
}
fn fact(statement: &str, subject: &str, quote: &str) -> ExtractedFact {
    ExtractedFact {
        kind: "fact".into(),
        statement: statement.into(),
        subject: subject.into(),
        quote: quote.into(),
    }
}

const LETTER: &str = "Maple Roofing LLC will replace the roof for Dana Whitfield. The price is $18,400. Change orders need written approval.";

fn letter_extraction() -> Extraction {
    Extraction {
        doc_type: "contract".into(),
        summary: "A roofing engagement.".into(),
        entities: vec![
            entity(
                "Maple Roofing LLC",
                "organization",
                "vendor",
                "Maple Roofing LLC will replace the roof",
            ),
            entity("Dana Whitfield", "person", "customer", "for Dana Whitfield"),
        ],
        facts: vec![fact(
            "The price is $18,400.",
            "Maple Roofing LLC",
            "The price is $18,400",
        )],
        relationships: vec![ExtractedRelationship {
            subject: "Dana Whitfield".into(),
            predicate: "customer_of".into(),
            object: "Maple Roofing LLC".into(),
            quote: "replace the roof for Dana Whitfield".into(),
        }],
    }
}

async fn add_document(env: &Env, firm: &Firm, name: &str, text: &str, readers: &[&str]) -> Uuid {
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    let id = tx
        .upsert_document(&NewDocument {
            connection_id: firm.connection,
            external_id: name.into(),
            name: name.into(),
            path: String::new(),
            readable_by: readers.iter().map(|r| r.to_string()).collect(),
        })
        .await
        .unwrap();
    tx.replace_chunks(
        id,
        &[Passage {
            text: text.into(),
            label: None,
        }],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

/// A firm with one document and one queued connection, analysis switched on or off.
async fn firm_with(env: &Env, on: bool) -> Firm {
    let firm = env
        .firm("Firm A", "letter.docx", LETTER, &["user:alice"])
        .await;
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    queue::enqueue(&tx, firm.connection).await.unwrap();
    tx.commit().await.unwrap();
    if on {
        set_enabled(env, &firm, true).await;
    }
    firm
}

async fn set_enabled(env: &Env, firm: &Firm, on: bool) {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    store::set_analysis_enabled(&tx, on).await.unwrap();
    tx.commit().await.unwrap();
}

fn readers(user: &str) -> Vec<String> {
    vec![format!("user:{user}")]
}

async fn facts_for(env: &Env, firm: &Firm, user: &str) -> Vec<store::FactView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    store::list_facts(&tx, &readers(user), &all_statuses(), None, false, 50)
        .await
        .unwrap()
}

async fn entities_for(env: &Env, firm: &Firm, user: &str) -> Vec<store::EntityView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    store::list_entities(&tx, &readers(user), &all_statuses(), None, None, 50)
        .await
        .unwrap()
}

fn all_statuses() -> Vec<String> {
    ["suggested", "verified", "rejected", "outdated"]
        .map(String::from)
        .to_vec()
}

#[tokio::test]
async fn nothing_is_read_until_an_administrator_switches_it_on() {
    let env = Env::new().await;
    let firm = firm_with(&env, false).await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    let pool = env.pool("t_worker", 4);

    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 0);
    assert_eq!(model.calls(), 0, "no document text went to the model");

    set_enabled(&env, &firm, true).await;
    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 1);
    assert_eq!(model.calls(), 1);
    // Read once: a second pass has nothing new to do.
    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 0);
    assert_eq!(model.calls(), 1);
}

#[tokio::test]
async fn what_a_document_says_is_stored_with_the_evidence_it_came_from() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();

    let entities = entities_for(&env, &firm, "alice").await;
    let names: Vec<&str> = entities.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    let maple = entities
        .iter()
        .find(|e| e.name == "Maple Roofing LLC")
        .unwrap();
    assert_eq!(
        (
            maple.kind.as_str(),
            maple.roles.clone(),
            maple.status.as_str()
        ),
        ("organization", vec!["vendor".to_string()], "suggested")
    );
    assert_eq!(maple.evidence[0].document_name, "letter.docx");
    assert_eq!(
        maple.evidence[0].quote,
        "Maple Roofing LLC will replace the roof"
    );

    let facts = facts_for(&env, &firm, "alice").await;
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].statement, "The price is $18,400.");
    assert_eq!(facts[0].subject.as_deref(), Some("Maple Roofing LLC"));
    assert_eq!(facts[0].evidence[0].quote, "The price is $18,400");

    let count: i64 = env
        .admin
        .query_one("SELECT count(*) FROM relationships", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    let kind: String = env
        .admin
        .query_one("SELECT doc_type FROM document_analysis", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(kind, "contract");
}

#[tokio::test]
async fn an_invented_quote_never_reaches_the_knowledge() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    let model = Scripted::new();
    let mut made_up = letter_extraction();
    made_up.entities.push(entity(
        "Invented Corp",
        "organization",
        "none",
        "Invented Corp signed this",
    ));
    made_up
        .facts
        .push(fact("The price is $99,999.", "", "The price is $99,999"));
    model.says("letter.docx", made_up);
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();
    let names: Vec<String> = entities_for(&env, &firm, "alice")
        .await
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(!names.contains(&"Invented Corp".to_string()), "{names:?}");
    assert_eq!(facts_for(&env, &firm, "alice").await.len(), 1);
}

#[tokio::test]
async fn the_same_entity_in_two_documents_is_one_entity_with_two_pieces_of_evidence() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    add_document(
        &env,
        &firm,
        "invoice.pdf",
        "Invoice from Maple Roofing, LLC for $9,200.",
        &["user:alice"],
    )
    .await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    model.says(
        "invoice.pdf",
        Extraction {
            doc_type: "invoice".into(),
            summary: "An invoice.".into(),
            entities: vec![entity(
                "Maple Roofing, LLC",
                "organization",
                "vendor",
                "Invoice from Maple Roofing, LLC",
            )],
            ..Default::default()
        },
    );
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();

    let entities = entities_for(&env, &firm, "alice").await;
    let maple: Vec<_> = entities
        .iter()
        .filter(|e| e.name.to_lowercase().contains("maple"))
        .collect();
    assert_eq!(maple.len(), 1, "one entity, not two spellings");
    assert_eq!(maple[0].mentions, 2);
    assert_eq!(maple[0].evidence.len(), 2);
}

#[tokio::test]
async fn a_person_sees_only_what_a_document_they_may_open_supports() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    add_document(
        &env,
        &firm,
        "salaries.xlsx",
        "Marcus Reyes earns $85,000 a year.",
        &["user:boss"],
    )
    .await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    model.says(
        "salaries.xlsx",
        Extraction {
            doc_type: "financial_statement".into(),
            summary: "Pay.".into(),
            entities: vec![entity(
                "Marcus Reyes",
                "person",
                "employee",
                "Marcus Reyes earns $85,000 a year",
            )],
            facts: vec![fact(
                "Marcus Reyes earns $85,000 a year.",
                "Marcus Reyes",
                "Marcus Reyes earns $85,000 a year",
            )],
            ..Default::default()
        },
    );
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();

    let alice_facts: Vec<String> = facts_for(&env, &firm, "alice")
        .await
        .into_iter()
        .map(|f| f.statement)
        .collect();
    assert_eq!(
        alice_facts,
        ["The price is $18,400."],
        "alice cannot open the salary file"
    );
    let alice_entities: Vec<String> = entities_for(&env, &firm, "alice")
        .await
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(
        !alice_entities.contains(&"Marcus Reyes".to_string()),
        "{alice_entities:?}"
    );
    let boss_facts: Vec<String> = facts_for(&env, &firm, "boss")
        .await
        .into_iter()
        .map(|f| f.statement)
        .collect();
    assert_eq!(boss_facts, ["Marcus Reyes earns $85,000 a year."]);
    // Someone who can open neither sees nothing at all.
    assert!(facts_for(&env, &firm, "carol").await.is_empty());
    assert!(entities_for(&env, &firm, "carol").await.is_empty());
}

#[tokio::test]
async fn one_firm_never_sees_another_firms_knowledge() {
    let env = Env::new().await;
    let a = firm_with(&env, true).await;
    let b = env
        .firm("Firm B", "b.docx", "Nothing here.", &["user:alice"])
        .await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();
    assert_eq!(facts_for(&env, &a, "alice").await.len(), 1);
    assert!(facts_for(&env, &b, "alice").await.is_empty());
    assert!(entities_for(&env, &b, "alice").await.is_empty());
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, b.tenant).await.unwrap();
    assert_eq!(store::overview(&tx).await.unwrap().facts_total, 0);
}

#[tokio::test]
async fn a_confirmed_fact_is_flagged_when_its_source_changes_and_outdated_when_it_disappears() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    let pool = env.pool("t_worker", 4);
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    analyse_pass(&pool, &model).await.unwrap();

    // Someone confirms the price.
    let fact_id = facts_for(&env, &firm, "alice").await[0].id;
    {
        let mut api = env.api().await;
        let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
        let done = store::review_fact(
            &tx,
            &readers("alice"),
            fact_id,
            "boss",
            &store::Review::Confirm,
        )
        .await
        .unwrap();
        assert_eq!(done, store::Reviewed::Done);
        tx.commit().await.unwrap();
    }
    let confirmed = &facts_for(&env, &firm, "alice").await[0];
    assert_eq!(
        (
            confirmed.status.as_str(),
            confirmed.needs_review,
            confirmed.stale
        ),
        ("verified", false, false)
    );

    // The document is edited, and the price is still there: confirmed, but check it again.
    env.admin
        .execute("UPDATE documents SET content_hash = 'v2'", &[])
        .await
        .unwrap();
    analyse_pass(&pool, &model).await.unwrap();
    let after_edit = &facts_for(&env, &firm, "alice").await[0];
    assert_eq!(
        (after_edit.status.as_str(), after_edit.needs_review),
        ("verified", true)
    );
    assert!(
        !after_edit.evidence[0].source_changed,
        "the new reading refreshed the quote"
    );

    // The document is edited again and the price is gone: the confirmed fact is outdated.
    env.admin
        .execute("UPDATE documents SET content_hash = 'v3'", &[])
        .await
        .unwrap();
    model.says(
        "letter.docx",
        Extraction {
            doc_type: "contract".into(),
            summary: "Edited.".into(),
            ..Default::default()
        },
    );
    analyse_pass(&pool, &model).await.unwrap();
    // With no evidence left it is no longer shown to anyone through a document, but it is kept.
    let row = env
        .admin
        .query_one("SELECT status, needs_review FROM facts", &[])
        .await
        .unwrap();
    assert_eq!(
        (row.get::<_, String>(0), row.get::<_, bool>(1)),
        ("outdated".to_string(), true)
    );
    // A suggestion nobody confirmed simply goes away.
    let suggested: i64 = env
        .admin
        .query_one(
            "SELECT count(*) FROM entities WHERE status = 'suggested'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(suggested, 0);
}

#[tokio::test]
async fn a_rejected_fact_is_not_suggested_again() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    let pool = env.pool("t_worker", 4);
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    analyse_pass(&pool, &model).await.unwrap();
    let fact_id = facts_for(&env, &firm, "alice").await[0].id;
    {
        let mut api = env.api().await;
        let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
        store::review_fact(
            &tx,
            &readers("alice"),
            fact_id,
            "boss",
            &store::Review::Reject,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    env.admin
        .execute("UPDATE documents SET content_hash = 'v2'", &[])
        .await
        .unwrap();
    analyse_pass(&pool, &model).await.unwrap();
    let status: String = env
        .admin
        .query_one("SELECT status FROM facts", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        status, "rejected",
        "it stays rejected when the document is read again"
    );
}

#[tokio::test]
async fn review_can_confirm_correct_and_refuses_what_a_person_cannot_see() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    add_document(
        &env,
        &firm,
        "terms.docx",
        "Late invoices accrue interest at 1.5% a month.",
        &["user:alice"],
    )
    .await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    model.says(
        "terms.docx",
        Extraction {
            doc_type: "policy".into(),
            summary: "Terms.".into(),
            facts: vec![fact(
                "Late invoices accrue interest at 1.5% a month.",
                "",
                "Late invoices accrue interest at 1.5% a month",
            )],
            ..Default::default()
        },
    );
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();
    let facts = facts_for(&env, &firm, "alice").await;
    let price = facts
        .iter()
        .find(|f| f.statement.starts_with("The price"))
        .unwrap()
        .id;
    let interest = facts
        .iter()
        .find(|f| f.statement.starts_with("Late"))
        .unwrap()
        .id;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    // A correction replaces the wording and confirms it.
    let done = store::review_fact(
        &tx,
        &readers("alice"),
        interest,
        "boss",
        &store::Review::Correct("Late invoices accrue 2% interest a month.".into()),
    )
    .await
    .unwrap();
    assert_eq!(done, store::Reviewed::Done);
    // Correcting a fact into another existing one is refused.
    let clash = store::review_fact(
        &tx,
        &readers("alice"),
        interest,
        "boss",
        &store::Review::Correct("The price is $18,400".into()),
    )
    .await
    .unwrap();
    assert_eq!(clash, store::Reviewed::Duplicate);
    // Someone who cannot open the evidence cannot review the fact, and cannot tell it exists.
    let hidden = store::review_fact(
        &tx,
        &readers("carol"),
        price,
        "carol",
        &store::Review::Confirm,
    )
    .await
    .unwrap();
    assert_eq!(hidden, store::Reviewed::NotFound);
    let missing = store::review_fact(
        &tx,
        &readers("alice"),
        Uuid::new_v4(),
        "boss",
        &store::Review::Confirm,
    )
    .await
    .unwrap();
    assert_eq!(missing, store::Reviewed::NotFound);
    tx.commit().await.unwrap();
    let row = env
        .admin
        .query_one(
            "SELECT statement, status, verified_by FROM facts WHERE id = $1",
            &[&interest],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, String>(2)
        ),
        (
            "Late invoices accrue 2% interest a month.".to_string(),
            "verified".to_string(),
            "boss".to_string()
        )
    );
    let untouched: String = env
        .admin
        .query_one("SELECT status FROM facts WHERE id = $1", &[&price])
        .await
        .unwrap()
        .get(0);
    assert_eq!(untouched, "suggested");
}

#[tokio::test]
async fn a_failing_document_is_retried_a_few_times_and_then_left_alone() {
    let env = Env::new().await;
    let _firm = firm_with(&env, true).await;
    let pool = env.pool("t_worker", 4);
    let model = Scripted::new();
    model.fails_on("letter.docx");
    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 1);
    assert_eq!(model.calls(), 1);
    // Not again straight away.
    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 0);
    assert_eq!(model.calls(), 1);
    for attempt in 2..=store::MAX_ATTEMPTS {
        env.admin
            .execute(
                "UPDATE document_analysis SET analyzed_at = now() - interval '2 hours'",
                &[],
            )
            .await
            .unwrap();
        analyse_pass(&pool, &model).await.unwrap();
        assert_eq!(model.calls(), attempt as usize);
    }
    env.admin
        .execute(
            "UPDATE document_analysis SET analyzed_at = now() - interval '2 hours'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        analyse_pass(&pool, &model).await.unwrap(),
        0,
        "given up after {} tries",
        store::MAX_ATTEMPTS
    );
    // Until the document changes.
    env.admin
        .execute("UPDATE documents SET content_hash = 'changed'", &[])
        .await
        .unwrap();
    assert_eq!(analyse_pass(&pool, &model).await.unwrap(), 1);
}

#[tokio::test]
async fn the_company_profile_is_drafted_once_enough_is_read_and_a_confirmed_one_is_kept() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    for n in 0..11 {
        add_document(
            &env,
            &firm,
            &format!("doc{n}.txt"),
            "Some text about the business.",
            &["user:alice"],
        )
        .await;
    }
    let model = Scripted::new();
    for n in 0..11 {
        model.says(
            &format!("doc{n}.txt"),
            Extraction {
                doc_type: "report".into(),
                summary: format!("Note {n}."),
                ..Default::default()
            },
        );
    }
    let pool = env.pool("t_worker", 4);
    // Read everything (12 documents, 6 at a time), then the pass that finds nothing left drafts it.
    while analyse_pass(&pool, &model).await.unwrap() > 0 {}
    assert_eq!(model.profiles.load(std::sync::atomic::Ordering::SeqCst), 1);

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let profile = store::profile(&tx).await.unwrap().unwrap();
    assert_eq!(
        (profile.summary.as_str(), profile.status.as_str()),
        ("A roofing company.", "suggested")
    );
    assert_eq!(profile.services, ["Roof replacement"]);
    // An administrator corrects and confirms it, and later drafts do not overwrite it.
    assert!(
        store::review_profile(&tx, "boss", Some("We replace and repair roofs."), None)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let draft = ProfileDraft {
        summary: "Something else.".into(),
        ..Default::default()
    };
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    store::save_profile(&tx, &draft, 99, "scripted")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let kept: (String, String) = {
        let row = env
            .admin
            .query_one("SELECT summary, status FROM company_profile", &[])
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(
        kept,
        (
            "We replace and repair roofs.".to_string(),
            "verified".to_string()
        )
    );
}

#[tokio::test]
async fn the_overview_counts_what_is_known_and_what_still_needs_a_person() {
    let env = Env::new().await;
    let firm = firm_with(&env, true).await;
    let model = Scripted::new();
    model.says("letter.docx", letter_extraction());
    analyse_pass(&env.pool("t_worker", 4), &model)
        .await
        .unwrap();
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let o = store::overview(&tx).await.unwrap();
    assert_eq!(
        (o.documents_total, o.documents_analysed, o.documents_failed),
        (1, 1, 0)
    );
    assert_eq!(o.document_types, [("contract".to_string(), 1)]);
    assert_eq!(
        (
            o.facts_total,
            o.facts_unverified,
            o.facts_verified,
            o.entities_unverified,
            o.relationships
        ),
        (1, 1, 0, 2, 1)
    );
}
