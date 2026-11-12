//! Terminal questions on stderr, answers from stdin. Only reached after the
//! command knows it is interactive (a TTY and no `--json`).

use std::io::{BufRead, Write};

use pk_cli_core::CliError;

fn read_line() -> Result<Option<String>, CliError> {
    let mut s = String::new();
    let n = std::io::stdin()
        .lock()
        .read_line(&mut s)
        .map_err(|e| CliError::Other(format!("reading the answer: {e}")))?;
    Ok((n > 0).then(|| s.trim().to_string()))
}

/// Pick one of `options` (`(value, label)`) by number. Enter takes the
/// default; `s` (or end of input) skips and returns `None`.
pub fn choose(
    question: &str,
    options: &[(String, String)],
    default: Option<usize>,
) -> Result<Option<String>, CliError> {
    loop {
        eprintln!("{question}");
        for (i, (_, label)) in options.iter().enumerate() {
            let mark = if Some(i) == default { "*" } else { " " };
            eprintln!(" {mark}{:>2}) {label}", i + 1);
        }
        eprint!(
            "  number{}, or s to leave it for review: ",
            if default.is_some() {
                " (Enter for *)"
            } else {
                ""
            }
        );
        let _ = std::io::stderr().flush();
        let Some(answer) = read_line()? else {
            return Ok(None);
        };
        if answer.eq_ignore_ascii_case("s") {
            return Ok(None);
        }
        if answer.is_empty() {
            if let Some(d) = default {
                return Ok(Some(options[d].0.clone()));
            }
            continue;
        }
        if let Ok(n) = answer.parse::<usize>() {
            if (1..=options.len()).contains(&n) {
                return Ok(Some(options[n - 1].0.clone()));
            }
        }
        eprintln!("  not one of the numbers above");
    }
}

/// A `YYYY-MM-DD` date; Enter takes the default, `s` skips.
pub fn ask_date(question: &str, default: Option<&str>) -> Result<Option<String>, CliError> {
    loop {
        match default {
            Some(d) => eprint!("{question} [{d}] (YYYY-MM-DD, s to skip): "),
            None => eprint!("{question} (YYYY-MM-DD, s to skip): "),
        }
        let _ = std::io::stderr().flush();
        let Some(answer) = read_line()? else {
            return Ok(None);
        };
        if answer.eq_ignore_ascii_case("s") {
            return Ok(None);
        }
        if answer.is_empty() {
            if let Some(d) = default {
                return Ok(Some(d.to_string()));
            }
            continue;
        }
        if pk_cli_core::dates::parse_iso(&answer).is_ok() {
            return Ok(Some(answer));
        }
        eprintln!("  write the date as YYYY-MM-DD");
    }
}

/// Free text; Enter takes the default (possibly none).
pub fn ask_text(question: &str, default: Option<&str>) -> Result<Option<String>, CliError> {
    match default {
        Some(d) => eprint!("{question} [{d}] (- for none): "),
        None => eprint!("{question} (Enter for none): "),
    }
    let _ = std::io::stderr().flush();
    let Some(answer) = read_line()? else {
        return Ok(default.map(str::to_string));
    };
    Ok(match answer.as_str() {
        "" => default.map(str::to_string),
        "-" => None,
        other => Some(other.to_string()),
    })
}
