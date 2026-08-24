#!/usr/bin/env python3
"""Add public commit_all to GitRepo; switch inventory test to it."""
NL = chr(10)
p = "crates/repo/src/git.rs"
s = open(p).read()

anchor = "    /// Recent commits touching one path (provenance for change risk)."
addition = (
    "    /// Stage every change and create a commit." + NL +
    "    ///" + NL +
    "    /// Authorship is supplied explicitly - never read from ambient" + NL +
    "    /// config - so audit records can attribute the acting principal." + NL +
    "    /// Returns the new commit hash." + NL +
    "    pub fn commit_all(&self, message: &str, author: (&str, &str)) -> Result<String> {" + NL +
    "        self.run(&["add", "--", "."])?;" + NL +
    "        let ident = [" + NL +
    "            "-c"," + NL +
    '            &format!("user.name={}", author.0),' + NL +
    "            "-c"," + NL +
    '            &format!("user.email={}", author.1),' + NL +
    "        ];" + NL +
    "        let with_ident: Vec<&str> = ident.iter().copied()" + NL +
    '            .chain(["commit", "-q", "-m", message].iter().copied())' + NL +
    "            .collect();" + NL +
    "        self.run(&with_ident)?;" + NL +
    "        self.head_commit()" + NL +
    "    }" + NL +
    NL
)
assert anchor in s, "anchor missing in git.rs"
s = s.replace(anchor, addition + anchor, 1)

# inventory test: replace manual add/commit block.
p2 = "crates/repo/src/inventory.rs"
s2 = open(p2).read()
old_block = (
    '        repo.run(&["add", "--", "."]).expect("add");' + NL +
    "        repo.run(&[" + NL +
    '            "-c", "user.name=T", "-c", "user.email=t@t.invalid",' + NL +
    '            "commit", "-q", "-m", "x",' + NL +
    "        ])" + NL +
    '        .expect("commit");'
)
new_block = (
    '        repo.commit_all("x", ("T", "t@t.invalid")).expect("commit");'
)
if old_block in s2:
    s2 = s2.replace(old_block, new_block, 1)
    print("inventory test switched to commit_all")
else:
    print("WARN: inventory pattern not found")
open(p2, "w").write(s2)
open(p, "w").write(s)
print("done")
