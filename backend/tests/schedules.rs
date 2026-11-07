//! Recurring tasks against a real Postgres with a scripted stand-in for the model. They cover what
//! matters most: a schedule runs as its owner with their access, an unchanged week is skipped, a
//! failing schedule stops, one person cannot touch another's, and a scheduled report waits for
//! its owner's approval.

mod common;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use common::{Env, Firm};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::{
    agent_runner::{AgentModel, ModelError, TurnReply, TurnRequest, run_pass},
    agents::{self, AgentSettings, AgentSpec},
    auth::Principal,
    db::TenantTx,
    schedules::{self, Cadence, Created, Launched, Review, SchedulePatch},
};

/// Searches once and reports what it found; remembers every conversation it was sent.
struct Searcher {
    seen: std::sync::Mutex<Vec<Vec<Value>>>,
}

#[async_trait]
impl AgentModel for Searcher {
    async fn turn(&self, request: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
        self.seen.lock().unwrap().push(request.messages.to_vec());
        let reply = |content: Value, stop: &str| TurnReply {
            content: content.as_array().unwrap().clone(),
            stop_reason: stop.into(),
            input_tokens: 1_000,
            output_tokens: 200,
            model: "claude-haiku-5-5".into(),
        };
        Ok(match request.messages.last().and_then(|m| m["content"][0]["content"].as_str()) {
            Some(found) => reply(json!([{ "type": "text", "text": format!("WEEKLY REPORT: {found}") }]), "end_turn"),
            None => reply(json!([{ "type": "tool_use", "id": "toolu_1", "name": "search_documents", "input": { "query": "retreat budget" } }]), "tool_use"),
        })
    }
}

struct Failing;

#[async_trait]
impl AgentModel for Failing {
    async fn turn(&self, _: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
        Err(ModelError("the model is down".into()))
    }
}

fn searcher() -> Arc<Searcher> {
    Arc::new(Searcher { seen: Default::default() })
}

fn person(firm: &Firm, user: &str, groups: &[&str]) -> Principal {
    Principal { tenant: firm.tenant, user: user.into(), groups: groups.iter().map(|g| g.to_string()).collect(), email: None }
}

fn friday_9() -> Cadence {
    Cadence { kind: "weekly".into(), weekday: 4, day: 1, minutes: 540, offset: 0 }
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

async fn schedule(env: &Env, owner: &Principal, agent: Uuid, budget: Option<i64>) -> Uuid {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, owner.tenant).await.unwrap();
    let id = match schedules::create(&tx, owner, agent, "Weekly report", "Report on the retreat budget.", &friday_9(), budget).await.unwrap() {
        Created::Made(id) => id,
        _ => panic!("not created"),
    };
    tx.commit().await.unwrap();
    id
}

async fn make_due(env: &Env) {
    env.admin.execute("UPDATE agent_schedule_clock SET next_run_at = now() - interval '1 minute', claimed_until = NULL", &[]).await.unwrap();
}

async fn due(env: &Env) -> usize {
    schedules::due_pass(&env.pool("t_worker", 4)).await.unwrap()
}

async fn work(env: &Env, model: Arc<dyn AgentModel>) -> usize {
    run_pass(&env.pool("t_worker", 8), model, None).await.unwrap()
}

async fn tasks_of(env: &Env, schedule: Uuid) -> Vec<(Uuid, String, Option<String>)> {
    env.admin
        .query("SELECT id, status, review_status FROM agent_tasks WHERE schedule_id = $1 ORDER BY created_at", &[&schedule])
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect()
}

async fn views(env: &Env, owner: &Principal) -> Vec<schedules::ScheduleView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, owner.tenant).await.unwrap();
    let list = schedules::list(&tx, owner, None).await.unwrap();
    tx.commit().await.unwrap();
    list
}

#[tokio::test]
async fn a_due_schedule_runs_as_its_owner_waits_for_approval_and_compares_with_last_time() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = schedule(&env, &alice, agent, None).await;

    // Not due yet: nothing happens.
    assert_eq!(due(&env).await, 0);
    make_due(&env).await;
    assert_eq!(due(&env).await, 1);
    let first = tasks_of(&env, id).await;
    assert_eq!(first.len(), 1);
    let who: String = env.admin.query_one("SELECT requested_by FROM agent_tasks WHERE id = $1", &[&first[0].0]).await.unwrap().get(0);
    assert_eq!(who, "alice");

    let model = searcher();
    work(&env, model.clone()).await;
    let done = tasks_of(&env, id).await;
    assert_eq!((done[0].1.as_str(), done[0].2.as_deref()), ("done", Some("pending")));
    // It reads with Alice's access: she may open the file, so the report has its figure.
    let result: String = env.admin.query_one("SELECT result FROM agent_tasks WHERE id = $1", &[&first[0].0]).await.unwrap().get(0);
    assert!(result.contains("5000 dollars"), "{result}");

    // Next time due: nothing the last report used has changed, so no new report and a clear note.
    make_due(&env).await;
    due(&env).await;
    assert_eq!(tasks_of(&env, id).await.len(), 1);
    let note = views(&env, &alice).await.remove(0).last_note.unwrap();
    assert!(note.contains("nothing the last report used has changed"), "{note}");

    // The file changes, so the next run goes ahead and is given the last report to compare with.
    env.admin.execute("UPDATE documents SET content_hash = 'changed'", &[]).await.unwrap();
    make_due(&env).await;
    due(&env).await;
    let both = tasks_of(&env, id).await;
    assert_eq!(both.len(), 2);
    let model = searcher();
    work(&env, model.clone()).await;
    let first_message = serde_json::to_string(&model.seen.lock().unwrap()[0][0]).unwrap();
    assert!(first_message.contains("The previous report is below") && first_message.contains("WEEKLY REPORT"), "{first_message}");
    let previous: Uuid = env.admin.query_one("SELECT previous_task_id FROM agent_tasks WHERE id = $1", &[&both[1].0]).await.unwrap().get(0);
    assert_eq!(previous, both[0].0);

    // Both reports are waiting in the owner's inbox, and no one else's.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert_eq!(agents::inbox(&tx, &alice).await.unwrap().len(), 2);
    assert!(agents::inbox(&tx, &person(&f, "bob", &[])).await.unwrap().is_empty());
    assert!(agents::inbox(&tx, &person(&f, "admin", &["firm_admin"])).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_report_can_be_approved_edited_or_rejected_only_by_its_owner_and_only_once() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = schedule(&env, &alice, agent, None).await;
    make_due(&env).await;
    due(&env).await;
    work(&env, searcher()).await;
    let task = tasks_of(&env, id).await[0].0;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let bob = person(&f, "bob", &[]);
    let admin = person(&f, "admin", &["firm_admin"]);
    assert!(!schedules::review(&tx, &bob, task, Review::Approve, None).await.unwrap());
    assert!(!schedules::review(&tx, &admin, task, Review::Approve, None).await.unwrap());
    assert!(schedules::review(&tx, &alice, task, Review::Approve, Some("My corrected report")).await.unwrap());
    // Decided: it cannot be decided again.
    assert!(!schedules::review(&tx, &alice, task, Review::Reject, None).await.unwrap());
    let seen = agents::get_task(&tx, &alice, task).await.unwrap().unwrap();
    assert_eq!(seen.review_status.as_deref(), Some("approved"));
    assert_eq!(seen.edited_result.as_deref(), Some("My corrected report"));
    assert!(seen.result.unwrap().contains("WEEKLY REPORT"), "the original is kept");
    assert!(agents::inbox(&tx, &alice).await.unwrap().is_empty());
    // An administrator sees neither the report nor the edit.
    let admin_view = agents::get_task(&tx, &admin, task).await.unwrap().unwrap();
    assert!(admin_view.edited_result.is_none() && admin_view.review_status.is_none());
}

#[tokio::test]
async fn schedules_belong_to_one_person() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let bob = person(&f, "bob", &[]);
    let admin = person(&f, "admin", &["firm_admin"]);
    let id = schedule(&env, &alice, agent, None).await;

    assert!(views(&env, &bob).await.is_empty());
    assert!(views(&env, &admin).await.is_empty(), "not even an administrator sees another person's schedules");
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let patch = SchedulePatch { name: Some("Mine now".into()), ..Default::default() };
    assert!(!schedules::update(&tx, &bob, id, &patch).await.unwrap());
    assert!(!schedules::remove(&tx, &admin, id).await.unwrap());
    assert!(schedules::run_now(&tx, &bob, id).await.unwrap().is_none());
    assert!(schedules::update(&tx, &alice, id, &patch).await.unwrap());
    tx.commit().await.unwrap();
    assert_eq!(views(&env, &alice).await[0].name, "Mine now");

    // Another firm sees nothing.
    let other = env.firm("Firm B", "b.txt", "text", &["user:zed"]).await;
    assert!(views(&env, &person(&other, "alice", &[])).await.is_empty());
}

#[tokio::test]
async fn a_schedule_that_keeps_failing_pauses_and_can_be_turned_back_on() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = schedule(&env, &alice, agent, None).await;
    for _ in 0..3 {
        make_due(&env).await;
        due(&env).await;
        work(&env, Arc::new(Failing)).await;
    }
    let paused = views(&env, &alice).await.remove(0);
    assert_eq!(paused.status, "paused");
    assert!(paused.pause_reason.unwrap().contains("3 failed runs"));
    assert!(paused.next_run_epoch.is_none());
    // Paused means paused: nothing is due even if time passes.
    make_due(&env).await;
    assert_eq!(due(&env).await, 0);

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    schedules::update(&tx, &alice, id, &SchedulePatch { status: Some("active".into()), ..Default::default() }).await.unwrap();
    tx.commit().await.unwrap();
    let back = views(&env, &alice).await.remove(0);
    assert_eq!((back.status.as_str(), back.pause_reason), ("active", None));
    assert!(back.next_run_epoch.is_some());
}

#[tokio::test]
async fn an_owner_who_has_not_been_seen_for_two_weeks_stops_the_schedule_and_coming_back_keeps_it_running() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let id = schedule(&env, &alice, agent, None).await;

    env.admin.execute("UPDATE agent_schedules SET identity_refreshed_at = now() - interval '20 days'", &[]).await.unwrap();
    make_due(&env).await;
    due(&env).await;
    assert!(tasks_of(&env, id).await.is_empty());
    let stopped = views(&env, &alice).await.remove(0);
    assert_eq!(stopped.status, "paused");
    assert!(stopped.pause_reason.unwrap().contains("not been seen"));

    // A second schedule whose owner opens the app in time keeps going: listing refreshes the access
    // it runs with.
    let kept = schedule(&env, &alice, agent, None).await;
    env.admin.execute("UPDATE agent_schedules SET identity_refreshed_at = now() - interval '13 days' WHERE id = $1", &[&kept]).await.unwrap();
    views(&env, &alice).await;
    let age: f64 = env.admin.query_one("SELECT EXTRACT(EPOCH FROM now() - identity_refreshed_at)::float8 FROM agent_schedules WHERE id = $1", &[&kept]).await.unwrap().get(0);
    assert!(age < 60.0, "{age}");
}

#[tokio::test]
async fn a_schedule_pauses_when_the_agent_goes_away_or_the_owner_loses_access_to_it() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    let a = schedule(&env, &alice, agent, None).await;
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    agents::update_agent(&tx, agent, "admin", None, &AgentSettings { audience: Some(vec!["user:bob".into()]), ..Default::default() }, "").await.unwrap();
    tx.commit().await.unwrap();
    make_due(&env).await;
    due(&env).await;
    let v = views(&env, &alice).await.remove(0);
    assert_eq!(v.status, "paused");
    assert!(v.pause_reason.unwrap().contains("no longer have access"));
    assert!(tasks_of(&env, a).await.is_empty());
}

#[tokio::test]
async fn budgets_and_run_now() {
    let env = Env::new().await;
    let (f, agent) = setup(&env).await;
    let alice = person(&f, "alice", &[]);
    // A budget so small that one report uses it all.
    let id = schedule(&env, &alice, agent, Some(1)).await;
    make_due(&env).await;
    due(&env).await;
    work(&env, searcher()).await;
    assert_eq!(tasks_of(&env, id).await.len(), 1);
    env.admin.execute("UPDATE documents SET content_hash = 'changed again'", &[]).await.unwrap();
    make_due(&env).await;
    due(&env).await;
    assert_eq!(tasks_of(&env, id).await.len(), 1, "over budget: not run");
    assert!(views(&env, &alice).await.remove(0).last_note.unwrap().contains("monthly budget"));

    // "Run now" is for the owner, ignores the unchanged-files shortcut, and still respects the budget.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(matches!(schedules::run_now(&tx, &alice, id).await.unwrap(), Some(Launched::Skipped(_))));
    schedules::update(&tx, &alice, id, &SchedulePatch { monthly_budget_micros: Some(None), ..Default::default() }).await.unwrap();
    assert!(matches!(schedules::run_now(&tx, &alice, id).await.unwrap(), Some(Launched::Started(_))));
    tx.commit().await.unwrap();
    assert_eq!(tasks_of(&env, id).await.len(), 2);
    let _ = Duration::from_secs(0);
}
