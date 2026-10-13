//! Turns the names an LLM supplies into ids. Matching is exact (case-insensitive,
//! whitespace-trimmed), never fuzzy: a wrong guess would write to the wrong place.

use std::collections::HashMap;

use crate::{
    error::{Error, Result},
    model::{Category, Kind, Wallet},
};

const MAX_LISTED: usize = 30;

pub fn normalized(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn find_wallet<'a>(wallets: &'a [Wallet], query: &str) -> Result<&'a Wallet> {
    let q = normalized(query);
    if q.is_empty() {
        return Err(Error::Invalid("wallet name must not be empty".into()));
    }
    let matches: Vec<&Wallet> = wallets
        .iter()
        .filter(|w| normalized(&w.name) == q)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::Invalid(format!(
            "Wallet '{}' not found. Available wallets: {}",
            query.trim(),
            join_limited(wallets.iter().map(|w| w.name.clone()))
        ))),
        many => Err(Error::Invalid(format!(
            "Wallet '{}' is ambiguous; {} wallets share that name: {}",
            query.trim(),
            many.len(),
            join_limited(many.iter().map(|w| w.name.clone()))
        ))),
    }
}

/// `Parent > Child` for sub-categories, the bare name otherwise.
pub fn category_path(cat: &Category, all: &[Category]) -> String {
    let by_id: HashMap<&str, &Category> = all.iter().map(|c| (c.id.as_str(), c)).collect();
    match cat.parent_id.as_deref().and_then(|id| by_id.get(id)) {
        Some(parent) => format!("{} > {}", parent.name, cat.name),
        None => cat.name.clone(),
    }
}

pub fn category_label(cat: &Category, all: &[Category]) -> String {
    format!("{} ({})", category_path(cat, all), cat.kind.label())
}

/// Matches on the category name or its full `Parent > Child` path.
pub fn find_category<'a>(
    categories: &'a [Category],
    query: &str,
    kind: Option<Kind>,
) -> Result<&'a Category> {
    let q = normalized(query);
    if q.is_empty() {
        return Err(Error::Invalid("category name must not be empty".into()));
    }
    let candidates: Vec<&Category> = categories
        .iter()
        .filter(|c| kind.is_none_or(|k| c.kind == k))
        .collect();
    let matches: Vec<&Category> = candidates
        .iter()
        .copied()
        .filter(|c| normalized(&c.name) == q || normalized(&category_path(c, categories)) == q)
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => {
            let similar: Vec<&Category> = candidates
                .iter()
                .copied()
                .filter(|c| normalized(&c.name).contains(&q) || q.contains(&normalized(&c.name)))
                .collect();
            let (hint, pool) = if similar.is_empty() {
                ("Available categories", candidates)
            } else {
                ("Similar categories", similar)
            };
            Err(Error::Invalid(format!(
                "Category '{}' not found{}. {hint}: {}",
                query.trim(),
                kind.map(|k| format!(" among {} categories", k.label()))
                    .unwrap_or_default(),
                join_limited(pool.iter().map(|c| category_label(c, categories)))
            )))
        }
        many => Err(Error::Invalid(format!(
            "Category '{}' is ambiguous. Matches: {}. Pass `type` or use the full 'Parent > Child' name.",
            query.trim(),
            join_limited(many.iter().map(|c| category_label(c, categories)))
        ))),
    }
}

fn join_limited(items: impl Iterator<Item = String>) -> String {
    let all: Vec<String> = items.collect();
    if all.len() <= MAX_LISTED {
        all.join(", ")
    } else {
        format!(
            "{}, … and {} more",
            all[..MAX_LISTED].join(", "),
            all.len() - MAX_LISTED
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(id: &str, name: &str) -> Wallet {
        Wallet {
            id: id.into(),
            name: name.into(),
            currency: None,
            balance: None,
            archived: false,
            exclude_total: false,
        }
    }

    fn cat(id: &str, name: &str, kind: Kind, parent: Option<&str>) -> Category {
        Category {
            id: id.into(),
            name: name.into(),
            kind,
            parent_id: parent.map(str::to_string),
            wallet_id: None,
            excluded_wallets: Vec::new(),
        }
    }

    #[test]
    fn wallet_matching_is_case_and_space_insensitive() {
        let wallets = [wallet("1", "Cash"), wallet("2", "BCA Main")];
        assert_eq!(find_wallet(&wallets, "  bca   main ").unwrap().id, "2");
    }

    #[test]
    fn wallet_errors_list_candidates() {
        let wallets = [wallet("1", "Cash"), wallet("2", "Cash")];
        let ambiguous = find_wallet(&wallets, "cash").unwrap_err().to_string();
        assert!(ambiguous.contains("ambiguous"));
        let missing = find_wallet(&wallets, "bank").unwrap_err().to_string();
        assert!(missing.contains("not found") && missing.contains("Cash"));
        assert!(find_wallet(&wallets, "  ").is_err());
    }

    #[test]
    fn same_name_with_different_types_needs_a_type() {
        let cats = [
            cat("1", "Other", Kind::Income, None),
            cat("2", "Other", Kind::Expense, None),
        ];
        let err = find_category(&cats, "other", None).unwrap_err().to_string();
        assert!(
            err.contains("ambiguous")
                && err.contains("Other (income)")
                && err.contains("Other (expense)")
        );
        assert_eq!(
            find_category(&cats, "other", Some(Kind::Expense))
                .unwrap()
                .id,
            "2"
        );
    }

    #[test]
    fn subcategory_matches_by_name_or_full_path() {
        let cats = [
            cat("1", "Food", Kind::Expense, None),
            cat("2", "Lunch", Kind::Expense, Some("1")),
        ];
        assert_eq!(find_category(&cats, "lunch", None).unwrap().id, "2");
        assert_eq!(find_category(&cats, "food > lunch", None).unwrap().id, "2");
        assert_eq!(category_label(&cats[1], &cats), "Food > Lunch (expense)");
    }

    #[test]
    fn category_not_found_suggests_similar_names() {
        let cats = [
            cat("1", "Groceries", Kind::Expense, None),
            cat("2", "Salary", Kind::Income, None),
        ];
        let err = find_category(&cats, "grocery", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Available categories"), "{err}");
        let err = find_category(&cats, "Groceries Weekly", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Similar categories") && err.contains("Groceries (expense)"),
            "{err}"
        );
    }

    #[test]
    fn long_candidate_lists_are_truncated() {
        let cats: Vec<Category> = (0..40)
            .map(|i| cat(&i.to_string(), &format!("C{i}"), Kind::Expense, None))
            .collect();
        let err = find_category(&cats, "zzz", None).unwrap_err().to_string();
        assert!(err.contains("and 10 more"), "{err}");
    }
}
