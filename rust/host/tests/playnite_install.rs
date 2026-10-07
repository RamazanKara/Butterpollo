use butterpollo_windows::process::{Process, Target};
use std::{collections::BTreeMap, ffi::OsStr, path::Path, time::Duration};

fn helper(args: &[&OsStr]) -> u32 {
    let program = Path::new(env!("CARGO_BIN_EXE_butterpollo"));
    let args: Vec<_> = args.iter().map(|&arg| arg.to_owned()).collect();
    Process::spawn(
        program,
        &args,
        program.parent(),
        Target::User { elevated: false },
        &BTreeMap::new(),
        true,
    )
    .unwrap()
    .wait(Duration::from_secs(30))
    .unwrap()
}

fn install(source: &Path, target: &Path) -> u32 {
    helper(&[
        "--playnite-install".as_ref(),
        source.as_os_str(),
        target.as_os_str(),
    ])
}

fn uninstall(target: &Path) -> u32 {
    helper(&["--playnite-uninstall".as_ref(), target.as_os_str()])
}

#[test]
fn user_session_helper_copies_unicode_paths_and_reports_partial_installs() {
    let root = std::env::temp_dir().join(format!("butterpollo-playnite-{}", uuid::Uuid::new_v4()));
    let source = root.join("Packaged & plugin");
    let target = root.join("Çağrı & Oyunlar/Extensions/SunshinePlaynite");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("extension.yaml"), "Version: 0.4.14").unwrap();
    std::fs::write(source.join("SunshinePlaynite.psm1"), "module").unwrap();

    assert_eq!(install(&source, &target), 0);
    assert_eq!(
        std::fs::read_to_string(target.join("extension.yaml")).unwrap(),
        "Version: 0.4.14"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("SunshinePlaynite.psm1")).unwrap(),
        "module"
    );

    std::fs::write(source.join("extension.yaml"), "Version: 0.4.15").unwrap();
    std::fs::remove_file(target.join("SunshinePlaynite.psm1")).unwrap();
    std::fs::create_dir(target.join("SunshinePlaynite.psm1")).unwrap();
    // A failed file operation reports its Windows error to the service.
    assert_eq!(install(&source, &target), 5);
    assert_eq!(
        std::fs::read_to_string(target.join("extension.yaml")).unwrap(),
        "Version: 0.4.14"
    );
    std::fs::remove_dir(target.join("SunshinePlaynite.psm1")).unwrap();
    assert_eq!(install(&source, &target), 0);
    assert_eq!(
        std::fs::read_to_string(target.join("extension.yaml")).unwrap(),
        "Version: 0.4.15"
    );
    assert_eq!(install(&root.join("missing"), &target), 1);

    assert_eq!(uninstall(&target), 0);
    assert!(!target.exists());
    assert_eq!(uninstall(&target), 0);
    std::fs::remove_dir_all(root).unwrap();
}
