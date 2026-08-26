use std::ffi::OsString;

use bstr::{BStr, BString};

/// The result of [`command_line()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Leading environment assignments, without the separating `=`.
    pub env: Vec<(BString, BString)>,
    /// The command to execute.
    pub command: OsString,
    /// The arguments to pass to the command.
    pub args: Vec<OsString>,
}

/// The error returned when a command line cannot be parsed into a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A quote was opened but never closed.
    MissingClosingQuote,
    /// The input contains no command to execute.
    MissingCommand,
    /// The command or an argument cannot be represented as an OS string on this platform.
    UnrepresentableOsString,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::MissingClosingQuote => "missing closing quote",
            Error::MissingCommand => "missing command",
            Error::UnrepresentableOsString => "command or argument cannot be represented as an OS string",
        })
    }
}

impl std::error::Error for Error {}

fn into_os_string(value: BString) -> Result<OsString, Error> {
    gix_path::try_from_bstring(value)
        .map(std::path::PathBuf::into_os_string)
        .map_err(|_| Error::UnrepresentableOsString)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Single,
    Double,
}

struct Word {
    value: BString,
    assignment_separator: Option<usize>,
}

fn push_unquoted(
    value: &mut BString,
    assignment_possible: &mut bool,
    assignment_separator: &mut Option<usize>,
    byte: u8,
) {
    if assignment_separator.is_none() && *assignment_possible {
        if value.is_empty() {
            *assignment_possible = byte == b'_' || byte.is_ascii_alphabetic();
        } else if byte == b'=' {
            *assignment_separator = Some(value.len());
        } else if byte != b'_' && !byte.is_ascii_alphanumeric() {
            *assignment_possible = false;
        }
    }
    value.push(byte);
}

/// Split `input` into leading environment assignments and the command with its arguments.
///
/// Whitespace, quotes, escapes, line continuations, and comments follow POSIX shell word-splitting rules. Shell
/// expansions and operators are not interpreted. An assignment is recognized only when its name is an unquoted
/// shell identifier and another word follows it; thus an assignment-only input remains a directly invocable
/// command name. Environment assignments remain byte strings, while the command and arguments are converted
/// losslessly to OS strings or rejected if the platform cannot represent them.
pub fn command_line(input: &BStr) -> Result<Outcome, Error> {
    let mut words = Vec::new();
    let mut value = BString::default();
    let mut assignment_possible = true;
    let mut assignment_separator = None;
    let mut word_started = false;
    let mut quote = None;
    let mut bytes = input.iter().copied();

    while let Some(byte) = bytes.next() {
        match quote {
            Some(Quote::Single) => {
                if byte == b'\'' {
                    quote = None;
                } else {
                    value.push(byte);
                }
            }
            Some(Quote::Double) => match byte {
                b'"' => quote = None,
                b'\\' => match bytes.next() {
                    Some(b'\n') => {}
                    Some(next @ (b'$' | b'`' | b'"' | b'\\')) => value.push(next),
                    Some(next) => {
                        value.push(b'\\');
                        value.push(next);
                    }
                    None => return Err(Error::MissingClosingQuote),
                },
                _ => value.push(byte),
            },
            None => match byte {
                b' ' | b'\t' | b'\n' => {
                    if word_started {
                        words.push(Word {
                            value: std::mem::take(&mut value),
                            assignment_separator,
                        });
                        assignment_possible = true;
                        assignment_separator = None;
                        word_started = false;
                    }
                }
                b'#' if !word_started => {
                    bytes.by_ref().find(|byte| *byte == b'\n');
                }
                b'\'' => {
                    assignment_possible = false;
                    word_started = true;
                    quote = Some(Quote::Single);
                }
                b'"' => {
                    assignment_possible = false;
                    word_started = true;
                    quote = Some(Quote::Double);
                }
                b'\\' => match bytes.next() {
                    Some(b'\n') => {}
                    Some(next) => {
                        assignment_possible = false;
                        word_started = true;
                        value.push(next);
                    }
                    None => {
                        assignment_possible = false;
                        word_started = true;
                        value.push(b'\\');
                    }
                },
                _ => {
                    word_started = true;
                    push_unquoted(&mut value, &mut assignment_possible, &mut assignment_separator, byte);
                }
            },
        }
    }
    if quote.is_some() {
        return Err(Error::MissingClosingQuote);
    }
    if word_started {
        words.push(Word {
            value,
            assignment_separator,
        });
    }

    let assignment_count = words
        .iter()
        .take_while(|word| word.assignment_separator.is_some())
        .count();
    let assignment_count = if assignment_count < words.len() {
        assignment_count
    } else {
        0
    };
    let mut args = words.split_off(assignment_count).into_iter().map(|word| word.value);
    let command = into_os_string(args.next().ok_or(Error::MissingCommand)?)?;
    let env = words
        .into_iter()
        .map(|word| {
            let separator = word.assignment_separator.expect("only recognized assignments remain");
            (
                word.value[..separator].to_owned().into(),
                word.value[separator + 1..].to_owned().into(),
            )
        })
        .collect();
    Ok(Outcome {
        env,
        command,
        args: args.map(into_os_string).collect::<Result<_, _>>()?,
    })
}
