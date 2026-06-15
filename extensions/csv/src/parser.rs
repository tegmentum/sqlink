//! Minimal CSV parser. RFC-ish: comma-separated, optional
//! double-quoted fields with `""` escape inside quotes, newline-
//! terminated rows. No `\r\n` normalization tricks  CR before LF
//! is dropped.

use alloc::string::String;
use alloc::vec::Vec;

/// Parse CSV text into rows. Empty input yields zero rows.
pub fn parse(input: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                // Quoted field. Loop until the closing quote;
                // doubled "" becomes a literal " inside.
                loop {
                    match chars.next() {
                        None => break,
                        Some('"') => {
                            if chars.peek() == Some(&'"') {
                                chars.next();
                                field.push('"');
                            } else {
                                break;
                            }
                        }
                        Some(other) => field.push(other),
                    }
                }
            }
            ',' => {
                row.push(core::mem::take(&mut field));
            }
            '\n' => {
                row.push(core::mem::take(&mut field));
                rows.push(core::mem::take(&mut row));
            }
            '\r' => {
                // Drop CR; the following LF closes the row.
            }
            other => field.push(other),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple() {
        let r = parse("a,b,c\n1,2,3\n");
        assert_eq!(r, vec![vec!["a", "b", "c"], vec!["1", "2", "3"]]);
    }

    #[test]
    fn parses_quoted() {
        let r = parse("\"hello, world\",x\n");
        assert_eq!(r, vec![vec!["hello, world", "x"]]);
    }

    #[test]
    fn parses_escaped_quote() {
        let r = parse("\"she said \"\"hi\"\"\",x\n");
        assert_eq!(r, vec![vec!["she said \"hi\"", "x"]]);
    }

    #[test]
    fn empty_input() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn no_trailing_newline() {
        let r = parse("a,b\n1,2");
        assert_eq!(r, vec![vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test]
    fn handles_crlf() {
        let r = parse("a,b\r\n1,2\r\n");
        assert_eq!(r, vec![vec!["a", "b"], vec!["1", "2"]]);
    }
}
