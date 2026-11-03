//! Combines passages found in local files with passages from the firm's cloud index into one
//! ranked list. The two searches score on different scales, so scores are never compared directly:
//! each list contributes by rank (reciprocal rank fusion), and a passage near the top of both lists
//! ranks above one that is first in only one.

use std::collections::HashMap;

use crate::models::{Origin, SearchHit};

/// Standard damping constant for reciprocal rank fusion. Higher values flatten the difference
/// between neighbouring ranks.
const RRF_K: f64 = 60.0;
/// How much of a passage is compared when deciding whether two hits are the same text.
const DUPLICATE_PREFIX_CHARS: usize = 160;

fn fingerprint(hit: &SearchHit) -> String {
    hit.excerpt
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(DUPLICATE_PREFIX_CHARS)
        .collect()
}

/// Merges two ranked lists, best first. A passage present in both (the same file reached through
/// a local folder and a connector) appears once, as the local copy, and gains rank from both.
pub fn merge(local: Vec<SearchHit>, cloud: Vec<SearchHit>, limit: usize) -> Vec<SearchHit> {
    struct Entry {
        hit: SearchHit,
        score: f64,
        first_seen: usize,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut by_text: HashMap<String, usize> = HashMap::new();
    let mut order = 0usize;

    for list in [local, cloud] {
        for (rank, hit) in list.into_iter().enumerate() {
            let contribution = 1.0 / (RRF_K + rank as f64 + 1.0);
            let key = fingerprint(&hit);
            match by_text.get(&key).filter(|_| !key.is_empty()) {
                Some(&index) => {
                    let entry = &mut entries[index];
                    entry.score += contribution;
                    // Keep the local copy, which can be opened and verified on this device.
                    if entry.hit.origin == Origin::Cloud && hit.origin == Origin::Local {
                        entry.hit = hit;
                    }
                }
                None => {
                    by_text.insert(key, entries.len());
                    entries.push(Entry {
                        hit,
                        score: contribution,
                        first_seen: order,
                    });
                    order += 1;
                }
            }
        }
    }

    // Ties keep the order the passages were found in, local first.
    entries.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.first_seen.cmp(&b.first_seen))
    });
    entries
        .into_iter()
        .take(limit)
        .map(|mut entry| {
            entry.hit.score = entry.score;
            entry.hit
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, text: &str, origin: Origin) -> SearchHit {
        SearchHit {
            citation_id: id.into(),
            document_id: id.into(),
            document_name: format!("{id}.txt"),
            path: String::new(),
            excerpt: text.into(),
            location: Default::default(),
            location_label: String::new(),
            score: 0.0,
            ocr: false,
            stale: false,
            origin,
            link: None,
        }
    }

    fn ids(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter().map(|h| h.citation_id.as_str()).collect()
    }

    #[test]
    fn the_two_lists_are_interleaved_by_rank_not_by_raw_score() {
        // Scores on different scales must not decide the order: here the local scores are huge.
        let mut local = vec![
            hit("L1", "local one", Origin::Local),
            hit("L2", "local two", Origin::Local),
        ];
        local[0].score = 9_000.0;
        let cloud = vec![
            hit("C1", "cloud one", Origin::Cloud),
            hit("C2", "cloud two", Origin::Cloud),
        ];
        let merged = merge(local, cloud, 10);
        assert_eq!(ids(&merged), ["L1", "C1", "L2", "C2"]);
    }

    #[test]
    fn a_passage_found_by_both_searches_outranks_one_found_by_one() {
        let local = vec![
            hit("L1", "alpha", Origin::Local),
            hit("L2", "the retainer is due", Origin::Local),
        ];
        let cloud = vec![
            hit("C1", "beta", Origin::Cloud),
            hit("C2", "The retainer is due!", Origin::Cloud),
        ];
        let merged = merge(local, cloud, 10);
        // L2 and C2 are the same text; together they beat the singletons.
        assert_eq!(ids(&merged), ["L2", "L1", "C1"]);
        assert_eq!(merged[0].origin, Origin::Local, "the local copy is kept");
        assert!(merged[0].score > merged[1].score);
    }

    #[test]
    fn duplicates_collapse_whichever_list_found_them_first() {
        let cloud = vec![hit("C1", "same text here", Origin::Cloud)];
        let local = vec![hit("L1", "Same text, here", Origin::Local)];
        let merged = merge(local, cloud, 10);
        assert_eq!((merged.len(), merged[0].origin), (1, Origin::Local));
        let merged = merge(
            vec![],
            vec![hit("C1", "x", Origin::Cloud), hit("C2", "x", Origin::Cloud)],
            10,
        );
        assert_eq!(merged.len(), 1, "duplicates inside one list collapse too");
    }

    #[test]
    fn limits_and_empty_lists_behave() {
        assert!(merge(vec![], vec![], 5).is_empty());
        let cloud: Vec<_> = (0..5)
            .map(|n| hit(&format!("C{n}"), &format!("c{n}"), Origin::Cloud))
            .collect();
        assert_eq!(ids(&merge(vec![], cloud.clone(), 3)), ["C0", "C1", "C2"]);
        assert_eq!(merge(vec![], cloud, 0).len(), 0);
        let local = vec![hit("L1", "only local", Origin::Local)];
        assert_eq!(ids(&merge(local, vec![], 5)), ["L1"]);
    }

    #[test]
    fn empty_text_is_never_treated_as_a_duplicate() {
        let merged = merge(
            vec![hit("L1", "", Origin::Local)],
            vec![hit("C1", "", Origin::Cloud)],
            5,
        );
        assert_eq!(merged.len(), 2);
    }
}
