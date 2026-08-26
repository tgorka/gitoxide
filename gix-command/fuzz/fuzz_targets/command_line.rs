#![no_main]

use libfuzzer_sys::fuzz_target;
use std::{ffi::OsStr, hint::black_box};

fn os_bytes(input: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        input.as_bytes()
    }
    #[cfg(not(unix))]
    {
        input
            .to_str()
            .expect("successfully parsed command lines contain representable OS strings")
            .as_bytes()
    }
}

fn quote(word: &[u8]) -> Vec<u8> {
    let mut out = vec![b'\''];
    for &byte in word {
        if byte == b'\'' {
            out.extend_from_slice(br"'\''");
        } else {
            out.push(byte);
        }
    }
    out.push(b'\'');
    out
}

fuzz_target!(|input: &[u8]| {
    if let Ok(parsed) = gix_command::parse::command_line(input.into()) {
        assert!(parsed.env.iter().all(|(name, _)| {
            let mut bytes = name.iter().copied();
            bytes.next().is_some_and(|b| b == b'_' || b.is_ascii_alphabetic())
                && bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
        }));

        let canonical = parsed
            .env
            .iter()
            .map(|(name, value)| {
                let mut assignment = name.to_vec();
                assignment.push(b'=');
                assignment.extend_from_slice(&quote(value));
                assignment
            })
            .chain(std::iter::once(quote(os_bytes(&parsed.command))))
            .chain(parsed.args.iter().map(|arg| quote(os_bytes(arg))))
            .collect::<Vec<_>>()
            .join(&b' ');
        assert_eq!(
            gix_command::parse::command_line(canonical.as_slice().into()),
            Ok(parsed.clone())
        );
        _ = black_box(parsed);
    }
});
