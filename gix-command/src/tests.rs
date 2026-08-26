use super::*;

#[test]
fn internal_win_path_lookup() -> gix_testtools::Result {
    let root = gix_testtools::scripted_fixture_read_only("win_path_lookup.sh")?;
    let mut paths: Vec<_> = std::fs::read_dir(&root)?
        .filter_map(Result::ok)
        .map(|e| e.path().to_str().expect("no illformed UTF8").to_owned())
        .collect();
    paths.sort();
    let lookup_path: OsString = paths.join(";").into();

    assert_eq!(
        win_path_lookup("a/b".as_ref(), &lookup_path),
        None,
        "any path with separator is considered ready to use"
    );
    assert_eq!(
        win_path_lookup("x".as_ref(), &lookup_path),
        Some(root.join("a").join("x.exe")),
        "exe will be preferred, and it searches left to right thus doesn't find c/x.exe"
    );
    assert_eq!(
        win_path_lookup("x.exe".as_ref(), &lookup_path),
        Some(root.join("a").join("x.exe")),
        "no matter what, a/x won't be found as it's shadowed by an exe file"
    );
    assert_eq!(
        win_path_lookup("exe".as_ref(), &lookup_path),
        Some(root.join("b").join("exe")),
        "it finds files further down the path as well"
    );
    Ok(())
}
