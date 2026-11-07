//! Emails and meetings an agent prepares for a person to send or book themselves. Against a real
//! Postgres with a scripted stand-in for the model.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{Env, Firm};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::{
    actions::{self, Decided, Payload},
    agent_runner::{AgentModel, ModelError, TurnReply, TurnRequest, run_pass},
    agents::{self, AgentSettings, AgentSpec, Started},
    auth::Principal,
    db::TenantTx,
    retention,
};

fn reply(content: Value, stop: &str) -> TurnReply {
    TurnReply { content: content.as_array().unwrap().clone(), stop_reason: stop.into(), input_tokens: 1_000, output_tokens: 200, model: "claude-haiku-5-5".into() }
}

fn call(id: &str, name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
}

/// Makes each of its tool calls in turn, then says it is done.
struct Script {
    steps: Vec<Vec<Value>>,
    seen: std::sync::Mutex<Vec<String>>,
    /// The instructions and tools every call was given.
    given: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl AgentModel for Script {
    async fn turn(&self, request: TurnRequest<'_>) -> Result<TurnReply, ModelError> {
        self.seen.lock().unwrap().push(serde_json::to_string(request.messages).unwrap());
        self.given.lock().unwrap().push(format!("{} {}", request.system, serde_json::to_string(&request.tools).unwrap()));
        let step = request.messages.len() / 2;
        Ok(match self.steps.get(step) {
            Some(calls) => reply(Value::Array(calls.clone()), "tool_use"),
            None => reply(json!([{ "type": "text", "text": "I prepared it for you to review." }]), "end_turn"),
        })
    }
}

fn script(steps: Vec<Vec<Value>>) -> Arc<Script> {
    Arc::new(Script { steps, seen: Default::default(), given: Default::default() })
}

fn person(firm: &Firm, user: &str, groups: &[&str], email: Option<&str>) -> Principal {
    Principal { tenant: firm.tenant, user: user.into(), groups: groups.iter().map(|g| g.to_string()).collect(), email: email.map(str::to_string) }
}

async fn setup(env: &Env, tools: &[&str]) -> (Firm, Uuid) {
    let firm = env.firm("Firm A", "retreat.docx", "The retreat budget is 5000 dollars.", &["user:alice"]).await;
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let spec = AgentSpec {
        name: "Dana".into(),
        role: "Drafts".into(),
        description: "Drafts.".into(),
        icon: "search".into(),
        color: "blue".into(),
        instructions: "Draft what is asked.".into(),
        tools: tools.iter().map(|t| t.to_string()).collect(),
        model: "standard".into(),
        effort: "low".into(),
    };
    let agent = agents::create_agent(&tx, "admin", &spec, &AgentSettings::default()).await.unwrap();
    tx.commit().await.unwrap();
    (firm, agent)
}

async fn run_task(env: &Env, firm: &Firm, agent: Uuid, who: &Principal, model: Arc<dyn AgentModel>) -> Uuid {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, firm.tenant).await.unwrap();
    let Started::Queued(id) = agents::start_task(&tx, who, agent, "Write to the client about the budget").await.unwrap() else { panic!() };
    tx.commit().await.unwrap();
    run_pass(&env.pool("t_worker", 8), model, None).await.unwrap();
    id
}

async fn waiting(env: &Env, who: &Principal) -> Vec<actions::ActionView> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, who.tenant).await.unwrap();
    actions::waiting(&tx, who).await.unwrap()
}

fn email_call(id: &str, to: &str, subject: &str) -> Value {
    call(id, "prepare_email", json!({ "to": [to], "subject": subject, "body": "The budget is 5000 dollars.\n\nRegards" }))
}

#[tokio::test]
async fn an_agent_can_prepare_an_email_and_a_meeting_and_only_for_the_person_it_ran_for() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["prepare_email", "prepare_meeting"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    let model = script(vec![vec![
        email_call("t1", "Client@Other.example", "Retreat budget"),
        call("t2", "prepare_meeting", json!({ "title": "Budget review", "start": "2026-11-05T14:00", "end": "2026-11-05T15:00", "attendees": ["bob@firm.com"] })),
    ]]);
    let id = run_task(&env, &f, agent, &alice, model.clone()).await;

    let mine = waiting(&env, &alice).await;
    assert_eq!(mine.len(), 2);
    // Newest first: the meeting was prepared after the email.
    let Payload::Email { to, subject, .. } = &mine[1].payload else { panic!("{:?}", mine[1].payload) };
    assert_eq!((to.as_slice(), subject.as_str()), (&["client@other.example".to_string()][..], "Retreat budget"));
    // Someone outside Alice's own organisation is flagged, so she looks twice before sending.
    assert_eq!(mine[1].outside, ["client@other.example"]);
    assert!(mine[0].outside.is_empty());
    assert_eq!((mine[1].agent_name.as_str(), mine[1].status.as_str()), ("Dana", "proposed"));
    // The model was told nothing was sent.
    let seen = model.seen.lock().unwrap().join(" ");
    assert!(seen.contains("Nothing has been sent") && seen.contains("Nothing has been booked"), "{seen}");

    // Nobody else sees it: not another employee, not an administrator, not another firm.
    for other in [person(&f, "bob", &[], Some("bob@firm.com")), person(&f, "boss", &["firm_admin"], None)] {
        assert!(waiting(&env, &other).await.is_empty(), "{} saw it", other.user);
    }
    let elsewhere = env.firm("Firm B", "b.txt", "text", &["user:zed"]).await;
    assert!(waiting(&env, &person(&elsewhere, "alice", &[], None)).await.is_empty());
    // The activity history says that it happened, not what it said.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let task = agents::get_task(&tx, &alice, id).await.unwrap().unwrap();
    let history: Vec<&str> = task.events.iter().map(|e| e.summary.as_str()).collect();
    assert!(history.contains(&"Prepared an email for review") && history.contains(&"Prepared a meeting for review"), "{history:?}");
    // The audit log has the kind of thing and its task, never the text.
    let audit: String = env.admin.query_one("SELECT string_agg(action || detail::text, ' ') FROM audit_events WHERE action = 'agent_action_proposed'", &[]).await.unwrap().get(0);
    assert!(audit.contains("email") && audit.contains("meeting") && !audit.contains("5000") && !audit.contains("client@other"), "{audit}");
}

#[tokio::test]
async fn an_agent_without_the_tool_cannot_prepare_anything_and_bad_drafts_are_sent_back_to_be_fixed() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["search_documents"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    run_task(&env, &f, agent, &alice, script(vec![vec![email_call("t1", "a@b.com", "s")]])).await;
    assert!(waiting(&env, &alice).await.is_empty(), "the tool was never given to this agent");

    // An agent that has the tool, and makes mistakes, is told what is wrong and nothing is stored.
    let (f, agent) = setup(&env, &["prepare_email", "prepare_meeting"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    let model = script(vec![
        vec![
            email_call("t1", "not an address", "s"),
            call("t2", "prepare_email", json!({ "to": ["a@b.com\nBcc: x@y.com"], "subject": "s", "body": "b" })),
            call("t3", "prepare_meeting", json!({ "title": "x", "start": "2026-11-05T15:00", "end": "2026-11-05T14:00" })),
            call("t4", "prepare_meeting", json!({ "title": "x" })),
        ],
        vec![email_call("t5", "ok@firm.com", "Fixed")],
    ]);
    run_task(&env, &f, agent, &alice, model.clone()).await;
    let mine = waiting(&env, &alice).await;
    assert_eq!(mine.len(), 1, "only the corrected one");
    let seen = model.seen.lock().unwrap().join(" ");
    for reason in ["not valid", "must end after it starts", "wrong form"] {
        assert!(seen.contains(reason), "{reason}: {seen}");
    }
}

#[tokio::test]
async fn one_task_may_only_prepare_a_few_items() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["prepare_email"]).await;
    let alice = person(&f, "alice", &[], None);
    let calls: Vec<Value> = (0..8).map(|n| email_call(&format!("t{n}"), &format!("p{n}@firm.com"), "Hello")).collect();
    let model = script(vec![calls]);
    run_task(&env, &f, agent, &alice, model.clone()).await;
    assert_eq!(waiting(&env, &alice).await.len() as i64, actions::MAX_PER_TASK);
    assert!(model.seen.lock().unwrap().join(" ").contains("as many items as one task may"));
}

#[tokio::test]
async fn the_person_may_edit_then_approve_or_dismiss_and_only_they_decide_and_only_once() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["prepare_email", "prepare_meeting"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    run_task(&env, &f, agent, &alice, script(vec![vec![
        email_call("t1", "client@other.example", "Budget"),
        call("t2", "prepare_meeting", json!({ "title": "Review", "start": "2026-11-05T14:00", "end": "2026-11-05T15:00" })),
        email_call("t3", "x@y.example", "Skip this"),
    ]])).await;
    let items = waiting(&env, &alice).await;
    // Newest first.
    let (skip, meeting, email) = (items[0].id, items[1].id, items[2].id);

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let bob = person(&f, "bob", &[], None);
    let admin = person(&f, "boss", &["firm_admin"], None);
    // Not theirs to decide.
    for other in [&bob, &admin] {
        assert!(matches!(actions::approve(&tx, other, email, None).await.unwrap(), Decided::NotFound));
        assert!(!actions::dismiss(&tx, other, email).await.unwrap());
    }
    // An edit that cannot work is refused and the proposal stays as it was.
    let broken = Payload::Email { to: vec!["nobody".into()], cc: vec![], subject: "s".into(), body: "b".into() };
    assert!(matches!(actions::approve(&tx, &alice, email, Some(&broken)).await.unwrap(), Decided::Invalid(_)));
    let turned_into_a_meeting = Payload::Meeting { title: "t".into(), start: "2026-11-05T14:00".into(), end: "2026-11-05T15:00".into(), attendees: vec![], location: "".into(), notes: "".into() };
    assert!(matches!(actions::approve(&tx, &alice, email, Some(&turned_into_a_meeting)).await.unwrap(), Decided::Invalid(_)));
    // A good edit is what gets approved, and is marked as edited.
    let better = Payload::Email { to: vec!["Client@Other.example".into()], cc: vec!["me@firm.com".into()], subject: "Budget, revised".into(), body: "Short version.".into() };
    let Decided::Done(Payload::Email { subject, cc, .. }) = actions::approve(&tx, &alice, email, Some(&better)).await.unwrap() else { panic!() };
    assert_eq!((subject.as_str(), cc.as_slice()), ("Budget, revised", &["me@firm.com".to_string()][..]));
    // As written, with no change: not marked as edited.
    assert!(matches!(actions::approve(&tx, &alice, meeting, None).await.unwrap(), Decided::Done(_)));
    assert!(actions::dismiss(&tx, &alice, skip).await.unwrap());
    // Decided once, decided: it cannot be approved or dismissed again.
    assert!(matches!(actions::approve(&tx, &alice, email, None).await.unwrap(), Decided::NotFound));
    assert!(!actions::dismiss(&tx, &alice, meeting).await.unwrap());
    tx.commit().await.unwrap();
    assert!(waiting(&env, &alice).await.is_empty());
    let rows = env.admin.query("SELECT status, edited FROM agent_actions ORDER BY created_at", &[]).await.unwrap();
    let states: Vec<(String, bool)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    assert_eq!(states, [("approved".into(), true), ("approved".into(), false), ("dismissed".into(), false)]);
}

#[tokio::test]
async fn the_api_cannot_create_a_proposal_and_a_deleted_report_takes_its_proposals_with_it() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["prepare_email"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    let task = run_task(&env, &f, agent, &alice, script(vec![vec![email_call("t1", "a@b.example", "Hello")]])).await;

    // Only the agent's own worker can make one: the service people use cannot.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    let forged = tx.sql().execute(
        "INSERT INTO agent_actions (tenant_id, task_id, requested_by, kind, payload) VALUES (app_tenant(), $1, 'alice', 'email', '{}')",
        &[&task],
    ).await;
    assert!(forged.is_err(), "the API could create a proposal");
    drop(tx);

    // Deleting the report removes what it prepared, used or not.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, f.tenant).await.unwrap();
    assert!(retention::delete_now(&tx, &alice, task).await.unwrap());
    tx.commit().await.unwrap();
    retention::purge_pass(&env.pool("t_worker", 4)).await.unwrap();
    let left: i64 = env.admin.query_one("SELECT count(*) FROM agent_actions", &[]).await.unwrap().get(0);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn an_agent_is_told_to_just_do_what_the_request_already_covers_instead_of_searching_first() {
    let env = Env::new().await;
    let (f, agent) = setup(&env, &["search_documents", "prepare_email", "prepare_meeting"]).await;
    let alice = person(&f, "alice", &[], Some("alice@firm.com"));
    let model = script(vec![vec![email_call("t1", "friend@example.com", "Saturday")]]);
    run_task(&env, &f, agent, &alice, model.clone()).await;
    let given = model.given.lock().unwrap().join(" ");
    // Every agent is told that the person's own words are a source, and not to search for what it was given.
    assert!(given.contains("do the task directly and do not search first"), "{given}");
    // And the tools that prepare things say so too.
    assert!(given.contains("call it straight away without searching first"), "{given}");
}
