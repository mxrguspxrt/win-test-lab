//! Reading a run file: `extends`, `vars` and `${WTL_NAME}`.
//!
//! A run file may build on another (`"extends": "./base.json"`, relative to the file, local
//! files only, at most [`MAX_EXTENDS`] deep, no loops). Each file's paths (`lab` when it is a
//! path, the `mount` folders) stay relative to the file they are written in. The files merge
//! base first: objects key by key, anything else (`steps` included) replaced by the child's.
//!
//! `vars` declares the run's variables: `WTL_` + `A-Z 0-9 _`, not one of `wtl`'s own settings
//! ([`RESERVED`]). A host environment variable of the same name wins over the default; `null`
//! (or the long form without `default`) must be set. `${WTL_NAME}` in any string value (not in
//! keys, `vars` or `$schema`) is replaced by the value after parsing, so a value can never
//! change the file's structure; it must be declared. `$${` is a literal `${`, and any other
//! `${...}` (PowerShell's `${env:PATH}`) is left alone. Every value is then checked by the
//! usual validation as if it had been typed in.

use crate::labdef::{lab_is_path, RunVar, VarDef};
use crate::util;
use anyhow::{anyhow, bail, Result};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// How many files deep an `extends` chain may go.
pub const MAX_EXTENDS: usize = 8;

/// `wtl`'s own settings, read from the environment by `wtl` itself, and the variables `wtl`
/// sets in every step itself (who and where it runs): no run variable may use these names.
pub const RESERVED: &[&str] = &[
    "WTL_ENVIRONMENT",
    "WTL_ENVIRONMENT_ID",
    "WTL_MACHINE",
    "WTL_RUN_ID",
    "WTL_HOSTNAME",
    "WTL_HOME",
    "WTL_ROOT",
    "WTL_CONFIG",
    "WTL_ISO_DIR",
    "WTL_IMAGES_DIR",
    "WTL_STATE_DIR",
    "WTL_RUNS_DIR",
    "WTL_DRIVERS_DIR",
    "WTL_QEMU",
    "WTL_QEMU_ARM64",
    "WTL_QEMU_IMG",
    "WTL_NO_KVM",
    "WTL_ACCEL",
    "WTL_CPU",
    "WTL_CPUS",
    "WTL_MEM",
    "WTL_PASSWORD",
    "WTL_NIC",
    "WTL_OVMF_CODE",
    "WTL_OVMF_VARS",
    "WTL_AAVMF_CODE",
    "WTL_AAVMF_VARS",
    "WTL_DISPLAY",
    "WTL_QMP",
    "WTL_PRODUCT_KEY",
    "WTL_BUILDER",
    "WTL_TEST_FAST",
];

/// One file of an `extends` chain.
struct Layer {
    path: PathBuf,
    value: Map<String, Value>,
}

/// Read the run file `config` and the files it extends, and apply the variables (`env` looks
/// one up in the host environment). Returns the merged run (without `extends`) for
/// [`crate::labdef::RunDef`] and the variables with their values.
pub fn load(config: &Path, env: impl Fn(&str) -> Option<String>) -> Result<(Value, Vec<RunVar>)> {
    let mut layers = read_chain(config)?;
    let vars = resolve_vars(&layers, env)?;
    let values: IndexMap<String, String> = vars.iter().map(|v| (v.name.clone(), v.value.clone())).collect();
    for (i, layer) in layers.iter_mut().enumerate() {
        let shown = layer.path.display().to_string();
        for (k, v) in layer.value.iter_mut() {
            if k != "vars" && k != "$schema" {
                substitute_value(v, &values).map_err(|e| anyhow!("{}: {}: {}", shown, k, e))?;
            }
        }
        // a base file's relative paths are relative to that file, not to the run file
        if i > 0 {
            let dir = layer.path.parent().unwrap_or(Path::new(".")).to_path_buf();
            rebase_paths(&mut layer.value, &dir);
        }
    }
    let mut merged = Value::Object(Map::new());
    for layer in layers.into_iter().rev() {
        merge(&mut merged, Value::Object(layer.value));
    }
    Ok((merged, vars))
}

/// The run file and every file it extends, the run file first (`extends` taken out).
fn read_chain(config: &Path) -> Result<Vec<Layer>> {
    let mut layers: Vec<Layer> = Vec::new();
    let mut next = Some(util::canon(config));
    while let Some(path) = next.take() {
        if layers.iter().any(|l| l.path == path) {
            let chain: Vec<String> = layers.iter().map(|l| l.path.display().to_string()).collect();
            bail!("{}: extends loops back to {}", chain.join(" -> "), path.display());
        }
        if layers.len() > MAX_EXTENDS {
            bail!("{}: extends goes more than {} files deep", config.display(), MAX_EXTENDS);
        }
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(&path).map_err(|e| match layers.last() {
            Some(child) => anyhow!("{}: extends {}: {}", child.path.display(), shown, e),
            None => anyhow!("{}: {}", shown, e),
        })?;
        let mut value = match serde_json::from_str::<Value>(&text).map_err(|e| anyhow!("{}: {}", shown, e))? {
            Value::Object(m) => m,
            _ => bail!("{}: a run file must be a JSON object", shown),
        };
        next = match value.remove("extends") {
            None | Some(Value::Null) => None,
            Some(Value::String(base)) => {
                if base.contains("${") {
                    bail!("{}: extends {:?}: variables are not allowed here (the base file declares some of them)", shown, base);
                }
                if base.trim().is_empty() {
                    bail!("{}: extends is empty", shown);
                }
                let p = Path::new(&base);
                let dir = path.parent().unwrap_or(Path::new("."));
                Some(util::canon(&util::normpath(&if p.is_absolute() { p.to_path_buf() } else { dir.join(p) })))
            }
            Some(_) => bail!("{}: extends must be a path to a run file, e.g. \"./base.json\"", shown),
        };
        layers.push(Layer { path, value });
    }
    Ok(layers)
}

/// The declared variables of the whole chain (base first, the child's entry wins, `secret`
/// sticks) with their values.
fn resolve_vars(layers: &[Layer], env: impl Fn(&str) -> Option<String>) -> Result<Vec<RunVar>> {
    // name -> (default, secret, the file that last declared it)
    let mut decl: IndexMap<String, (Option<String>, bool, String)> = IndexMap::new();
    for layer in layers.iter().rev() {
        let shown = layer.path.display().to_string();
        let Some(vars) = layer.value.get("vars") else { continue };
        let vars: IndexMap<String, Option<VarDef>> = serde_json::from_value(vars.clone())
            .map_err(|e| anyhow!("{}: vars: {} (each value is a string, null, or {{\"default\": ..., \"secret\": true}})", shown, e))?;
        for (name, def) in vars {
            check_var_name(&name).map_err(|e| anyhow!("{}: vars: {}", shown, e))?;
            let (default, secret) = match def {
                None => (None, false),
                Some(VarDef::Value(v)) => (Some(v), false),
                Some(VarDef::Spec(s)) => (s.default, s.secret),
            };
            let was_secret = decl.get(&name).is_some_and(|d| d.1);
            decl.insert(name, (default, secret || was_secret, shown.clone()));
        }
    }
    let mut out = Vec::new();
    for (name, (default, secret, file)) in decl {
        let value = match env(&name).or(default) {
            Some(v) => v,
            None => bail!("{}: {} is not set (it has no default in vars; run e.g. {}=... wtl run ...)", file, name, name),
        };
        if value.chars().any(|c| c == '\0' || c == '\r' || c == '\n') {
            bail!("{}: {}: the value must be a single line (no line breaks or NUL)", file, name);
        }
        out.push(RunVar { name, value, secret });
    }
    Ok(out)
}

/// A run variable's name: `WTL_` + `A-Z 0-9 _`, and not one of `wtl`'s own settings.
pub fn check_var_name(name: &str) -> std::result::Result<(), String> {
    let rest = name.strip_prefix("WTL_").unwrap_or("");
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
        return Err(format!("{:?} must be WTL_ followed by A-Z 0-9 _ (e.g. WTL_OS)", name));
    }
    if RESERVED.contains(&name) || name.starts_with("WTL_HOST_") {
        return Err(format!("{} is one of wtl's own settings; pick another name", name));
    }
    Ok(())
}

/// Is `${inner}` a variable reference (`WTL_` + name characters)? Anything else is left alone.
fn is_var_ref(inner: &str) -> bool {
    inner.strip_prefix("WTL_").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

/// Replace `${WTL_NAME}` in `s` (`$${` is a literal `${`). An undeclared name is an error.
pub fn substitute(s: &str, vars: &IndexMap<String, String>) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let at = &rest[i..];
        if let Some(after) = at.strip_prefix("$${") {
            out.push_str("${");
            rest = after;
            continue;
        }
        if let Some(after) = at.strip_prefix("${") {
            if let Some(end) = after.find('}') {
                let name = &after[..end];
                // ${WTL_HOST_<KEY>}: a machine's computer name, filled in once the lab is known
                if name.starts_with("WTL_HOST_") {
                    out.push_str(&at[..2 + end + 1]);
                    rest = &after[end + 1..];
                    continue;
                }
                if is_var_ref(name) {
                    match vars.get(name) {
                        Some(v) => out.push_str(v),
                        None => return Err(format!("${{{}}} is not declared in \"vars\"", name)),
                    }
                    rest = &after[end + 1..];
                    continue;
                }
            }
        }
        out.push('$');
        rest = &at[1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `${WTL_HOST_<KEY>}` in `s`: each machine's computer name (`hosts`: `WTL_HOST_CL1` ->
/// `CL1-EXP42`), once the lab is known; [`substitute`] leaves them for this. An unknown machine is
/// an error.
pub fn substitute_hosts(s: &str, hosts: &IndexMap<String, String>) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${WTL_HOST_") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[i..]);
            return Ok(out);
        };
        let name = &after[..end];
        match hosts.get(name) {
            Some(h) => out.push_str(h),
            None => {
                let have: Vec<String> = hosts.keys().map(|k| format!("${{{}}}", k)).collect();
                return Err(format!("${{{}}} is no machine of the lab ({})", name, have.join(" ")));
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// [`substitute`] in every string value under `v` (object keys stay as they are).
fn substitute_value(v: &mut Value, vars: &IndexMap<String, String>) -> std::result::Result<(), String> {
    match v {
        Value::String(s) => *s = substitute(s, vars)?,
        Value::Array(a) => a.iter_mut().try_for_each(|x| substitute_value(x, vars))?,
        Value::Object(m) => m.values_mut().try_for_each(|x| substitute_value(x, vars))?,
        _ => {}
    }
    Ok(())
}

/// Make a base file's relative `lab` path and `mount` folders relative to its own folder.
fn rebase_paths(m: &mut Map<String, Value>, dir: &Path) {
    let rebase = |s: &mut String| {
        if !s.trim().is_empty() && !Path::new(s.as_str()).is_absolute() {
            *s = util::normpath(&dir.join(s.as_str())).to_string_lossy().into_owned();
        }
    };
    if let Some(Value::String(lab)) = m.get_mut("lab") {
        if lab_is_path(lab) {
            rebase(lab);
        }
    }
    if let Some(Value::Object(mounts)) = m.get_mut("mount") {
        for v in mounts.values_mut() {
            if let Value::String(s) = v {
                rebase(s);
            }
        }
    }
}

/// Merge `child` into `base`: objects key by key, anything else replaced.
fn merge(base: &mut Value, child: Value) {
    match (base, child) {
        (Value::Object(b), Value::Object(c)) => {
            for (k, v) in c {
                match b.get_mut(&k) {
                    Some(bv) => merge(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, c) => *b = c,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn substitutes_declared_names_only() {
        let v = vars(&[("WTL_OS", "single-win11"), ("WTL_DIR", "/b/out")]);
        assert_eq!(substitute("${WTL_OS}", &v).unwrap(), "single-win11");
        assert_eq!(substitute("smoke-${WTL_OS}-${WTL_OS}", &v).unwrap(), "smoke-single-win11-single-win11");
        assert_eq!(substitute("${WTL_DIR}/x", &v).unwrap(), "/b/out/x");
        // PowerShell and anything not WTL_-shaped is left alone
        assert_eq!(substitute("$env:PATH ${env:TEMP} $x ${wtl_os} $", &v).unwrap(), "$env:PATH ${env:TEMP} $x ${wtl_os} $");
        // $${ is a literal ${
        assert_eq!(substitute("$${WTL_OS} costs $$5", &v).unwrap(), "${WTL_OS} costs $$5");
        // an unclosed ${ stays as it is
        assert_eq!(substitute("${WTL_OS", &v).unwrap(), "${WTL_OS");
        // a value is inserted as it is, never re-read
        let tricky = vars(&[("WTL_A", "${WTL_B}\"}"), ("WTL_B", "no")]);
        assert_eq!(substitute("${WTL_A}", &tricky).unwrap(), "${WTL_B}\"}");
        let e = substitute("${WTL_OSS}", &v).unwrap_err();
        assert!(e.contains("${WTL_OSS} is not declared"), "{}", e);
        // machine names are left for later, then filled in
        assert_eq!(substitute("http://${WTL_HOST_CA1}/x", &v).unwrap(), "http://${WTL_HOST_CA1}/x");
        let hosts = vars(&[("WTL_HOST_CA1", "CA1-EXP42")]);
        assert_eq!(substitute_hosts("http://${WTL_HOST_CA1}/x", &hosts).unwrap(), "http://CA1-EXP42/x");
        assert!(substitute_hosts("${WTL_HOST_NOPE}", &hosts).unwrap_err().contains("no machine of the lab"));
    }

    #[test]
    fn var_names() {
        assert!(check_var_name("WTL_OS").is_ok());
        assert!(check_var_name("WTL_BUILD_DIR2").is_ok());
        for bad in ["OS", "WTL_", "WTL_os", "wtl_OS", "WTL_A-B", "GITHUB_TOKEN"] {
            assert!(check_var_name(bad).unwrap_err().contains("must be WTL_"), "{}", bad);
        }
        assert!(check_var_name("WTL_PASSWORD").unwrap_err().contains("wtl's own settings"));
        assert!(check_var_name("WTL_HOST_CA1").unwrap_err().contains("wtl's own settings"));
        assert!(check_var_name("WTL_HOSTNAME").unwrap_err().contains("wtl's own settings"));
    }

    /// Every setting `wtl` seeds from the environment is reserved.
    #[test]
    fn reserved_covers_wtl_settings() {
        let home = crate::home::Home::fixed(Path::new("/r"));
        for (k, _) in crate::qemu::defaults(&home) {
            assert!(RESERVED.contains(&k), "{} is not in RESERVED", k);
        }
    }

    fn write(dir: &Path, name: &str, v: Value) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wtl-runconf-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        util::canon(&d)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn vars_defaults_env_and_required() {
        let d = tmp("vars");
        let p = write(
            &d,
            "run.json",
            json!({
                "vars": { "WTL_OS": "single-win11", "WTL_DIR": null, "WTL_KEY": { "default": "k-123", "secret": true } },
                "lab": "${WTL_OS}", "name": "smoke-${WTL_OS}",
                "mount": { "X": "${WTL_DIR}" },
                "steps": [ { "run": "X:/setup.ps1", "args": "-Key ${WTL_KEY} $env:PATH" } ]
            }),
        );
        // a required var that is not set
        let e = load(&p, no_env).unwrap_err().to_string();
        assert!(e.contains("WTL_DIR is not set"), "{}", e);
        // the environment wins over the default
        let env = |k: &str| match k {
            "WTL_DIR" => Some("/b/out".to_string()),
            "WTL_OS" => Some("single-2022".to_string()),
            _ => None,
        };
        let (v, vars) = load(&p, env).unwrap();
        assert_eq!(v["lab"], "single-2022");
        assert_eq!(v["name"], "smoke-single-2022");
        assert_eq!(v["mount"]["X"], "/b/out");
        assert_eq!(v["steps"][0]["args"], "-Key k-123 $env:PATH");
        let names: Vec<(&str, &str, bool)> = vars.iter().map(|x| (x.name.as_str(), x.value.as_str(), x.secret)).collect();
        assert_eq!(names, vec![("WTL_OS", "single-2022", false), ("WTL_DIR", "/b/out", false), ("WTL_KEY", "k-123", true)]);
        // vars are kept as written (for RunDef); no substitution inside them
        assert_eq!(v["vars"]["WTL_OS"], "single-win11");
    }

    #[test]
    fn rejects_bad_vars() {
        let d = tmp("badvars");
        let undeclared = write(&d, "a.json", json!({ "lab": "${WTL_OS}", "steps": [] }));
        let e = load(&undeclared, no_env).unwrap_err().to_string();
        assert!(e.contains("lab: ${WTL_OS} is not declared"), "{}", e);
        let reserved = write(&d, "b.json", json!({ "vars": { "WTL_MEM": "1" }, "lab": "x" }));
        assert!(load(&reserved, no_env).unwrap_err().to_string().contains("wtl's own settings"));
        let shape = write(&d, "c.json", json!({ "vars": { "WTL_X": 5 }, "lab": "x" }));
        assert!(load(&shape, no_env).unwrap_err().to_string().contains("each value is a string"));
        let multi = write(&d, "d.json", json!({ "vars": { "WTL_X": null }, "lab": "x" }));
        let e = load(&multi, |_| Some("a\nb".into())).unwrap_err().to_string();
        assert!(e.contains("single line") && !e.contains("a\nb"), "{}", e);
    }

    #[test]
    fn extends_merges_and_rebases() {
        let d = tmp("extends");
        write(
            &d,
            "base/base.json",
            json!({
                "vars": { "WTL_OS": "single-2019", "WTL_KEY": { "default": "s3", "secret": true } },
                "lab": "../labs/${WTL_OS}.json",
                "wait": 90,
                "mount": { "X": "../out", "Y": "/abs/tools" },
                "steps": [ { "run": "X:/a.bat" } ]
            }),
        );
        let p = write(
            &d,
            "proj/run.json",
            json!({
                "extends": "../base/base.json",
                "vars": { "WTL_OS": "single-2022", "WTL_KEY": "plain" },
                "mount": { "Z": "./z" },
                "steps": [ { "run": "Z:/b.ps1", "args": "${WTL_KEY}" } ]
            }),
        );
        let (v, vars) = load(&p, no_env).unwrap();
        // the child's var default wins, and is applied in the base file too
        assert_eq!(v["lab"], d.join("labs/single-2022.json").to_string_lossy().as_ref());
        assert_eq!(v["wait"], 90);
        // mounts merge; the base's relative one is relative to the base file
        assert_eq!(v["mount"]["X"], d.join("out").to_string_lossy().as_ref());
        assert_eq!(v["mount"]["Y"], "/abs/tools");
        assert_eq!(v["mount"]["Z"], "./z");
        // steps are replaced, not appended
        assert_eq!(v["steps"].as_array().unwrap().len(), 1);
        assert_eq!(v["steps"][0]["args"], "plain");
        assert!(v.get("extends").is_none());
        // a secret stays secret when a child overrides it
        assert!(vars.iter().find(|x| x.name == "WTL_KEY").unwrap().secret);
    }

    #[test]
    fn extends_errors() {
        let d = tmp("extends-bad");
        let a = write(&d, "a.json", json!({ "extends": "./b.json", "lab": "x" }));
        write(&d, "b.json", json!({ "extends": "a.json" }));
        let e = load(&a, no_env).unwrap_err().to_string();
        assert!(e.contains("loops back"), "{}", e);
        let missing = write(&d, "c.json", json!({ "extends": "./nope.json" }));
        let e = load(&missing, no_env).unwrap_err().to_string();
        assert!(e.contains("extends") && e.contains("nope.json"), "{}", e);
        let var = write(&d, "e.json", json!({ "extends": "./${WTL_X}.json" }));
        assert!(load(&var, no_env).unwrap_err().to_string().contains("variables are not allowed"));
        let num = write(&d, "f.json", json!({ "extends": 3 }));
        assert!(load(&num, no_env).unwrap_err().to_string().contains("must be a path"));
        // too deep
        for i in 0..=MAX_EXTENDS + 1 {
            write(&d, &format!("deep{}.json", i), json!({ "extends": format!("./deep{}.json", i + 1) }));
        }
        write(&d, &format!("deep{}.json", MAX_EXTENDS + 2), json!({}));
        let e = load(&d.join("deep0.json"), no_env).unwrap_err().to_string();
        assert!(e.contains("files deep"), "{}", e);
    }
}
