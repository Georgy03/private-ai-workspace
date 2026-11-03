#![allow(dead_code)]
//! Test setup shared by the integration tests: a fresh database per test with the real
//! migrations, and logins for each application role.

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio::sync::Mutex;
use tokio_postgres::{Client, Config, NoTls};
use uuid::Uuid;
use workspace_cloud::{
    auth::TenantId,
    db::{self, NewDocument, TenantTx},
};

// Roles are shared by the whole server, so creating them must not run twice at once.
static SETUP: Mutex<()> = Mutex::const_new(());

pub struct Env {
    pub admin: Client,
    pub config: Config,
}

pub async fn connect(config: &Config) -> Client {
    let (client, connection) = config.connect(NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

impl Env {
    pub async fn new() -> Self {
        let url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is not set; run backend/scripts/test.sh");
        let base: Config = url.parse().expect("TEST_DATABASE_URL");
        let _setup = SETUP.lock().await;
        let server = connect(&base).await;
        let name = format!("test_{}", Uuid::new_v4().simple());
        server
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        let mut config = base.clone();
        config.dbname(&name);
        let mut admin = connect(&config).await;
        db::migrate(&mut admin).await.unwrap();
        for (login, role) in [
            ("t_api", "app_api"),
            ("t_worker", "app_worker"),
            ("t_prov", "app_provisioner"),
        ] {
            admin
                .batch_execute(&format!(
                    "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = '{login}') \
                     THEN CREATE ROLE {login} LOGIN PASSWORD 'test' IN ROLE {role}; END IF; END $$"
                ))
                .await
                .unwrap();
        }
        Self { admin, config }
    }

    pub async fn as_user(&self, user: &str) -> Client {
        let mut config = self.config.clone();
        config.user(user).password("test");
        connect(&config).await
    }

    pub async fn api(&self) -> Client {
        self.as_user("t_api").await
    }
    pub async fn worker(&self) -> Client {
        self.as_user("t_worker").await
    }
    pub async fn provisioner(&self) -> Client {
        self.as_user("t_prov").await
    }

    /// Creates a firm and gives it one connection and one file, through the real code paths.
    pub async fn firm(&self, name: &str, file: &str, text: &str, readable_by: &[&str]) -> Firm {
        let tenant = db::provision_tenant(&self.provisioner().await, name, None)
            .await
            .unwrap();
        let mut worker = self.worker().await;
        let tx = TenantTx::begin(&mut worker, tenant).await.unwrap();
        let connection = tx
            .add_connection("microsoft365", "secret/name")
            .await
            .unwrap();
        let document = tx
            .upsert_document(&NewDocument {
                connection_id: connection,
                external_id: format!("ext-{file}"),
                name: file.into(),
                path: format!("/{name}/{file}"),
                readable_by: readable_by.iter().map(|s| s.to_string()).collect(),
            })
            .await
            .unwrap();
        tx.add_chunk(document, 0, text).await.unwrap();
        tx.commit().await.unwrap();
        Firm {
            tenant,
            connection,
            document,
        }
    }
}

pub struct Firm {
    pub tenant: TenantId,
    pub connection: Uuid,
    pub document: Uuid,
}

impl Env {
    /// A pool of logins for one role, for code that needs more than one connection.
    pub fn pool(&self, user: &str, size: usize) -> Pool {
        let mut config = self.config.clone();
        config.user(user).password("test");
        let manager = Manager::from_config(
            config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        Pool::builder(manager)
            .max_size(size)
            .runtime(Runtime::Tokio1)
            .build()
            .unwrap()
    }

    /// A firm with one connection of the given kind and no documents.
    pub async fn firm_with_connection(
        &self,
        name: &str,
        kind: &str,
        config: serde_json::Value,
    ) -> (TenantId, Uuid) {
        let tenant = db::provision_tenant(&self.provisioner().await, name, None)
            .await
            .unwrap();
        let mut worker = self.worker().await;
        let tx = TenantTx::begin(&mut worker, tenant).await.unwrap();
        let connection = tx.add_connection(kind, "secret/name").await.unwrap();
        tx.commit().await.unwrap();
        self.admin
            .execute(
                "UPDATE connections SET config = $2 WHERE id = $1",
                &[&connection, &config],
            )
            .await
            .unwrap();
        (tenant, connection)
    }
}
