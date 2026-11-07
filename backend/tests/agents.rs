//! Shared agents against a real Postgres with a scripted stand-in for the model. Run with
//! scripts/test.sh. They cover what matters most: one agent's tasks never share a conversation, a
//! task reads only what the person who started it may read, budgets and limits stop work, versions
//! can be rolled back, and a person's report is not visible to anyone else.

mod common;

use std::{sync::Mutex, time::Duration};

use async_trait::async_trait;
use common::{Env, Firm};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::{
    agent_runner::{AgentModel, ModelError, TurnReply, TurnRequest, run_pass},
    agents::{self, AgentSettings, AgentSpec, Started},
    auth::Principal,
    db::TenantTx,
};

type Script = Box<dyn Fn(&[Value]) -> TurnReply + Send + Sync>;

/// Replies from a script and remembers every conversation it was sent.
struct Scripted {
    script: Script,
    seen: Mutex<Vec<Vec<Value>>>,
    delay: Duration,
}

impl Scripted {
    fn new(script: impl Fn(&[Value]) -> TurnReply + Send + Sync + 'static) -> Self {
        Self { script: Box::new(script), seen: Mutex::default(), delay: Duration::ZERO }
    }
    fn slow(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
    fn conversations(&self) -> Vec<Vec<Value>> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentModel for Scripted {
    async fn turn(&self, request: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
        self.seen.lock().unwrap().push(request.messages.to_vec());
        tokio::time::sleep(self.delay).await;
        Ok((self.script)(request.messages))
    }
}

fn reply(content: Value, stop: &str) -> TurnReply {
    TurnReply {
        content: content.as_array().unwrap().clone(),
        stop_reason: stop.into(),
        input_tokens: 1_000,
        output_tokens: 200,
        model: "claude-haiku-5-5".into(),
    }
}

fn text(t: &str) -> TurnReply {
    reply(json!([{ "type": "text", "text": t }]), "end_turn")
}

fn search(query: &str) -> TurnReply {
    reply(json!([{ "type": "tool_use", "id": "toolu_1", "name": "search_documents", "input": { "query": query } }]), "tool_use")
}

/// Searches once, then writes a report that repeats what the search returned.
fn searcher() -> Scripted {
    Scripted::new(|messages| match messages.last().and_then(|m| m["content"][0]["content"].as_str()) {
        Some(found) => text(&format!("REPORT: {found}")),
        None => search("retreat budget"),
    })
}

fn person(firm: &Firm, user: &str, groups: &[&str]) -> Principal {
    Principal {
        tenant: firm.tenant,
        user: user.into(),
        groups: groups.iter().map(|g| g.to_string()).collect(),
        email: None,
    }
}

fn spec(name: &str) -> AgentSpec {
    AgentSpec {
        name: name.into(),
        role: "Research & Reporting".into(),
        description: "Finds and reports.".into(),
        icon: "search".into(),
        color: "blue".into(),
        instructions: "Research the question and report.".into(),
        tools: vec!["search_documents".into(), "read_document".into(), "lookup_knowledge".into()],
        model: "standard".into(),
        effort: "low".into(),
    }
}

const DOC: &str = "The retreat budget is 5000 dollars for the whole group.";

async fn firm(env: &Env) -> Firm {
    env.firm("Firm A", "retreat.docx", DOC, &["user:alice"]).await
}

async fn create(env: &Env, firm: &Firm, settings: AgentSettings) -> Uuid {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let id = agents::create_agent(&tx, "admin", &spec("John"), &settings).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn start(env: &Env, p: &Principal, agent: Uuid, what: &str) -> Started {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, p.tenant).await.unwrap();
    let started = agents::start_task(&tx, p, agent, what).await.unwrap();
    tx.commit().await.unwrap();
    started
}

async fn queued(env: &Env, p: &Principal, agent: Uuid, what: &str) -> Uuid {
    match start(env, p, agent, what).await {
        Started::Queued(id) => id,
        other => panic!("not queued: {other:?}"),
    }
}

async fn task(env: &Env, viewer: &Principal, id: Uuid) -> Option<agents::TaskView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, viewer.tenant).await.unwrap();
    agents::get_task(&tx, viewer, id).await.unwrap()
}

async fn run(env: &Env, model: Scripted) -> (usize, std::sync::Arc<Scripted>) {
    let model = std::sync::Arc::new(model);
    let n = run_pass(&env.pool("t_worker", 8), model.clone(), None).await.unwrap();
    (n, model)
}

#[tokio::test]
async fn a_task_searches_reports_and_records_what_it_did() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let alice = person(&f, "alice", &[]);
    let agent = create(&env, &f, AgentSettings::default()).await;
    let id = queued(&env, &alice, agent, "What is the retreat budget?").await;

    let (n, _) = run(&env, searcher()).await;
    assert_eq!(n, 1);
    let t = task(&env, &alice, id).await.unwrap();
    assert_eq!(t.status, "done", "{:?}", t.error);
    assert!(t.result.unwrap().contains("5000 dollars"));
    assert_eq!(t.turns, 2);
    assert!(t.cost_micros > 0);
    assert_eq!(t.sources.unwrap().as_array().unwrap().len(), 1);
    let kinds: Vec<&str> = t.events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["created", "started", "tool", "finished"]);
    // The history says what happened, not what the documents said.
    assert!(t.events.iter().all(|e| !e.summary.contains("5000")));

    // The work counts against the person who asked.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert_eq!(tx.usage_this_month("alice").await.unwrap().used_micros, t.cost_micros);
}

#[tokio::test]
async fn a_task_cannot_read_what_the_person_who_started_it_cannot() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let bob = person(&f, "bob", &[]);
    let agent = create(&env, &f, AgentSettings::default()).await;
    let id = queued(&env, &bob, agent, "What is the retreat budget?").await;

    run(&env, searcher()).await;
    let t = task(&env, &bob, id).await.unwrap();
    assert_eq!(t.status, "done");
    let result = t.result.unwrap();
    assert!(!result.contains("5000"), "{result}");
    assert!(result.contains("No matching documents"));
    assert!(t.sources.unwrap().as_array().unwrap().is_empty());
}

#[tokio::test]
async fn tasks_of_one_agent_never_share_a_conversation() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let agent = create(&env, &f, AgentSettings { max_concurrent: Some(5), ..Default::default() }).await;
    let a = queued(&env, &person(&f, "alice", &[]), agent, "SECRET-ALPHA question").await;
    let b = queued(&env, &person(&f, "bob", &[]), agent, "SECRET-BETA question").await;

    let (n, model) = run(&env, searcher().slow(Duration::from_millis(100))).await;
    assert_eq!(n, 2);
    for conversation in model.conversations() {
        let all = serde_json::to_string(&conversation).unwrap();
        assert!(!(all.contains("ALPHA") && all.contains("BETA")), "two tasks shared a conversation: {all}");
    }
    let alice = person(&f, "alice", &[]);
    assert!(task(&env, &alice, a).await.unwrap().result.is_some());
    // Bob's task is not Alice's to open.
    assert!(task(&env, &alice, b).await.is_none());
}

#[tokio::test]
async fn an_agent_does_no_more_at_once_than_it_is_allowed() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let agent = create(&env, &f, AgentSettings { max_concurrent: Some(1), ..Default::default() }).await;
    let alice = person(&f, "alice", &[]);
    let one = queued(&env, &alice, agent, "first").await;
    let two = queued(&env, &alice, agent, "second").await;

    run(&env, searcher().slow(Duration::from_millis(300))).await;
    let statuses = [task(&env, &alice, one).await.unwrap().status, task(&env, &alice, two).await.unwrap().status];
    assert_eq!(statuses.iter().filter(|s| *s == "done").count(), 1, "{statuses:?}");
    assert_eq!(statuses.iter().filter(|s| *s == "queued").count(), 1, "{statuses:?}");

    // The one that had to wait runs on the next pass, once its wait is over.
    env.admin.execute("UPDATE agent_queue SET run_after = now()", &[]).await.unwrap();
    run(&env, searcher()).await;
    assert_eq!(task(&env, &alice, one).await.unwrap().status, "done");
    assert_eq!(task(&env, &alice, two).await.unwrap().status, "done");
}

#[tokio::test]
async fn budgets_and_allowances_stop_work() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let alice = person(&f, "alice", &[]);

    // An agent whose month's budget is already used up takes no more tasks.
    let agent = create(&env, &f, AgentSettings { monthly_budget_micros: Some(Some(1)), ..Default::default() }).await;
    queued(&env, &alice, agent, "uses the budget").await;
    run(&env, searcher()).await;
    assert_eq!(start(&env, &alice, agent, "one more").await, Started::BudgetReached);

    // A person who has used their allowance cannot have an agent work for them.
    let other = create(&env, &f, AgentSettings::default()).await;
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    tx.set_monthly_limit(Some(1)).await.unwrap();
    tx.commit().await.unwrap();
    let id = queued(&env, &alice, other, "over the allowance").await;
    run(&env, searcher()).await;
    let t = task(&env, &alice, id).await.unwrap();
    assert_eq!(t.status, "failed");
    assert!(t.error.unwrap().contains("allowance"));
}

#[tokio::test]
async fn versions_can_be_rolled_back_and_tasks_keep_the_version_they_started_on() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let alice = person(&f, "alice", &[]);
    let agent = create(&env, &f, AgentSettings::default()).await;
    let id = queued(&env, &alice, agent, "on version one").await;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let mut changed = spec("John");
    changed.instructions = "Be brief.".into();
    agents::update_agent(&tx, agent, "admin", Some(&changed), &AgentSettings::default(), "shorter").await.unwrap();
    // Saving the same thing again makes no new version.
    agents::update_agent(&tx, agent, "admin", Some(&changed), &AgentSettings::default(), "again").await.unwrap();
    assert_eq!(agents::versions(&tx, agent).await.unwrap().len(), 2);
    assert_eq!(agents::rollback(&tx, agent, "admin", 1).await.unwrap(), Some(3));
    assert_eq!(agents::rollback(&tx, agent, "admin", 99).await.unwrap(), None);
    let current = agents::get_agent(&tx, agent).await.unwrap().unwrap();
    assert_eq!((current.version, current.instructions.as_deref()), (3, Some("Research the question and report.")));
    tx.commit().await.unwrap();

    assert_eq!(task(&env, &alice, id).await.unwrap().agent_name, "John");
    let version: i32 = env.admin.query_one("SELECT agent_version FROM agent_tasks WHERE id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(version, 1);
}

#[tokio::test]
async fn a_stopped_task_does_not_run_and_a_leased_one_waits_for_its_lease() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let alice = person(&f, "alice", &[]);
    let agent = create(&env, &f, AgentSettings::default()).await;
    let stopped = queued(&env, &alice, agent, "stop me").await;
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(agents::cancel_task(&tx, &alice, stopped).await.unwrap());
    assert!(!agents::cancel_task(&tx, &person(&f, "bob", &[]), stopped).await.unwrap());
    tx.commit().await.unwrap();
    assert_eq!(run(&env, searcher()).await.0, 0);
    assert_eq!(task(&env, &alice, stopped).await.unwrap().status, "cancelled");

    // Another worker holds this one; when its lease runs out it is picked up.
    let held = queued(&env, &alice, agent, "held").await;
    env.admin.execute("UPDATE agent_queue SET claimed_until = now() + interval '5 minutes' WHERE task_id = $1", &[&held]).await.unwrap();
    assert_eq!(run(&env, searcher()).await.0, 0);
    env.admin.execute("UPDATE agent_queue SET claimed_until = now() - interval '1 second' WHERE task_id = $1", &[&held]).await.unwrap();
    assert_eq!(run(&env, searcher()).await.0, 1);
    assert_eq!(task(&env, &alice, held).await.unwrap().status, "done");
}

#[tokio::test]
async fn what_each_kind_of_person_can_see() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let admin = person(&f, "admin", &["firm_admin"]);
    let alice = person(&f, "alice", &[]);
    let agent = create(&env, &f, AgentSettings { audience: Some(vec!["user:alice".into()]), monthly_budget_micros: Some(Some(5_000_000)), ..Default::default() }).await;
    let id = queued(&env, &alice, agent, "Private question").await;
    run(&env, searcher()).await;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    // The audience sees the agent without its instructions, budget or spending.
    let mine = agents::list_agents(&tx, &alice).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert!(mine[0].instructions.is_none() && mine[0].monthly_budget_micros.is_none());
    // Others are not offered it and cannot give it work.
    let bob = person(&f, "bob", &[]);
    assert!(agents::list_agents(&tx, &bob).await.unwrap().is_empty());
    assert_eq!(agents::start_task(&tx, &bob, agent, "hello").await.unwrap(), Started::NotAvailable);
    // An administrator sees all of it.
    let all = agents::list_agents(&tx, &admin).await.unwrap();
    assert!(all[0].instructions.is_some() && all[0].spent_this_month_micros > 0);
    tx.commit().await.unwrap();

    // An administrator sees that the task happened and what it cost, not what was asked or found.
    let seen = task(&env, &admin, id).await.unwrap();
    assert_eq!(seen.status, "done");
    assert!(seen.instruction.is_none() && seen.result.is_none() && seen.sources.is_none());
    assert!(task(&env, &bob, id).await.is_none());

    // Another firm sees nothing of it.
    let other = env.firm("Firm B", "b.txt", "text", &["user:zed"]).await;
    let zed_admin = person(&other, "zed", &["firm_admin"]);
    assert!(task(&env, &zed_admin, id).await.is_none());
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, other.tenant).await.unwrap();
    assert!(agents::list_agents(&tx, &zed_admin).await.unwrap().is_empty());
}

/// Looks the report's document up by name in the search results it was shown.
fn id_of(messages: &[Value], name: &str) -> String {
    let results = serde_json::to_string(messages).unwrap();
    let at = results.find(&format!("] {name}")).expect("the search showed the document");
    let before = &results[..at];
    let start = before.rfind("document_id: ").unwrap() + "document_id: ".len();
    before[start..].to_string()
}

#[tokio::test]
async fn a_finished_report_names_documents_lists_only_those_used_and_is_checked_against_what_was_read() {
    let env = Env::new().await;
    let f = firm(&env).await;
    // A second file that matches the search but is never opened or mentioned.
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, f.tenant).await.unwrap();
    let other = tx
        .upsert_document(&workspace_cloud::db::NewDocument {
            connection_id: f.connection,
            external_id: "ext-notes".into(),
            name: "notes.docx".into(),
            path: "/notes.docx".into(),
            readable_by: vec!["user:alice".into()],
        })
        .await
        .unwrap();
    tx.add_chunk(other, 0, "The retreat schedule is in spring.").await.unwrap();
    tx.commit().await.unwrap();

    let alice = person(&f, "alice", &[]);
    let agent = create(&env, &f, AgentSettings::default()).await;
    let id = queued(&env, &alice, agent, "What is the retreat budget?").await;

    let model = Scripted::new(|messages| match messages.len() {
        1 => search("retreat budget"),
        3 => {
            let doc = id_of(messages, "retreat.docx");
            reply(json!([{ "type": "tool_use", "id": "toolu_2", "name": "read_document", "input": { "document_id": doc } }]), "tool_use")
        }
        _ => {
            let doc = id_of(messages, "retreat.docx");
            text(&format!(
                "# Report\n\nThe budget is **$5,000.00** (see Doc {}), the note says \"The retreat budget is 5000 dollars\", and we spent $7,777.77. Full ID: {doc}.",
                &doc[..8]
            ))
        }
    });
    run(&env, model).await;

    let t = task(&env, &alice, id).await.unwrap();
    assert_eq!(t.status, "done", "{:?}", t.error);
    let report = t.result.unwrap();
    // Documents are named in words, never by ID.
    assert!(report.contains("retreat.docx") && !report.contains("document_id"), "{report}");
    let full = f.document.to_string();
    assert!(!report.contains(&full) && !report.contains(&full[..8]), "{report}");
    // Only the file that was read is listed; the other one only turned up in a search.
    let sources = t.sources.unwrap();
    let names: Vec<&str> = sources.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["retreat.docx"]);
    // The figure that no document contains and the quotation that is there are told apart.
    let checks = t.checks.unwrap();
    assert_eq!(checks["figures_missing"], json!(["$7,777.77"]), "{checks}");
    assert_eq!((checks["quotes_total"].as_u64(), checks["quotes_found"].as_u64()), (Some(1), Some(1)));
    // An administrator sees neither the report nor its checks.
    let admin = person(&f, "admin", &["firm_admin"]);
    let seen = task(&env, &admin, id).await.unwrap();
    assert!(seen.result.is_none() && seen.checks.is_none());
}

#[tokio::test]
async fn an_agent_checks_arithmetic_with_the_calculator_and_its_results_count_as_evidence() {
    let env = Env::new().await;
    let f = firm(&env).await;
    let alice = person(&f, "alice", &[]);
    // Not offered any tools of its own: the calculator is still there.
    let agent = create(&env, &f, AgentSettings { max_concurrent: Some(3), ..Default::default() }).await;
    let id = queued(&env, &alice, agent, "Check the pens variance").await;

    let model = Scripted::new(|messages| match messages.len() {
        1 => reply(
            json!([
                { "type": "tool_use", "id": "toolu_1", "name": "calculate", "input": { "expression": "20 - 32.15", "label": "Pens variance" } },
                { "type": "tool_use", "id": "toolu_2", "name": "calculate", "input": { "expression": "1 / 0" } },
            ]),
            "tool_use",
        ),
        _ => {
            let results = serde_json::to_string(messages).unwrap();
            assert!(results.contains("20 - 32.15 = -12.15"), "{results}");
            assert!(results.contains("division by zero"), "{results}");
            text("## Discrepancies found\n\n| Where | Says | Should be |\n|---|---|---|\n| Pens | -$12.15 | $20.00 |")
        }
    });
    run(&env, model).await;

    let t = task(&env, &alice, id).await.unwrap();
    assert_eq!(t.status, "done", "{:?}", t.error);
    // The computed difference is evidence; the figure nobody calculated or read is not.
    let checks = t.checks.unwrap();
    assert_eq!(checks["figures_missing"], json!(["$20.00"]), "{checks}");
    // The history says a calculation happened, not what was calculated.
    let summaries: Vec<&str> = t.events.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(summaries.iter().filter(|s| **s == "Ran a calculation").count(), 2);
    assert!(summaries.iter().all(|s| !s.contains("32.15")));
}
