//! Fuzzy matching for the finder: how well a typed query matches a name,
//! like the fuzzy finders in code editors.

/// Points per matched character, and bonuses for where it matched: the
/// very start, the start of a word, or right after the previous match.
/// Each character skipped between matches costs `GAP`.
const MATCH: i32 = 1;
const START: i32 = 10;
const WORD_START: i32 = 7;
const RUN: i32 = 5;
const GAP: i32 = 1;

/// How well `query` matches `text`, higher is better, or None unless all
/// of `query`'s characters appear in `text` in order. Case doesn't matter;
/// spaces in the query are ignored. The best alignment wins: "ki" finds
/// "Kingston" at its start rather than at "k…i" further on. Among equal
/// matches, the shorter text scores higher.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() || q.len() > t.len() {
        return None;
    }
    let bonus = |j: usize| {
        if j == 0 {
            START
        } else if !t[j - 1].is_alphanumeric() {
            WORD_START
        } else {
            0
        }
    };
    // best[j]: the best score with the query so far matched, its last
    // character at text position j (None where it can't be).
    let mut best: Vec<Option<i32>> = (0..t.len())
        .map(|j| (t[j] == q[0]).then(|| MATCH + bonus(j)))
        .collect();
    for &c in &q[1..] {
        let mut next = vec![None; t.len()];
        // The best previous match before j, net of the gap to j.
        let mut carry: Option<i32> = None;
        for j in 0..t.len() {
            if t[j] == c {
                let run = j.checked_sub(1).and_then(|p| best[p]).map(|s| s + RUN);
                let jump = carry;
                next[j] = run.max(jump).map(|s| s + MATCH + bonus(j));
            }
            // Moving past j: a match at j is now one character back, and
            // anything carried pays for skipping j.
            carry = carry.map(|s| s - GAP).max(best[j]);
        }
        best = next;
    }
    let top = best.into_iter().flatten().max()?;
    Some(top * 100 - t.len() as i32)
}

/// The indices of the best `n` of `names` for `query`, best first.
pub fn best<'a>(query: &str, names: impl IntoIterator<Item = &'a str>, n: usize) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = names
        .into_iter()
        .enumerate()
        .filter_map(|(i, name)| score(query, name).map(|s| (i, s)))
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(n).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: [&str; 8] = [
        "Kaleetan",
        "Kingston",
        "Kitsap",
        "Kittitas",
        "Edmonds / Kingston",
        "Bainbridge Island",
        "Seattle / Bainbridge Island",
        "Walla Walla",
    ];

    #[test]
    fn prefers_starts_then_words_then_runs() {
        let top = |q| {
            best(q, NAMES, 3)
                .into_iter()
                .map(|i| NAMES[i])
                .collect::<Vec<_>>()
        };
        // Starting matches first, the shorter of equals ahead.
        assert_eq!(top("ki"), ["Kitsap", "Kingston", "Kittitas"]);
        assert_eq!(top("king"), ["Kingston", "Edmonds / Kingston"]);
        // A word start beats letters strewn through a name.
        assert_eq!(top("bi")[0], "Bainbridge Island");
        assert_eq!(top("sea bain"), ["Seattle / Bainbridge Island"]);
        assert_eq!(top("ww"), ["Walla Walla"]);
        assert!(top("xyz").is_empty());
    }

    #[test]
    fn scores_need_every_character_in_order() {
        assert!(score("KI", "kingston").is_some());
        assert_eq!(score("ik", "Kingston"), None);
        assert_eq!(score("", "Kingston"), None);
        assert_eq!(score("   ", "Kingston"), None);
        // The best alignment counts, not the first: "an" as a run at
        // "Kaleet-an" rather than the "a…n" from its second letter.
        assert!(score("an", "Kaleetan") > score("an", "Kaleetxn"));
    }
}
