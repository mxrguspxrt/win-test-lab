//! Port of tests/test_labconf.py: the run-config parser spec. Every case of the Python
//! suite is here with the same inputs and the same expectations, plus the example configs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use wtl_rs::labconf::{self, ConfError, Mount, RunConfig, Step, Who};

static N: AtomicUsize = AtomicUsize::new(0);

/// A fresh temp file name ending in .conf.
fn tmp_conf(dir: &Path) -> PathBuf {
    let n = N.fetch_add(1, Ordering::SeqCst);
    dir.join(format!("tmpwtl{}_{}.conf", std::process::id(), n))
}

/// Write TEXT to a temp .conf file and load it (like load_text in the Python suite).
fn load_text(text: &str) -> Result<RunConfig, ConfError> {
    load_files(&[text], &std::env::temp_dir())
}

/// Write several configs to temp files in DIR and merge-load them (left->right).
fn load_files(texts: &[&str], dir: &Path) -> Result<RunConfig, ConfError> {
    let paths: Vec<String> = texts
        .iter()
        .map(|t| {
            let p = tmp_conf(dir);
            std::fs::write(&p, t).unwrap();
            p.to_string_lossy().into_owned()
        })
        .collect();
    let r = labconf::load(&paths);
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    r
}

/// The Python suite wrote its multi-file configs to "." (the cwd).
fn load_files_cwd(texts: &[&str]) -> Result<RunConfig, ConfError> {
    load_files(texts, &std::env::current_dir().unwrap())
}

#[track_caller]
fn expect_error(text: &str, needle: &str) {
    match load_text(text) {
        Ok(c) => panic!("expected a ConfError, got none: {:?}", c),
        Err(e) => assert!(e.to_string().contains(needle), "error {:?} lacks {:?}", e.to_string(), needle),
    }
}

fn m(host: &str, letter: &str) -> Mount {
    Mount { host: host.into(), letter: letter.into() }
}

fn admin(p: &str) -> Step {
    Step { who: Who::Admin, path: p.into() }
}

fn student(p: &str) -> Step {
    Step { who: Who::Student, path: p.into() }
}

// --- valid: full config ---
#[test]
fn full_config() {
    let cfg = load_text(
        "
        version = 2019
        mount = /a:X, /b/c:Z
        run_as_admin = X:\\setup.bat, Z:\\go.ps1
        run_as_student = X:\\run.exe
        name = demo
    ",
    )
    .unwrap();
    assert_eq!(cfg.version, "2019");
    assert_eq!(cfg.name, "demo");
    assert_eq!(cfg.mounts.len(), 2, "two mounts");
    assert_eq!(cfg.mounts[0], m("/a", "X"), "mount letter X");
    assert_eq!(cfg.mounts[1], m("/b/c", "Z"), "mount letter Z");
    assert_eq!(cfg.steps, vec![admin("X:\\setup.bat"), admin("Z:\\go.ps1"), student("X:\\run.exe")]);
}

// --- steps run in the order written: admin/student may interleave ---
#[test]
fn ordered_interleave() {
    let cfg = load_text(
        "
        version = 2019
        mount = /a:X
        run_as_admin = X:\\install.bat
        run_as_student = X:\\run.exe
        run_as_admin = X:\\cleanup.bat
    ",
    )
    .unwrap();
    assert_eq!(cfg.steps, vec![admin("X:\\install.bat"), student("X:\\run.exe"), admin("X:\\cleanup.bat")]);
}

// --- slashes normalised, letters upper-cased ---
#[test]
fn slashes_and_case() {
    let cfg = load_text("version=2016\nmount=/x:z\nrun_as_student=z:/sub/app.exe\n").unwrap();
    assert_eq!(cfg.mounts[0].letter, "Z", "letter upper");
    assert_eq!(cfg.steps, vec![student("Z:\\sub\\app.exe")], "ref normalised");
}

// --- defaults ---
#[test]
fn defaults() {
    let cfg = load_text("version=2012\nmount=/x:X\nrun_as_student=X:\\a.bat\n").unwrap();
    assert_eq!(cfg.wait, 60, "default wait");
    assert_eq!(cfg.timeout_min, 60, "default timeout");
    assert!(!cfg.name.is_empty(), "name from filename"); // non-empty (temp file stem)
    assert!(cfg.name.starts_with("tmpwtl"), "name is the file stem: {}", cfg.name);
}

// --- no mounts is allowed if a C: script runs ---
#[test]
fn no_mount_ok_with_c() {
    let cfg = load_text("version=2012\nrun_as_admin=C:\\Windows\\System32\\whoami.exe\n").unwrap();
    assert!(cfg.mounts.is_empty());
    assert_eq!(cfg.steps, vec![admin("C:\\Windows\\System32\\whoami.exe")]);
}

// --- errors ---
#[test]
fn errors() {
    expect_error("mount=/a:X\nrun_as_student=X:\\a.bat\n", "version"); // missing version
    expect_error("version=2019\nfoo=bar\n", "unknown key");
    expect_error("version=2019\nmount=/a:X\n", "nothing to do"); // no steps
    expect_error("version=2019\nmount=/a\nrun_as_student=C:\\x.bat\n", "HOSTPATH:LETTER"); // bad mount form
    expect_error("version=2019\nmount=/a:C\nrun_as_student=C:\\x.bat\n", "reserved"); // reserved C
    expect_error("version=2019\nmount=/a:D\nrun_as_student=C:\\x.bat\n", "reserved"); // reserved D
    expect_error("version=2019\nmount=/a:X,/b:X\nrun_as_student=X:\\x.bat\n", "more than one"); // dup letter
    expect_error("version=2019\nmount=/a:XY\nrun_as_student=C:\\x.bat\n", "single letter"); // two-char letter
    expect_error("version=2019\nmount=/a:X\nrun_as_student=X:\\a.txt\n", ".bat"); // step bad ext
    expect_error("version=2019\nmount=/a:X\nrun_as_student=Z:\\a.bat\n", "not mounted"); // step unmounted drive
    expect_error("version=2019\nmount=/a:X\nrun_as_student=run.bat\n", "LETTER"); // step no drive
    expect_error("version=2019\nmount=/a:W,/b:X,/c:Y,/d:Z\nrun_as_student=W:\\a.bat\n", "at most 3"); // too many mounts
    expect_error("version=2019\nmount /a:X\n", "expected key = value"); // missing = line
}

// --- multi-file merge (later wins) ---
#[test]
fn merge_version_from_second_file() {
    let cfg = load_files_cwd(&[
        "mount=/a:X\nrun_as_admin=X:\\install.bat\nrun_as_student=X:\\run.exe\n", // workload
        "version=win11\n",                                                       // OS file
    ])
    .unwrap();
    assert_eq!(cfg.version, "win11", "merge: version from 2nd file");
    assert_eq!(cfg.mounts, vec![m("/a", "X")], "merge: mount from 1st file");
    assert_eq!(cfg.steps, vec![admin("X:\\install.bat"), student("X:\\run.exe")], "merge: steps from 1st file");
}

#[test]
fn merge_later_scalars_win() {
    let cfg = load_files_cwd(&["version=2019\nname=a\nrun_as_student=C:\\x.bat\n", "version=2022\nname=b\n"]).unwrap();
    assert_eq!(cfg.version, "2022", "merge: later version wins");
    assert_eq!(cfg.name, "b", "merge: later name wins");
    assert_eq!(cfg.steps, vec![student("C:\\x.bat")], "merge: steps kept when 2nd has none");
}

#[test]
fn merge_mount_and_steps_overridden() {
    // mount in the later file replaces the earlier (per-key override, not append)
    let cfg = load_files_cwd(&["version=2019\nmount=/a:X\nrun_as_student=X:\\r.exe\n", "mount=/b:Z\nrun_as_student=Z:\\r.exe\n"])
        .unwrap();
    assert_eq!(cfg.mounts, vec![m("/b", "Z")], "merge: mount overridden");
    // a later file that defines any step replaces all earlier steps
    assert_eq!(cfg.steps, vec![student("Z:\\r.exe")], "merge: steps overridden");
}

// ---------------------------------------------------------------- beyond the Python suite

/// Exact error texts of labconf.py (lab.sh printed them as "error: ..."), checked against
/// messages produced by the Python implementation.
#[test]
fn exact_messages() {
    let dir = std::env::temp_dir().join(format!("wtl-labconf-msg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("e.conf");
    let ps = p.to_string_lossy().into_owned();
    let cases: &[(&str, String)] = &[
        ("version=2019\nfoo=bar\n", format!("{}:2: unknown key 'foo' (known: mount, name, run_as_admin, run_as_student, timeout_min, version, wait)", ps)),
        ("version=2019\nmount /a:X\n", format!("{}:2: expected key = value", ps)),
        ("mount=/a:X\nrun_as_student=X:\\a.bat\n", "e.conf: 'version' is required (the lab to boot, e.g. 2019)".into()),
        ("version=20 19\nrun_as_student=C:\\a.bat\n", "e.conf: bad version '20 19'".into()),
        ("version=2019\nmount=/a\nrun_as_student=C:\\x.bat\n", "e.conf: mount '/a' must be HOSTPATH:LETTER".into()),
        ("version=2019\nmount=:X\nrun_as_student=C:\\x.bat\n", "e.conf: mount ':X' has an empty host path".into()),
        ("version=2019\nmount=/a:XY\nrun_as_student=C:\\x.bat\n", "e.conf: mount drive 'XY' must be a single letter A-Z".into()),
        ("version=2019\nmount=/a:c\nrun_as_student=C:\\x.bat\n", "e.conf: drive C: is reserved (C: system, D: lab disc); pick e.g. X, Y, Z".into()),
        ("version=2019\nmount=/a:X,/b:x\nrun_as_student=X:\\x.bat\n", "e.conf: drive X: used by more than one mount".into()),
        ("version=2019\nmount=/a:X\nrun_as_student=run.bat\n", "e.conf: run_as_student entry 'run.bat' must be LETTER:\\path (e.g. X:\\run.bat)".into()),
        ("version=2019\nmount=/a:X\nrun_as_admin=Z:\\a.bat\n", "e.conf: run_as_admin 'Z:\\\\a.bat' uses drive Z: which is not mounted".into()),
        ("version=2019\nmount=/a:X\nrun_as_student=X:\\a.txt\n", "e.conf: run_as_student 'X:\\\\a.txt' must end in .bat .cmd .ps1 or .exe".into()),
        ("version=2019\nmount=/a:X\n", "e.conf: nothing to do (set run_as_admin and/or run_as_student)".into()),
        ("version=2019\nname=a b\nrun_as_student=C:\\a.bat\n", "e.conf: bad name 'a b'".into()),
        ("version=2019\nmount=/a:W,/b:X,/c:Y,/d:Z\nrun_as_student=W:\\a.bat\n", "e.conf: at most 3 mounts (the lab disc takes one of the 4 CD slots)".into()),
    ];
    for (text, want) in cases {
        std::fs::write(&p, text).unwrap();
        let e = labconf::load(&[ps.as_str()]).unwrap_err();
        assert_eq!(&e.to_string(), want, "for {:?}", text);
    }
    let missing = dir.join("nope.conf").to_string_lossy().into_owned();
    assert_eq!(labconf::load(&[missing.as_str()]).unwrap_err().to_string(), format!("config not found: {}", missing));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Odd but valid inputs, with what labconf.py made of them.
#[test]
fn edge_cases() {
    // comments anywhere, CRLF, a value containing '=', the last scalar in a file wins,
    // mount lines in one file append, X:\\\\double slashes collapse, empty list items skipped
    let cfg = load_text(
        "# hi\r\nversion = 2016 # trailing\r\nversion=2019\r\nmount = /a:x\r\nmount=/b:Y ,\r\n\
         run_as_admin = X:\\\\\\\\deep\\\\a.BAT, ,Y:/b.Cmd\r\nname=\r\nwait= 5\r\ntimeout_min =  +7\r\n",
    )
    .unwrap();
    assert_eq!(cfg.version, "2019");
    assert_eq!(cfg.mounts, vec![m("/a", "X"), m("/b", "Y")]);
    assert_eq!(cfg.steps, vec![admin("X:\\deep\\\\a.BAT"), admin("Y:\\b.Cmd")]);
    assert!(cfg.name.starts_with("tmpwtl"), "empty name -> file stem");
    assert_eq!((cfg.wait, cfg.timeout_min), (5, 7));
    // relative mount paths resolve against the config file's directory
    let dir = std::env::temp_dir().join(format!("wtl-labconf-rel-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("conf")).unwrap();
    let p = dir.join("conf/r.conf");
    std::fs::write(&p, "version=2019\nmount=../out:X, ./a/../b:Y, /abs:Z\nrun_as_student=X:\\s.exe\n").unwrap();
    let cfg = labconf::load(&[p.to_string_lossy()]).unwrap();
    let d = dir.to_string_lossy();
    assert_eq!(cfg.mounts, vec![m(&format!("{}/out", d), "X"), m(&format!("{}/conf/b", d), "Y"), m("/abs", "Z")]);
    assert_eq!(cfg.name, "r");
    let _ = std::fs::remove_dir_all(&dir);
}

/// JSON exactly as `labconf.py json` printed it (lab.sh and guest/lab.ps1 consume it).
#[test]
fn json_output() {
    let cfg = load_text("version=2019\nmount=/a:X\nrun_as_admin=X:\\i.bat\nrun_as_student=X:\\r.exe\nname=n\n").unwrap();
    assert_eq!(
        wtl_rs::pyjson::dumps_indent(&cfg, 1),
        "{\n \"version\": \"2019\",\n \"name\": \"n\",\n \"mounts\": [\n  {\n   \"host\": \"/a\",\n   \"letter\": \"X\"\n  }\n ],\n \
         \"steps\": [\n  {\n   \"who\": \"admin\",\n   \"path\": \"X:\\\\i.bat\"\n  },\n  {\n   \"who\": \"student\",\n   \
         \"path\": \"X:\\\\r.exe\"\n  }\n ],\n \"wait\": 60,\n \"timeout_min\": 60\n}"
    );
}

/// Every shipped example config validates (tests/all.sh "example configs validate").
#[test]
fn example_configs_validate() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/configs");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("conf") {
            continue;
        }
        let cfg = labconf::load(&[p.to_string_lossy()]).unwrap_or_else(|e| panic!("{}: {}", p.display(), e));
        assert!(!cfg.steps.is_empty());
        n += 1;
    }
    assert!(n >= 3, "found {} example configs", n);
    let smoke = labconf::load(&[dir.join("2019-smoke.conf").to_string_lossy()]).unwrap();
    assert_eq!(smoke.version, "2019");
    assert_eq!(smoke.name, "smoke");
    assert_eq!(smoke.steps, vec![admin("X:\\setup.bat"), student("X:\\smoke.exe")]);
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/smoke-test/out");
    let out = wtl_rs::util::normpath(&wtl_rs::util::abspath(&out));
    assert_eq!(smoke.mounts, vec![m(&out.to_string_lossy(), "X")]);
}
