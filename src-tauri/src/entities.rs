//! Entity resolution: build clients/matters from folder names, and use them to find files that
//! never name the person in their file name.

use std::collections::HashMap;

use crate::{
    db::Database,
    error::AppResult,
    models::{Entity, SearchHit},
    state::make_id,
};

const GENERIC_FOLDERS: &[&str] = &[
    "archive",
    "archives",
    "audit",
    "backup",
    "billing",
    "clients",
    "contracts",
    "correspondence",
    "desktop",
    "documents",
    "downloads",
    "drafts",
    "email",
    "emails",
    "final",
    "general",
    "images",
    "invoices",
    "misc",
    "new folder",
    "old",
    "other",
    "pdf",
    "pdfs",
    "personal",
    "photos",
    "procedures",
    "reports",
    "scans",
    "shared",
    "signed",
    "tax",
    "taxes",
    "templates",
    "temp",
    "tmp",
    "untitled",
    "work",
];

/// Evidence weight added to a file whose folder path names the entity (the plan's "strong"
/// location evidence). A starting value to tune on the evaluation set.
const PATH_EVIDENCE: f64 = 5.0;

/// "Turnip, John" becomes the entity "John Turnip" with aliases "Turnip, John" and "Turnip".
/// Generic folder names ("Contracts", "2025") are not clients and are skipped.
pub fn entity_from_folder(folder: &str) -> Option<Entity> {
    let folder = folder.trim();
    if folder.chars().count() < 3
        || folder.starts_with('.')
        || !folder.chars().any(char::is_alphabetic)
        || GENERIC_FOLDERS.contains(&folder.to_lowercase().as_str())
    {
        return None;
    }
    let mut parts = folder.splitn(2, ',').map(str::trim);
    let (name, aliases) = match (parts.next(), parts.next()) {
        (Some(last), Some(first))
            if !last.is_empty() && !first.is_empty() && !first.contains(',') =>
        {
            let mut aliases = vec![folder.to_string()];
            if last.chars().count() >= 4 {
                aliases.push(last.to_string());
            }
            (format!("{first} {last}"), aliases)
        }
        _ => (folder.to_string(), Vec::new()),
    };
    Some(Entity {
        id: make_id(name.to_lowercase().as_bytes()),
        name,
        aliases,
    })
}

/// Entities whose name or alias appears in the query as whole words, case-insensitively.
pub fn matched<'a>(query: &str, entities: &'a [Entity]) -> Vec<&'a Entity> {
    let words = |text: &str| {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let query_words = words(query);
    entities
        .iter()
        .filter(|entity| {
            entity.all_names().any(|name| {
                let needle = words(name);
                !needle.is_empty() && query_words.windows(needle.len()).any(|w| w == needle)
            })
        })
        .collect()
}

/// Search that resolves entities first: the query is expanded with every alias of a matched
/// entity, and files in that entity's folder count as strong evidence even when their text
/// never names it. With no entity in the query this is a plain search.
pub fn search(db: &Database, query: &str, limit: usize) -> AppResult<Vec<SearchHit>> {
    let entities = db.entities()?;
    let found = matched(query, &entities);
    let mut merged: HashMap<String, SearchHit> = HashMap::new();
    let mut absorb = |hits: Vec<SearchHit>, bonus: f64| {
        for mut hit in hits {
            hit.score += bonus;
            match merged.get_mut(&hit.citation_id) {
                // Text scores keep the best spelling; location evidence adds on top.
                Some(existing) if bonus > 0.0 => existing.score += bonus,
                Some(existing) => existing.score = existing.score.max(hit.score),
                None => {
                    merged.insert(hit.citation_id.clone(), hit);
                }
            }
        }
    };
    // Do this for every query, not only known entities. Case names commonly appear only in a
    // filename while their scanned first page begins with a generic court heading.
    absorb(db.filename_hits(query, limit * 2)?, 0.0);
    absorb(db.search(query, limit * 2).unwrap_or_default(), 0.0);
    if found.is_empty() {
        let mut hits = merged.into_values().collect::<Vec<_>>();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.citation_id.cmp(&b.citation_id))
        });
        hits.truncate(limit);
        return Ok(hits);
    }
    let names = found
        .iter()
        .flat_map(|entity| entity.all_names().map(String::from))
        .collect::<Vec<_>>();
    for name in &names {
        // Text evidence: the entity under any of its spellings.
        absorb(db.search(name, limit * 2).unwrap_or_default(), 0.0);
    }
    // Location evidence: files stored under the entity's folder.
    absorb(db.first_chunks_under(&names, limit * 2)?, PATH_EVIDENCE);
    let mut hits = merged.into_values().collect::<Vec<_>>();
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.citation_id.cmp(&b.citation_id))
    });
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_become_entities() {
        let e = entity_from_folder("Turnip, John").unwrap();
        assert_eq!(e.name, "John Turnip");
        assert_eq!(e.aliases, vec!["Turnip, John", "Turnip"]);
        let e = entity_from_folder("ABC Construction").unwrap();
        assert_eq!((e.name.as_str(), e.aliases.len()), ("ABC Construction", 0));
        for generic in ["Contracts", "2025", ".git", "ab", "Scans"] {
            assert!(entity_from_folder(generic).is_none(), "{generic}");
        }
    }

    #[test]
    fn finds_files_by_folder_without_a_name_match() {
        use crate::models::{DocumentResource, ParseStatus, ParsedBlock, SourceLocation};
        let db = Database::open_memory().unwrap();
        db.upsert_entity(&entity_from_folder("Turnip, John").unwrap())
            .unwrap();
        let document = DocumentResource {
            id: "d1".into(),
            name: "scan0042.txt".into(),
            path: "/Clients/Turnip, John/scan0042.txt".into(),
            mime_type: "text/plain".into(),
            size: 1,
            modified_at: 1,
            content_hash: "h".into(),
            status: ParseStatus::Ready,
            warning: None,
            chunk_count: 1,
        };
        let block = ParsedBlock {
            text: "Retainer paid in full.".into(),
            location: SourceLocation::default(),
            ocr: false,
            ocr_confidence: None,
        };
        db.upsert_document(&document, &[block]).unwrap();
        assert!(db.search("john turnip files", 8).unwrap().is_empty());
        let hits = search(&db, "bring up John Turnip's files", 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, "d1");
    }

    fn add_document(db: &Database, id: &str, name: &str, text: &str) {
        use crate::models::{DocumentResource, ParseStatus, ParsedBlock, SourceLocation};
        let document = DocumentResource {
            id: id.into(),
            name: name.into(),
            path: format!("/Files/{name}"),
            mime_type: "text/plain".into(),
            size: 1,
            modified_at: 1,
            content_hash: id.into(),
            status: ParseStatus::Ready,
            warning: None,
            chunk_count: 1,
        };
        let block = ParsedBlock {
            text: text.into(),
            location: SourceLocation::default(),
            ocr: false,
            ocr_confidence: None,
        };
        db.upsert_document(&document, &[block]).unwrap();
    }

    #[test]
    fn a_word_that_only_shares_a_prefix_does_not_make_an_unrelated_file_a_match() {
        let db = Database::open_memory().unwrap();
        add_document(
            &db,
            "es",
            "README-es.md",
            "Cada mensaje de error se guarda en el registro.",
        );
        add_document(
            &db,
            "plan",
            "Session plan.txt",
            "The mens session opens with a gender breakout for small groups.",
        );
        let hits = search(&db, "tell me about mens gender breakout", 8).unwrap();
        let names: Vec<_> = hits.iter().map(|hit| hit.document_name.as_str()).collect();
        assert_eq!(names, ["Session plan.txt"]);
    }

    #[test]
    fn a_name_split_by_a_space_still_finds_the_one_word_spelling() {
        let db = Database::open_memory().unwrap();
        // Many unrelated files whose names merely contain "dock" or "zero".
        for index in 0..30 {
            add_document(
                &db,
                &format!("docket{index}"),
                &format!("Docket Entry {index}.txt"),
                "Filed a notice of appearance in the matter.",
            );
            add_document(
                &db,
                &format!("zero{index}"),
                &format!("Zero Balance {index}.txt"),
                "The account shows a zero balance.",
            );
        }
        add_document(
            &db,
            "text-only",
            "notes.txt",
            "ZeroDock is our product for secure sharing.",
        );
        add_document(&db, "named", "ZeroDock Overview.txt", "A short overview.");

        for question in ["tell me about zerodock?", "tell me about zero dock?"] {
            let ids = search(&db, question, 8)
                .unwrap()
                .into_iter()
                .map(|hit| hit.document_id)
                .collect::<Vec<_>>();
            assert!(
                ids.contains(&"text-only".to_string()),
                "{question}: {ids:?}"
            );
            assert!(ids.contains(&"named".to_string()), "{question}: {ids:?}");
        }
    }

    #[test]
    fn question_filler_words_do_not_match_file_names() {
        let db = Database::open_memory().unwrap();
        add_document(&db, "a", "Tell Us About Yourself.txt", "A questionnaire.");
        add_document(&db, "b", "Lease.txt", "The lease begins in May.");
        let ids = search(&db, "tell me about the lease", 8)
            .unwrap()
            .into_iter()
            .map(|hit| hit.document_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["b".to_string()]);
    }

    #[test]
    fn removed_entities_are_not_rediscovered() {
        let db = Database::open_memory().unwrap();
        let entity = entity_from_folder("ABC Construction").unwrap();
        assert!(db.add_entity_if_new(&entity).unwrap());
        db.remove_entity(&entity.id).unwrap();
        assert!(!db.add_entity_if_new(&entity).unwrap());
        assert!(db.entities().unwrap().is_empty());
    }

    #[test]
    fn matches_whole_words_and_aliases() {
        let entities = vec![
            entity_from_folder("Turnip, John").unwrap(),
            entity_from_folder("ABC Construction").unwrap(),
        ];
        assert_eq!(matched("bring up John Turnip's files", &entities).len(), 1);
        assert_eq!(matched("turnip payment terms", &entities).len(), 1);
        assert_eq!(matched("abc construction agreement", &entities).len(), 1);
        assert!(matched("turnips are vegetables", &entities).is_empty());
    }
}
