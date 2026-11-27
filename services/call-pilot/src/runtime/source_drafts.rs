//! Presentation-only state, scoped to one owned provider window. It cannot
//! register a canonical source item or satisfy a source-final drain barrier.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct SourceDrafts {
    active: BTreeMap<String, (u32, String)>,
    retired: BTreeSet<String>,
}
impl SourceDrafts {
    pub fn apply(
        &mut self,
        id: &str,
        revision: u32,
        text: &str,
        retired: bool,
        bound: bool,
    ) -> bool {
        if bound
            || id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            || !(1..=4096).contains(&revision)
            || text.len() > 16384
            || text.chars().count() > 4096
            || text.contains('\0')
            || self.retired.contains(id)
        {
            return false;
        }
        if retired {
            if !text.is_empty()
                || self.retired.len() >= 4096
                || self
                    .active
                    .get(id)
                    .is_none_or(|(prior, _)| *prior != revision)
            {
                return false;
            }
            self.active.remove(id);
            self.retired.insert(id.into());
        } else {
            if text.is_empty() {
                return false;
            }
            if let Some((prior, snapshot)) = self.active.get(id) {
                if revision != prior + 1
                    || !text.starts_with(snapshot)
                    || text.len() <= snapshot.len()
                {
                    return false;
                }
            } else if revision != 1 || self.active.len() >= 8 || self.retired.len() >= 4096 {
                return false;
            }
            self.active.insert(id.into(), (revision, text.into()));
        }
        true
    }
    pub fn can_bind(&self, id: &str) -> bool {
        !self.active.contains_key(id)
    }
    pub fn complete(&self, bound: &BTreeMap<String, (u32, u32, bool)>) -> bool {
        self.active.is_empty() && self.retired.iter().all(|id| bound.contains_key(id))
    }
    pub fn clear(&mut self) {
        self.active.clear();
        self.retired.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_draft_identity_growth_retirement_and_canonical_binding_are_separate() {
        let mut ledger = SourceDrafts::default();
        let mut bound = BTreeMap::new();
        assert!(ledger.apply("a", 1, "金額", false, false));
        assert!(!ledger.complete(&bound));
        assert!(!ledger.can_bind("a"));
        for (revision, text, retired, known) in [
            (1, "金額", false, false),
            (3, "金額は", false, false),
            (2, "replaced", false, false),
            (2, "金額", false, false),
            (1, "not empty", true, false),
            (2, "", true, false),
            (2, "金額は", false, true),
        ] {
            assert!(!ledger.apply("a", revision, text, retired, known));
        }
        assert!(ledger.apply("a", 2, "金額は12,000円", false, false));
        assert!(ledger.apply("a", 2, "", true, false));
        assert!(ledger.can_bind("a"));
        assert!(
            !ledger.complete(&bound),
            "retirement alone is not a source item"
        );
        bound.insert("a".into(), (1, 1, false));
        assert!(ledger.complete(&bound));
        assert!(!ledger.apply("a", 3, "resurrected", false, false));
        ledger.clear();
        bound.clear();
        assert!(ledger.complete(&bound));
        assert!(ledger.apply("a", 1, "new window", false, false));
    }
    #[test]
    fn draft_capacity_text_and_retirement_ids_remain_bounded() {
        let mut ledger = SourceDrafts::default();
        for n in 0..8 {
            assert!(ledger.apply(&format!("d{n}"), 1, &"🗂".repeat(4096), false, false));
        }
        assert!(!ledger.apply("ninth", 1, "a", false, false));
        assert!(!ledger.apply("d0", 2, &"🗂".repeat(4097), false, false));
        assert!(ledger.apply("d0", 1, "", true, false));
        assert!(ledger.apply("ninth", 1, "a", false, false));
        assert_eq!(ledger.active.len(), 8);
        assert_eq!(ledger.retired.len(), 1);
        let mut lifetime = SourceDrafts::default();
        for n in 0..4096 {
            let id = format!("d{n}");
            assert!(lifetime.apply(&id, 1, "a", false, false));
            assert!(lifetime.apply(&id, 1, "", true, false));
        }
        assert!(!lifetime.apply("next", 1, "a", false, false));
        assert_eq!(lifetime.retired.len(), 4096);
        assert!(lifetime.active.is_empty());
    }
}
