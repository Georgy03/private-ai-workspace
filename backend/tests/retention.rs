//! How long agent work is kept, and what other people can see of it. Against a real Postgres with
//! a scripted stand-in for the model.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{Env, Firm};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::{
    agent_runner::{AgentModel, ModelError, TurnReply, TurnRequest, run_pass},
    agents::{self, AgentSettings, AgentSpec, Started},
    auth::Principal,
    db::TenantTx,
    retention,
};

fn reply(content: Value, stop: &str) -> TurnReply {
    TurnReply { content: content.as_array().unwrap().clone(), stop_reason: stop.into(), input_tokens: 1_000, output_tokens: 200, model: "claude-haiku-5-5".into() }
}

/// Searches, then reports what it found. Remembers every conversation it was sent.
struct Searcher {
    seen: Mutex<Vec<Vec<Value>>>,
}

#[async_trait]
impl AgentModel for Searcher {
    async fn turn(&self, request: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
        self.seen.lock().unwrap().push(request.messages.to_vec());
        Ok(match request.messages.last().and_then(|m| m["content"][0]["content"].as_str()) {
            Some(found) => reply(json!([{ "type": "text", "text": format!("REPORT: {found}") }]), "end_turn"),
            None => reply(json!([{ "type": "tool_use", "id": "toolu_1", "name": "search_documents", "input": { "query": "retreat budget" } }]), "tool_use"),
        })
    }
}

fn searcher() -> Arc<Searcher> {
    Arc::new(Searcher { seen: Default::default() })
}

fn person(firm: &Firm, user: &str, groups: &[&str]) -> Principal {
    Principal { tenant: firm.tenant, user: user.into(), groups: groups.iter().map(|g| g.to_string()).collect(), email: None }
}

async fn setup(env: &Env) -> (Firm, Uuid) {
    let firm = env.firm("Firm A", "retreat.docx", "The retreat budget is 5000 dollars for the whole group.", &["user:alice"]).await;
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let spec = AgentSpec {
        name: "John".into(),
        role: "Research".into(),
        description: "Reports.".into(),
        icon: "search".into(),
        color: "blue".into(),
        instructions: "Research and report.".into(),
        tools: vec!["search_documents".into(), "read_document".into()],
        model: "standard".into(),
        effort: "low".into(),
    };
    let agent = agents::create_agent(&tx, "admin", &spec, &AgentSettings::default()).await.unwrap();
    tx.commit().await.unwrap();
    (firm, agent)
}

/// A finished task of Alice's, with its conversation stored.
async fn finished_task(env: &Env, firm: &Firm, agent: Uuid, model: Arc<dyn AgentModel>) -> Uuid {
    let alice = person(firm, "alice", &[]);
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let Started::Queued(id) = agents::start_task(&tx, &alice, agent, "What is the SECRET retreat budget?").await.unwrap() else { panic!() };
    tx.commit().await.unwrap();
    run_pass(&env.pool("t_worker", 8), model, None).await.unwrap();
    id
}

async fn view(env: &Env, viewer: &Principal, id: Uuid) -> Option<agents::TaskView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, viewer.tenant).await.unwrap();
    agents::get_task(&tx, viewer, id).await.unwrap()
}

async fn days_until_purge(env: &Env, id: Uuid) -> Option<f64> {
    env.admin
        .query_one("SELECT EXTRACT(EPOCH FROM retention_until - now())::float8 / 86400 FROM agent_tasks WHERE id = $1", &[&id])
        .await
        .unwrap()
        .get(0)
}

async fn rows(env: &Env, table: &str, id: Uuid) -> i64 {
    env.admin.query_one(&format!("SELECT count(*) FROM {table} WHERE task_id = $1"), &[&id]).await.unwrap().get(0)
}

async fn asked_of(env: &Env, who: &Principal) -> Vec<agents::TaskView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, who.tenant).await.unwrap();
    retention::expiring(&tx, who).await.unwrap()
}

async fn purge(env: &Env) -> usize {
    retention::purge_pass(&env.pool("t_worker", 4)).await.unwrap()
}

#[tokio::test]
async fn finished_work_is_kept_for_ninety_days_by_default_and_the_firm_can_change_it() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let id = finished_task(&env, &f, agent, searcher()).await;
    let days = days_until_purge(&env, id).await.unwrap();
    assert!((89.9..90.1).contains(&days), "{days}");

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert_eq!(retention::policy(&tx).await.unwrap(), 90);
    // A shorter period moves work nobody has been asked about, but never closer than the notice period.
    retention::set_policy(&tx, 30).await.unwrap();
    tx.commit().await.unwrap();
    let days = days_until_purge(&env, id).await.unwrap();
    assert!((29.9..30.1).contains(&days), "{days}");
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    retention::set_policy(&tx, 0).await.unwrap();
    tx.commit().await.unwrap();
    assert!(days_until_purge(&env, id).await.is_none(), "0 means until the owner deletes it");
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    retention::set_policy(&tx, 30).await.unwrap();
    tx.commit().await.unwrap();
    // The report is 0 days old, so 30 days is fine; age it and the notice floor applies.
    env.admin.execute("UPDATE agent_tasks SET finished_at = now() - interval '200 days' WHERE id = $1", &[&id]).await.unwrap();
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    retention::set_policy(&tx, 30).await.unwrap();
    tx.commit().await.unwrap();
    let days = days_until_purge(&env, id).await.unwrap();
    assert!((6.9..7.1).contains(&days), "old work still gets a week's notice: {days}");
}

#[tokio::test]
async fn an_owner_is_asked_a_week_before_and_can_keep_a_report_or_let_it_go() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = finished_task(&env, &f, agent, searcher()).await;
    // Months away: not asked yet.
    assert!(asked_of(&env, &alice).await.is_empty());
    env.admin.execute("UPDATE agent_tasks SET retention_until = now() + interval '5 days' WHERE id = $1", &[&id]).await.unwrap();
    let asked = asked_of(&env, &alice).await;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].deletes_epoch.is_some() && asked[0].result.is_none(), "the question carries no report text");
    // Only the owner is ever asked.
    assert!(asked_of(&env, &person(&f, "bob", &[])).await.is_empty());
    assert!(asked_of(&env, &person(&f, "admin", &["firm_admin"])).await.is_empty());

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(!retention::keep(&tx, &person(&f, "bob", &[]), id).await.unwrap(), "not Bob's to decide");
    assert!(retention::keep(&tx, &alice, id).await.unwrap());
    tx.commit().await.unwrap();
    // Kept: not asked again, never deleted, even much later.
    assert!(days_until_purge(&env, id).await.is_none());
    assert!(asked_of(&env, &alice).await.is_empty());
    assert_eq!(purge(&env).await, 0);
    assert_eq!(view(&env, &alice, id).await.unwrap().retention_choice.as_deref(), Some("keep"));

    // A second report the owner lets run out: asked once, then deleted on its date.
    let second = finished_task(&env, &f, agent, searcher()).await;
    env.admin.execute("UPDATE agent_tasks SET retention_until = now() + interval '3 days' WHERE id = $1", &[&second]).await.unwrap();
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(retention::let_expire(&tx, &alice, second).await.unwrap());
    tx.commit().await.unwrap();
    assert!(asked_of(&env, &alice).await.is_empty(), "the answer is final: no more asking");
    assert_eq!(purge(&env).await, 0, "not due yet");
    env.admin.execute("UPDATE agent_tasks SET retention_until = now() - interval '1 minute' WHERE id = $1", &[&second]).await.unwrap();
    assert_eq!(purge(&env).await, 1);
}

#[tokio::test]
async fn deleting_removes_the_conversation_the_report_and_what_it_read_but_not_the_cost() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = finished_task(&env, &f, agent, searcher()).await;
    assert!(rows(&env, "agent_task_messages", id).await > 0 && rows(&env, "agent_task_events", id).await > 0);
    let cost = view(&env, &alice, id).await.unwrap().cost_micros;
    assert!(cost > 0);
    let spent_before = {
        let mut api = env.api().await;
        let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
        agents::get_agent(&tx, agent).await.unwrap().unwrap().spent_this_month_micros
    };

    // Only the owner can delete, and nothing running can be deleted.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(!retention::delete_now(&tx, &person(&f, "admin", &["firm_admin"]), id).await.unwrap());
    assert!(retention::delete_now(&tx, &alice, id).await.unwrap());
    tx.commit().await.unwrap();
    // It already looks deleted; the worker removes the content within a minute.
    let shown = view(&env, &alice, id).await.unwrap();
    assert!(shown.deleted && shown.result.is_none() && shown.instruction.is_none() && shown.title == "Deleted report");
    assert_eq!(purge(&env).await, 1);

    assert_eq!(rows(&env, "agent_task_messages", id).await, 0);
    assert_eq!(rows(&env, "agent_task_events", id).await, 0);
    let (instruction, title, result, sources): (String, String, Option<String>, Value) = {
        let r = env.admin.query_one("SELECT instruction, title, result, sources FROM agent_tasks WHERE id = $1", &[&id]).await.unwrap();
        (r.get(0), r.get(1), r.get(2), r.get(3))
    };
    assert_eq!((instruction.as_str(), title.as_str(), result, sources), ("", "Deleted report", None, json!([])));
    // The cost stays, so budgets and allowances still add up.
    assert_eq!(view(&env, &alice, id).await.unwrap().cost_micros, cost);
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert_eq!(agents::get_agent(&tx, agent).await.unwrap().unwrap().spent_this_month_micros, spent_before);
    // The deletion is in the audit log, as an ID and nothing else.
    let audit: String = env.admin.query_one("SELECT string_agg(action || detail::text, ' ') FROM audit_events", &[]).await.unwrap().get(0);
    assert!(audit.contains("agent_task_purged") && !audit.contains("SECRET") && !audit.contains("5000"), "{audit}");
}

#[tokio::test]
async fn administrators_see_that_work_happened_but_not_what_it_asked_or_which_files_it_read() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let admin = person(&f, "admin", &["firm_admin"]);
    let id = finished_task(&env, &f, agent, searcher()).await;

    let seen = view(&env, &admin, id).await.unwrap();
    assert_eq!(seen.status, "done");
    assert_eq!(seen.title, "Task", "the title is the start of the question, so it is not shown");
    let history: Vec<&str> = seen.events.iter().map(|e| e.summary.as_str()).collect();
    assert!(!history.iter().any(|h| h.contains("SECRET") || h.contains("retreat") || h.contains("Searched")), "{history:?}");
    // The list an administrator can ask for is the same.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let all = agents::list_tasks(&tx, &admin, true, 10).await.unwrap();
    assert_eq!(all[0].title, "Task");
    // The owner still sees all of it.
    let own = view(&env, &person(&f, "alice", &[]), id).await.unwrap();
    assert!(own.title.contains("SECRET") && own.events.iter().any(|e| e.summary.contains("Searched documents")));
}

#[tokio::test]
async fn text_from_a_file_the_owner_can_no_longer_open_is_not_shown_to_the_model_again() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);

    struct Revoking {
        admin: tokio_postgres::Client,
        seen: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl AgentModel for Revoking {
        async fn turn(&self, request: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
            self.seen.lock().unwrap().push(serde_json::to_string(request.messages).unwrap());
            Ok(match request.messages.len() {
                1 => reply(json!([{ "type": "tool_use", "id": "t1", "name": "search_documents", "input": { "query": "retreat budget" } }]), "tool_use"),
                3 => {
                    // Between turns, Alice loses access to the file.
                    self.admin.execute("UPDATE documents SET readable_by = '{}'", &[]).await.unwrap();
                    reply(json!([{ "type": "tool_use", "id": "t2", "name": "calculate", "input": { "expression": "1 + 1" } }]), "tool_use")
                }
                _ => reply(json!([{ "type": "text", "text": "done" }]), "end_turn"),
            })
        }
    }
    let model = Arc::new(Revoking { admin: common::connect(&env.config).await, seen: Default::default() });
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    agents::start_task(&tx, &alice, agent, "report").await.unwrap();
    tx.commit().await.unwrap();
    run_pass(&env.pool("t_worker", 8), model.clone(), None).await.unwrap();

    let seen = model.seen.lock().unwrap().clone();
    // While she could open it, the model saw the text. After she lost access, it did not.
    assert!(seen[1].contains("5000 dollars"), "{}", seen[1]);
    assert!(!seen[2].contains("5000 dollars") && seen[2].contains("you can no longer open"), "{}", seen[2]);
}

#[tokio::test]
async fn the_worker_records_who_did_what_without_recording_what_was_in_the_files() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let id = finished_task(&env, &f, agent, searcher()).await;
    let rows = env.admin.query("SELECT actor, action, detail::text FROM audit_events WHERE action = 'agent_task_finished'", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    let (actor, detail): (String, String) = (rows[0].get(0), rows[0].get(2));
    assert_eq!(actor, "alice");
    assert!(detail.contains(&id.to_string()) && detail.contains(&f.document.to_string()), "{detail}");
    assert!(!detail.contains("5000") && !detail.contains("SECRET"), "{detail}");
}
