//! QuickBooks Online through the Accounting API. Everything this connector does is a read.
//!
//! Every customer, vendor, account, invoice, bill, payment, expense and journal entry becomes one
//! searchable item: a plain description of the record. The profit and loss and the balance sheet
//! for this and last fiscal year are kept as items too. A first listing reads every record; after
//! that only what changed is read, through QuickBooks' change feed, which also reports deletions.
//!
//! Only the person who connected the company can find what it returns.

use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Access, Api, Change, ChangePage, Connector, ConnectorError, RemoteFile, TokenSource, with_extra_readers};

/// The records kept, and what to call a group of them.
const ENTITIES: &[(&str, &str)] = &[
    ("Customer", "Customers"),
    ("Vendor", "Vendors"),
    ("Account", "Chart of accounts"),
    ("Invoice", "Invoices"),
    ("Bill", "Bills"),
    ("Payment", "Customer payments"),
    ("Purchase", "Expenses"),
    ("JournalEntry", "Journal entries"),
];
const REPORTS: &[(&str, &str)] = &[("ProfitAndLoss", "Profit and loss"), ("BalanceSheet", "Balance sheet")];
const PERIODS: &[(&str, &str)] = &[("This Fiscal Year", "this fiscal year"), ("Last Fiscal Year", "last fiscal year")];
const PAGE: usize = 100;
/// QuickBooks returns at most this many of one kind per change-feed call. At that size some may
/// have been left out, so the listing is started again rather than trusted.
const CDC_LIMIT: usize = 1_000;
/// The change feed is asked from a little before the last run, so a change made while a run was in
/// progress is not missed. Unchanged records are recognised by their version and not read again.
const OVERLAP_SECS: i64 = 3600;
const MAX_REMEMBERED: usize = 2_000;
const MINOR_VERSION: &str = "75";

pub struct QuickbooksConnector {
    api: Api,
    base: String,
    realm: String,
    company: String,
    always_readable_by: Vec<String>,
    /// Text built while listing, so reading an item needs no further call.
    remembered: Mutex<HashMap<String, String>>,
}

impl QuickbooksConnector {
    /// `realm` is the company's ID, which QuickBooks gives at sign-in. It goes into addresses, so
    /// only what a company ID looks like is accepted.
    pub fn new(tokens: Box<dyn TokenSource>, api_base: &str, realm: &str, company: &str) -> Result<Self, ConnectorError> {
        if !safe_realm(realm) {
            return Err(ConnectorError::Other("the QuickBooks company ID is not valid".into()));
        }
        Ok(Self {
            api: Api::new(tokens)?.asking_for_json()?,
            base: api_base.trim_end_matches('/').to_string(),
            realm: realm.to_string(),
            company: company.to_string(),
            always_readable_by: Vec::new(),
            remembered: Mutex::new(HashMap::new()),
        })
    }

    pub fn always_readable_by(mut self, readers: Vec<String>) -> Self {
        self.always_readable_by = readers;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v3/company/{}/{path}", self.base, self.realm)
    }
}

pub fn safe_realm(realm: &str) -> bool {
    !realm.is_empty() && realm.len() <= 20 && realm.chars().all(|c| c.is_ascii_digit())
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn iso(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0).map_or_else(|| "2000-01-01T00:00:00Z".to_string(), |d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn epoch(value: &Value) -> i64 {
    value.as_str().and_then(|text| DateTime::parse_from_rfc3339(text).ok()).map_or(0, |time| time.timestamp())
}

// ---- Describing a record in words ----

fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn money(value: &Value, currency: &Value) -> Option<String> {
    let amount = value.as_f64()?;
    let code = currency["value"].as_str().unwrap_or("");
    Some(format!("{amount:.2}{}", if code.is_empty() { String::new() } else { format!(" {code}") }))
}

fn name_of(reference: &Value) -> Option<String> {
    text_of(&reference["name"])
}

/// Adds "Label: value" when there is a value.
fn field(out: &mut String, label: &str, value: Option<String>) {
    if let Some(value) = value {
        out.push_str(&format!("{label}: {value}\n"));
    }
}

fn lines_text(record: &Value, currency: &Value) -> String {
    let mut out = String::new();
    for line in record["Line"].as_array().into_iter().flatten() {
        if line["DetailType"] == "SubTotalLineDetail" {
            continue;
        }
        let detail = ["SalesItemLineDetail", "AccountBasedExpenseLineDetail", "ItemBasedExpenseLineDetail", "JournalEntryLineDetail", "DiscountLineDetail"]
            .iter()
            .map(|key| &line[*key])
            .find(|detail| detail.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        let what = text_of(&line["Description"])
            .or_else(|| name_of(&detail["ItemRef"]))
            .or_else(|| name_of(&detail["AccountRef"]))
            .unwrap_or_else(|| "(no description)".into());
        let mut row = format!("- {what}");
        if let Some(account) = name_of(&detail["AccountRef"]).filter(|a| a != &what) {
            row.push_str(&format!(" [account: {account}]"));
        }
        if let Some(posting) = text_of(&detail["PostingType"]) {
            row.push_str(&format!(" ({posting})"));
        }
        if let (Some(qty), Some(price)) = (detail["Qty"].as_f64(), detail["UnitPrice"].as_f64()) {
            row.push_str(&format!(": {qty} x {price:.2}"));
        }
        if let Some(amount) = money(&line["Amount"], currency) {
            row.push_str(&format!(" = {amount}"));
        }
        out.push_str(&row);
        out.push('\n');
    }
    if out.is_empty() { out } else { format!("Lines:\n{out}") }
}

/// A plain description of one record, which is what is searched.
pub(crate) fn describe(entity: &str, record: &Value, company: &str) -> String {
    let currency = &record["CurrencyRef"];
    let mut out = String::new();
    let title = title_of(entity, record);
    out.push_str(&format!("QuickBooks {title}\n"));
    field(&mut out, "Company", text_of(&json!(company)));
    match entity {
        "Customer" | "Vendor" => {
            field(&mut out, "Company name", text_of(&record["CompanyName"]));
            field(&mut out, "Email", text_of(&record["PrimaryEmailAddr"]["Address"]));
            field(&mut out, "Phone", text_of(&record["PrimaryPhone"]["FreeFormNumber"]));
            field(&mut out, "Balance owing", money(&record["Balance"], currency));
            field(&mut out, "Active", record["Active"].as_bool().map(|a| if a { "yes" } else { "no" }.to_string()));
        }
        "Account" => {
            field(&mut out, "Full name", text_of(&record["FullyQualifiedName"]));
            field(&mut out, "Type", text_of(&record["AccountType"]));
            field(&mut out, "Sub-type", text_of(&record["AccountSubType"]));
            field(&mut out, "Current balance", money(&record["CurrentBalance"], currency));
            field(&mut out, "Active", record["Active"].as_bool().map(|a| if a { "yes" } else { "no" }.to_string()));
        }
        _ => {
            field(&mut out, "Number", text_of(&record["DocNumber"]));
            field(&mut out, "Date", text_of(&record["TxnDate"]));
            field(&mut out, "Due date", text_of(&record["DueDate"]));
            field(&mut out, "Customer", name_of(&record["CustomerRef"]));
            field(&mut out, "Vendor", name_of(&record["VendorRef"]).or_else(|| name_of(&record["EntityRef"])));
            field(&mut out, "Paid from", name_of(&record["AccountRef"]).filter(|_| entity == "Purchase"));
            field(&mut out, "Total", money(&record["TotalAmt"], currency));
            if matches!(entity, "Invoice" | "Bill") {
                let balance = record["Balance"].as_f64();
                field(&mut out, "Balance still owing", money(&record["Balance"], currency));
                if let Some(balance) = balance {
                    out.push_str(&format!("Status: {}\n", if balance == 0.0 { "paid in full" } else { "not yet paid in full" }));
                }
            }
            field(&mut out, "Memo", text_of(&record["PrivateNote"]).or_else(|| text_of(&record["CustomerMemo"]["value"])));
            out.push_str(&lines_text(record, currency));
        }
    }
    field(&mut out, "Last changed", text_of(&record["MetaData"]["LastUpdatedTime"]));
    field(&mut out, "QuickBooks ID", text_of(&record["Id"]));
    out
}

fn title_of(entity: &str, record: &Value) -> String {
    let number = text_of(&record["DocNumber"]).map(|n| format!(" {n}")).unwrap_or_default();
    let party = name_of(&record["CustomerRef"]).or_else(|| name_of(&record["VendorRef"])).or_else(|| name_of(&record["EntityRef"]));
    let with = party.map(|p| format!(" – {p}")).unwrap_or_default();
    match entity {
        "Customer" => format!("customer: {}", text_of(&record["DisplayName"]).unwrap_or_default()),
        "Vendor" => format!("vendor: {}", text_of(&record["DisplayName"]).unwrap_or_default()),
        "Account" => format!("account: {}", text_of(&record["Name"]).unwrap_or_default()),
        "Invoice" => format!("invoice{number}{with}"),
        "Bill" => format!("bill{number}{with}"),
        "Payment" => format!("customer payment{number}{with}"),
        "Purchase" => format!("expense{number}{with}"),
        "JournalEntry" => format!("journal entry{number}"),
        other => other.to_string(),
    }
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().collect::<String>() + chars.as_str())
}

fn group_label(entity: &str) -> &'static str {
    ENTITIES.iter().find(|(name, _)| *name == entity).map_or("Records", |(_, label)| *label)
}

// ---- Reports ----

fn render_rows(rows: &Value, depth: usize, out: &mut String) {
    for row in rows["Row"].as_array().into_iter().flatten() {
        let cells = |data: &Value| -> String {
            data.as_array().map(|list| list.iter().filter_map(|cell| text_of(&cell["value"])).collect::<Vec<_>>().join(" | ")).unwrap_or_default()
        };
        let indent = "  ".repeat(depth);
        if row["type"] == "Section" || row["Header"].is_object() {
            let header = cells(&row["Header"]["ColData"]);
            if !header.is_empty() {
                out.push_str(&format!("{indent}{header}\n"));
            }
            render_rows(&row["Rows"], depth + 1, out);
            let summary = cells(&row["Summary"]["ColData"]);
            if !summary.is_empty() {
                out.push_str(&format!("{indent}{summary}\n"));
            }
        } else {
            let line = cells(&row["ColData"]);
            if !line.is_empty() {
                out.push_str(&format!("{indent}{line}\n"));
            }
        }
    }
}

pub(crate) fn report_text(report: &Value, company: &str, label: &str) -> String {
    let header = &report["Header"];
    let mut out = format!("QuickBooks {} for {label}\n", text_of(&header["ReportName"]).unwrap_or_else(|| "report".into()));
    field(&mut out, "Company", text_of(&json!(company)));
    if let (Some(from), Some(to)) = (text_of(&header["StartPeriod"]), text_of(&header["EndPeriod"])) {
        out.push_str(&format!("Period: {from} to {to}\n"));
    }
    field(&mut out, "Basis", text_of(&header["ReportBasis"]));
    field(&mut out, "Currency", text_of(&header["Currency"]));
    let columns: Vec<String> = report["Columns"]["Column"].as_array().map(|list| list.iter().filter_map(|c| text_of(&c["ColTitle"])).collect()).unwrap_or_default();
    if !columns.is_empty() {
        out.push_str(&format!("Columns: {}\n", columns.join(" | ")));
    }
    render_rows(&report["Rows"], 0, &mut out);
    out
}

fn fingerprint(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().take(12).map(|byte| format!("{byte:02x}")).collect()
}

// ---- Listing ----

#[derive(Debug, PartialEq)]
enum Position {
    /// Reading every record of one kind, a page at a time. After the last kind come the reports.
    Full { entity: usize, start: usize, run: i64 },
    /// Reading what changed since a time.
    Changes { since: String },
}

fn parse(cursor: Option<&str>) -> Result<Position, ConnectorError> {
    let Some(cursor) = cursor else { return Ok(Position::Full { entity: 0, start: 1, run: now() }) };
    let value: Value = serde_json::from_str(cursor).map_err(|_| ConnectorError::CursorExpired)?;
    match value["mode"].as_str() {
        Some("full") => Ok(Position::Full {
            entity: value["entity"].as_u64().ok_or(ConnectorError::CursorExpired)? as usize,
            start: value["start"].as_u64().ok_or(ConnectorError::CursorExpired)? as usize,
            run: value["run"].as_i64().ok_or(ConnectorError::CursorExpired)?,
        }),
        Some("changes") => Ok(Position::Changes { since: value["since"].as_str().ok_or(ConnectorError::CursorExpired)?.to_string() }),
        _ => Err(ConnectorError::CursorExpired),
    }
}

impl QuickbooksConnector {
    fn remember(&self, id: &str, text: String) {
        let mut remembered = self.remembered.lock().unwrap();
        if remembered.len() >= MAX_REMEMBERED {
            remembered.clear();
        }
        remembered.insert(id.to_string(), text);
    }

    fn change_for(&self, entity: &str, record: &Value) -> Option<Change> {
        let id = text_of(&record["Id"]).filter(|id| safe_id(id))?;
        let external_id = format!("{entity}:{id}");
        if record["status"] == "Deleted" {
            return Some(Change::Delete(external_id));
        }
        let text = describe(entity, record, &self.company);
        let title = title_of(entity, record);
        let modified = &record["MetaData"]["LastUpdatedTime"];
        let version = format!("{}|{}", text_of(&record["SyncToken"]).unwrap_or_default(), text_of(modified).unwrap_or_default());
        let size = text.len() as u64;
        self.remember(&external_id, text);
        Some(Change::Upsert(RemoteFile {
            external_id,
            name: format!("{} (QuickBooks)", capitalised(&title)),
            path: format!("QuickBooks/{}/{}", self.company, group_label(entity)),
            mime_type: "text/plain".into(),
            size,
            modified_at: epoch(modified),
            version,
            // Nobody but the person who connected the company: the connection adds them.
            access: Some(Access::Known(Vec::new())),
            web_url: None,
        }))
    }

    /// The reports for this and last fiscal year. One that cannot be read (a company without that
    /// report) is left out; a sign-in problem is not.
    async fn report_changes(&self) -> Result<Vec<Change>, ConnectorError> {
        let mut changes = Vec::new();
        for (report, report_label) in REPORTS {
            for (macro_name, period_label) in PERIODS {
                let id = format!("Report:{report}:{}", macro_name.replace(' ', ""));
                let fetched = self.api.get_json(&self.url(&format!("reports/{report}")), &[("date_macro", macro_name), ("minorversion", MINOR_VERSION)]).await;
                let body = match fetched {
                    Ok(body) => body,
                    Err(ConnectorError::Unauthorized) => return Err(ConnectorError::Unauthorized),
                    Err(ConnectorError::RateLimited(wait)) => return Err(ConnectorError::RateLimited(wait)),
                    Err(_) => continue,
                };
                let text = report_text(&body, &self.company, period_label);
                let version = fingerprint(&text);
                let size = text.len() as u64;
                self.remember(&id, text);
                changes.push(Change::Upsert(RemoteFile {
                    external_id: id,
                    name: format!("{report_label}, {period_label} (QuickBooks)"),
                    path: format!("QuickBooks/{}/Reports", self.company),
                    mime_type: "text/plain".into(),
                    size,
                    modified_at: now(),
                    version,
                    access: Some(Access::Known(Vec::new())),
                    web_url: None,
                }));
            }
        }
        Ok(changes)
    }

    fn finished(&self, changes: Vec<Change>, run: i64) -> ChangePage {
        ChangePage { changes, cursor: json!({ "mode": "changes", "since": iso(run - OVERLAP_SECS) }).to_string(), has_more: false }
    }
}

#[async_trait]
impl Connector for QuickbooksConnector {
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError> {
        match parse(cursor)? {
            Position::Full { entity, start, run } if entity < ENTITIES.len() => {
                let name = ENTITIES[entity].0;
                let query = format!("select * from {name} STARTPOSITION {start} MAXRESULTS {PAGE}");
                let body = self.api.get_json(&self.url("query"), &[("query", query.as_str()), ("minorversion", MINOR_VERSION)]).await?;
                let records = body["QueryResponse"][name].as_array().cloned().unwrap_or_default();
                let changes = records.iter().filter_map(|record| self.change_for(name, record)).collect();
                let (entity, start) = if records.len() >= PAGE { (entity, start + PAGE) } else { (entity + 1, 1) };
                Ok(ChangePage { changes, cursor: json!({ "mode": "full", "entity": entity, "start": start, "run": run }).to_string(), has_more: true })
            }
            Position::Full { run, .. } => Ok(self.finished(self.report_changes().await?, run)),
            Position::Changes { since } => {
                let run = now();
                let names = ENTITIES.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(",");
                let body = match self.api.get_json(&self.url("cdc"), &[("entities", names.as_str()), ("changedSince", since.as_str()), ("minorversion", MINOR_VERSION)]).await {
                    Ok(body) => body,
                    // The change feed only reaches back about 30 days; before that, start again.
                    Err(ConnectorError::BadRequest) => return Err(ConnectorError::CursorExpired),
                    Err(other) => return Err(other),
                };
                let mut changes = Vec::new();
                for group in body["CDCResponse"][0]["QueryResponse"].as_array().into_iter().flatten() {
                    for (name, _) in ENTITIES {
                        let records = group[*name].as_array().cloned().unwrap_or_default();
                        if records.len() >= CDC_LIMIT {
                            return Err(ConnectorError::CursorExpired);
                        }
                        changes.extend(records.iter().filter_map(|record| self.change_for(name, record)));
                    }
                }
                changes.extend(self.report_changes().await?);
                Ok(self.finished(changes, run))
            }
        }
    }

    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        Ok(with_extra_readers(file.access.clone().unwrap_or(Access::Unknown), &self.always_readable_by))
    }

    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError> {
        if let Some(text) = self.remembered.lock().unwrap().remove(&file.external_id) {
            return Ok(Some(text));
        }
        // Not remembered (listed in an earlier pass): ask again.
        let mut parts = file.external_id.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("Report"), Some(report), Some(period)) => {
                let Some((macro_name, label)) = PERIODS.iter().find(|(name, _)| name.replace(' ', "") == period) else { return Ok(None) };
                if !REPORTS.iter().any(|(name, _)| *name == report) {
                    return Ok(None);
                }
                let body = self.api.get_json(&self.url(&format!("reports/{report}")), &[("date_macro", macro_name), ("minorversion", MINOR_VERSION)]).await?;
                Ok(Some(report_text(&body, &self.company, label)))
            }
            (Some(entity), Some(id), None) if safe_id(id) && ENTITIES.iter().any(|(name, _)| *name == entity) => {
                let body = self.api.get_json(&self.url(&format!("{}/{id}", entity.to_lowercase())), &[("minorversion", MINOR_VERSION)]).await?;
                Ok(Some(describe(entity, &body[entity], &self.company)))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoice() -> Value {
        json!({
            "Id": "145", "SyncToken": "2", "DocNumber": "1037", "TxnDate": "2026-03-01", "DueDate": "2026-03-31",
            "CustomerRef": { "value": "9", "name": "Whitfield Dental" }, "CurrencyRef": { "value": "CAD" },
            "TotalAmt": 1250.0, "Balance": 250.0, "PrivateNote": "Retainer top-up",
            "MetaData": { "LastUpdatedTime": "2026-03-02T09:00:00-08:00" },
            "Line": [
                { "Amount": 1250.0, "Description": "Bookkeeping, February", "DetailType": "SalesItemLineDetail",
                  "SalesItemLineDetail": { "ItemRef": { "name": "Services" }, "Qty": 5.0, "UnitPrice": 250.0 } },
                { "Amount": 1250.0, "DetailType": "SubTotalLineDetail", "SubTotalLineDetail": {} },
            ],
        })
    }

    #[test]
    fn an_invoice_is_described_with_who_what_how_much_and_whether_it_is_paid() {
        let text = describe("Invoice", &invoice(), "Firm Books Ltd");
        assert!(text.starts_with("QuickBooks invoice 1037 – Whitfield Dental\n"), "{text}");
        for expected in ["Company: Firm Books Ltd", "Total: 1250.00 CAD", "Balance still owing: 250.00 CAD", "Status: not yet paid in full", "- Bookkeeping, February: 5 x 250.00 = 1250.00 CAD", "QuickBooks ID: 145"] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
        assert!(!text.contains("SubTotal"), "the subtotal row is not a line item");
        let mut paid = invoice();
        paid["Balance"] = json!(0.0);
        assert!(describe("Invoice", &paid, "x").contains("Status: paid in full"));
    }

    #[test]
    fn customers_and_accounts_are_described_too() {
        let customer = json!({ "Id": "9", "DisplayName": "Whitfield Dental", "PrimaryEmailAddr": { "Address": "office@whitfield.example" }, "Balance": 250.0, "Active": true });
        let text = describe("Customer", &customer, "Firm Books Ltd");
        assert!(text.contains("customer: Whitfield Dental") && text.contains("Email: office@whitfield.example") && text.contains("Active: yes"), "{text}");
        let account = json!({ "Id": "3", "Name": "Meals", "FullyQualifiedName": "Expenses:Meals", "AccountType": "Expense", "CurrentBalance": 420.5 });
        assert!(describe("Account", &account, "x").contains("Current balance: 420.50"));
    }

    #[test]
    fn a_record_changes_version_when_it_is_edited_and_a_deleted_one_is_removed() {
        let connector = QuickbooksConnector::new(Box::new(super::super::StaticToken("t".into())), "https://sandbox-quickbooks.api.intuit.com", "123145", "Firm Books Ltd").unwrap();
        let Some(Change::Upsert(first)) = connector.change_for("Invoice", &invoice()) else { panic!() };
        assert_eq!(first.external_id, "Invoice:145");
        assert_eq!(first.access, Some(Access::Known(Vec::new())), "only the person who connected the company may read it");
        let mut edited = invoice();
        edited["SyncToken"] = json!("3");
        let Some(Change::Upsert(after)) = connector.change_for("Invoice", &edited) else { panic!() };
        assert_ne!(after.version, first.version);
        let deleted = json!({ "Id": "145", "domain": "QBO", "status": "Deleted" });
        assert!(matches!(connector.change_for("Invoice", &deleted), Some(Change::Delete(id)) if id == "Invoice:145"));
        // An ID that is not an ID is never used.
        assert!(connector.change_for("Invoice", &json!({ "Id": "../x" })).is_none());
    }

    #[test]
    fn the_company_id_is_checked_before_it_goes_into_an_address() {
        let tokens = || Box::new(super::super::StaticToken("t".into())) as Box<dyn TokenSource>;
        assert!(QuickbooksConnector::new(tokens(), "https://x", "9341454", "c").is_ok());
        for bad in ["", "12/../3", "12 3", "abc", "123456789012345678901"] {
            assert!(QuickbooksConnector::new(tokens(), "https://x", bad, "c").is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_report_is_written_out_row_by_row_with_its_totals() {
        let report = json!({
            "Header": { "ReportName": "ProfitAndLoss", "StartPeriod": "2026-01-01", "EndPeriod": "2026-12-31", "ReportBasis": "Accrual", "Currency": "CAD" },
            "Columns": { "Column": [{ "ColTitle": "" }, { "ColTitle": "Total" }] },
            "Rows": { "Row": [
                { "type": "Section", "Header": { "ColData": [{ "value": "Income" }, { "value": "" }] },
                  "Rows": { "Row": [{ "type": "Data", "ColData": [{ "value": "Services" }, { "value": "5000.00" }] }] },
                  "Summary": { "ColData": [{ "value": "Total Income" }, { "value": "5000.00" }] } },
            ] },
        });
        let text = report_text(&report, "Firm Books Ltd", "this fiscal year");
        assert!(text.contains("Period: 2026-01-01 to 2026-12-31") && text.contains("Basis: Accrual"), "{text}");
        assert!(text.contains("Income\n  Services | 5000.00\nTotal Income | 5000.00"), "{text}");
    }

    #[test]
    fn positions_round_trip_and_a_bad_one_means_start_again() {
        assert!(matches!(parse(None), Ok(Position::Full { entity: 0, start: 1, .. })));
        let full = json!({ "mode": "full", "entity": 3, "start": 101, "run": 5 }).to_string();
        assert_eq!(parse(Some(&full)).unwrap(), Position::Full { entity: 3, start: 101, run: 5 });
        let changes = json!({ "mode": "changes", "since": "2026-03-01T00:00:00Z" }).to_string();
        assert_eq!(parse(Some(&changes)).unwrap(), Position::Changes { since: "2026-03-01T00:00:00Z".into() });
        assert!(matches!(parse(Some("not json")), Err(ConnectorError::CursorExpired)));
        assert!(matches!(parse(Some(r#"{"mode":"other"}"#)), Err(ConnectorError::CursorExpired)));
    }
}
