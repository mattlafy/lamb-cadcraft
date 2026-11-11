//! Numbered `?` lists at the prompts that pick a view, a level or a detail (LAMB T663): `?` echoes
//! the options numbered from 1, and a typed number picks that option.

use cadcraft_engine::Session;

/// Echo `"<n>  <row>"` for each row, n from 1.
pub(crate) fn echo_numbered(s: &mut Session, rows: &[String]) {
    for (n, row) in rows.iter().enumerate() {
        s.echo(format!("{}  {row}", n + 1));
    }
}

/// The index of the first name equal to `text` ignoring ASCII case; else, when `text` is a whole
/// number `n` in `1..=names.len()`, `n - 1`; else None.
pub(crate) fn pick_numbered(names: &[&str], text: &str) -> Option<usize> {
    let text = text.trim();
    names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(text))
        .or_else(|| text.parse::<usize>().ok().filter(|n| (1..=names.len()).contains(n)).map(|n| n - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_wins_then_position() {
        let names = ["Wall Detail", "Eave", "2"];
        assert_eq!(pick_numbered(&names, "wall detail"), Some(0));
        assert_eq!(pick_numbered(&names, "EAVE"), Some(1));
        // A view literally named `2` is picked by name, not as the second option.
        assert_eq!(pick_numbered(&names, "2"), Some(2));
        assert_eq!(pick_numbered(&names, "1"), Some(0));
        assert_eq!(pick_numbered(&names, "3"), Some(2));
        assert_eq!(pick_numbered(&names, "4"), None);
        assert_eq!(pick_numbered(&names, "0"), None);
        assert_eq!(pick_numbered(&names, "nope"), None);
        assert_eq!(pick_numbered(&["A", "B"], "2"), Some(1));
        assert_eq!(pick_numbered(&[], "1"), None);
    }

    #[test]
    fn echo_numbers_from_one() {
        let mut s = Session::new();
        echo_numbered(&mut s, &["Plan  Ground".to_owned(), "Section  A".to_owned()]);
        assert!(s.log.iter().any(|l| l == "1  Plan  Ground"), "{:?}", s.log);
        assert!(s.log.iter().any(|l| l == "2  Section  A"), "{:?}", s.log);
    }
}
