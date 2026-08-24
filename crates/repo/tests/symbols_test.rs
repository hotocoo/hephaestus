//! Symbol extraction conformance across supported languages.
//!
//! These are golden-style checks: known sources must produce known
//! symbol kinds and names with sane line ranges. Grammar upgrades that
//! change extraction behavior fail here loudly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hephaestus_repo::{Language, SymbolKind, SymbolRegistry};

#[test]
fn rust_definitions_extracted() {
    let src = r###"
struct Ticket { id: u64 }

enum Status { Open, Closed }

trait Repo { fn find(&self, id: u64) -> Option<Ticket>; }

struct Db;

impl Repo for Db {
    fn find(&self, _id: u64) -> Option<Ticket> { None }
}

fn load_ticket(id: u64) -> Result<Ticket, String> {
    Ok(Ticket { id })
}
"###;
    let reg = SymbolRegistry::with_builtins();
    let syms = reg.extract(Language::Rust, src);
    let names: Vec<(SymbolKind, &str)> = syms.iter().map(|s| (s.kind, s.name.as_str())).collect();
    assert!(names.contains(&(SymbolKind::Struct, "Ticket")));
    assert!(names.contains(&(SymbolKind::Enum, "Status")));
    assert!(names.contains(&(SymbolKind::Trait, "Repo")));
    assert!(
        names.contains(&(SymbolKind::Impl, "Db")),
        "impl target from type field"
    );
    assert!(names.contains(&(SymbolKind::Function, "load_ticket")));
    assert!(names.contains(&(SymbolKind::Function, "find")));

    let load = syms.iter().find(|s| s.name == "load_ticket").expect("sym");
    // Source literal opens with a blank line: load_ticket lands on 14.
    assert_eq!(load.start_line, 14);
    assert!(load.end_line >= load.start_line);
}

#[test]
fn typescript_extractions() {
    let ts = r###"
export class UserService {
  find(id: string): string { return id; }
}

interface Repo { find(id: string): string; }

export function loadUser(id: string): string {
  return id;
}
"###;
    let reg = SymbolRegistry::with_builtins();
    let syms = reg.extract(Language::TypeScript, ts);
    let names: Vec<(&str, SymbolKind)> = syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
    assert!(names.contains(&("UserService", SymbolKind::Class)));
    assert!(names.contains(&("find", SymbolKind::Method)));
    assert!(names.contains(&("Repo", SymbolKind::Interface)));
    assert!(names.contains(&("loadUser", SymbolKind::Function)));
}

#[test]
fn python_extractions() {
    let py = concat!(
        "class TicketStore:
",
        "    def find(self, tid):
",
        "        return None
",
        "
",
        "def helper():
",
        "    pass
",
    );
    let reg = SymbolRegistry::with_builtins();
    let syms = reg.extract(Language::Python, py);
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"TicketStore"));
    assert!(names.contains(&"find"));
    assert!(names.contains(&"helper"));
}

#[test]
fn go_struct_vs_interface_distinguished() {
    let go = concat!(
        "package main

",
        "type Store struct{ }

",
        "type Finder interface { Find() }

",
        "func Load() {}
",
    );
    let reg = SymbolRegistry::with_builtins();
    let syms = reg.extract(Language::Go, go);
    assert!(
        syms.iter()
            .any(|s| s.name == "Store" && s.kind == SymbolKind::Struct)
    );
    assert!(
        syms.iter()
            .any(|s| s.name == "Finder" && s.kind == SymbolKind::Interface)
    );
    assert!(
        syms.iter()
            .any(|s| s.name == "Load" && s.kind == SymbolKind::Function)
    );
}

#[test]
fn java_class_method_interface_enum() {
    let java = r###"
public class UserService {
    public String find(String id) { return id; }
}

public interface Repo { String find(String id); }

public enum Status { OPEN, CLOSED }
"###;
    let reg = SymbolRegistry::with_builtins();
    let syms = reg.extract(Language::Java, java);
    assert!(
        syms.iter()
            .any(|s| s.name == "UserService" && s.kind == SymbolKind::Class)
    );
    assert!(
        syms.iter()
            .any(|s| s.name == "find" && s.kind == SymbolKind::Method)
    );
    assert!(
        syms.iter()
            .any(|s| s.name == "Repo" && s.kind == SymbolKind::Interface)
    );
    assert!(
        syms.iter()
            .any(|s| s.name == "Status" && s.kind == SymbolKind::Enum)
    );
}

#[test]
fn every_builtin_language_is_registered() {
    let reg = SymbolRegistry::with_builtins();
    for lang in [
        Language::Rust,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Python,
        Language::Go,
        Language::Java,
    ] {
        assert!(reg.supports(lang), "{lang:?} missing from registry");
    }
}
