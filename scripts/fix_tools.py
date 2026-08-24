#!/usr/bin/env python3
"""Fix shell.rs compile errors."""
NL = chr(10)
p = "crates/tools/src/shell.rs"
s = open(p).read()

# 1) stray /// inside inner-doc block
s = s.replace("/// macOS) and lands", "//! macOS) and lands")

# 2) read_capped mutable take
old_read = (
    "    let take = (&mut io).take(cap as u64);" + NL +
    "    let _ = take.read_to_end(buf);"
)
new_read = (
    "    let mut take = (&mut io).take(cap as u64);" + NL +
    "    let _ = take.read_to_end(buf);"
)
assert old_read in s, "read_capped pattern"
s = s.replace(old_read, new_read, 1)

# 3) env test without unsafe: rely on non-allowlisted ambient HOME.
old_env = (
    "        // SAFETY: test-only mutation of process env, single-threaded test." + NL +
    '        unsafe { std::env::set_var("HEPHAESTUS_TEST_SECRET", "hunter2") };' + NL +
    "        let dir = tempfile::tempdir().expect("tmp");" + NL +
    '        // Use sh with a builtin to print env (sh allowlisted only here).' + NL +
    '        let caps = caps_for(dir.path().to_path_buf(), &["env"]);' + NL +
    "        let shell = SandboxedShell::new(&caps);" + NL +
    '        let out = shell.execute("env", &[]).expect("run");' + NL +
    "        assert!(" + NL +
    '            !out.stdout.contains("hunter2"),' + NL +
    '            "ambient secret leaked into child env"' + NL +
    "        );" + NL +
    "        assert!(" + NL +
    '            out.stdout.contains("LC_ALL=C"),' + NL +
    '            "deterministic locale applied"' + NL +
    "        );"
)
new_env = (
    "        let dir = tempfile::tempdir().expect("tmp");" + NL +
    '        let caps = caps_for(dir.path().to_path_buf(), &["env"]);' + NL +
    "        let shell = SandboxedShell::new(&caps);" + NL +
    '        let out = shell.execute("env", &[]).expect("run");' + NL +
    "        // HOME is ambient but NOT allowlisted: must not reach child." + NL +
    "        let home_leaked = out" + NL +
    "            .stdout" + NL +
    '            .lines()' + NL +
    '            .any(|l| l.starts_with("HOME="));' + NL +
    "        assert!(!home_leaked, "non-allowlisted env var leaked");" + NL +
    "        assert!(" + NL +
    '            out.stdout.contains("LC_ALL=C"),' + NL +
    '            "deterministic locale applied"' + NL +
    "        );"
)
assert old_env in s, "env test pattern"
s = s.replace(old_env, new_env, 1)
open(p, "w").write(s)

# 4) manifest: wait-timeout belongs in main deps.
p2 = "crates/tools/Cargo.toml"
s2 = open(p2).read()
if "wait-timeout = { workspace = true }" in s2.split("[dev-dependencies]")[0]:
    print("manifest already ok")
else:
    s2 = s2.replace(
        "tracing = { workspace = true }",
        "tracing = { workspace = true }" + NL + "wait-timeout = { workspace = true }",
        1,
    )
    s2 = s2.replace(NL + "wait-timeout = { workspace = true }" + NL + "[lints]", NL + "[lints]")
    open(p2, "w").write(s2)
    print("manifest fixed")
print("done")
