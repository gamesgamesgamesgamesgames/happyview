//! The two custom sections a script module carries, read without
//! instantiating anything.
//!
//! `happyview.handlers` is one line per handler, `<trigger id>\t<function>`.
//! It is lines rather than a single document because each handler attribute
//! contributes its own bytes and the linker concatenates same-named sections,
//! so the format has to survive concatenation in any order.
//!
//! The bytes come from a file on disk, so every length is capped and a module
//! that exceeds a cap is refused with a diagnostic rather than read to the end.

use wasmparser::{Encoding, Parser, Payload, TypeRef};

pub const HANDLERS_SECTION: &str = "happyview.handlers";
pub const INTERFACE_SECTION: &str = "happyview.interface";
/// The guest interface this host speaks. A module built against another one
/// is refused at build time rather than trapping on its first request.
pub const SCRIPT_INTERFACE_VERSION: &str = "1";

/// Total bytes across every section of one name. A real module's handler table
/// is a few hundred bytes; this leaves room for thousands of handlers.
const MAX_SECTION_BYTES: usize = 256 * 1024;
const MAX_HANDLERS: usize = 4096;
/// A trigger id or function name longer than this is not one.
const MAX_LINE_BYTES: usize = 1024;
/// The version is a short token; anything longer is not one.
const MAX_INTERFACE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleHandler {
    pub trigger: String,
    /// The Rust function, for display only; dispatch is by trigger.
    pub function: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ModuleSections {
    /// In section order, duplicates kept, so the caller can name a trigger
    /// claimed twice.
    pub handlers: Vec<ModuleHandler>,
    pub interface: Option<String>,
    /// A `throw` needs a tag, defined or imported, so a module without one
    /// cannot throw or catch an exception. Handler instructions such as
    /// `try_table` with no tag are not reported: they cannot catch a throw
    /// the module can never create.
    pub uses_exceptions: bool,
}

pub fn read_sections(wasm: &[u8]) -> Result<ModuleSections, String> {
    let mut out = ModuleSections::default();
    let mut interface: Option<String> = None;
    let mut handler_bytes = 0usize;
    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(|e| format!("invalid WASM: {e}"))? {
            Payload::Version { encoding, .. } if encoding != Encoding::Module => {
                return Err("the file is a WASM component, not a core module".into());
            }
            Payload::CustomSection(section) if section.name() == HANDLERS_SECTION => {
                handler_bytes = handler_bytes.saturating_add(section.data().len());
                if handler_bytes > MAX_SECTION_BYTES {
                    return Err(format!(
                        "the {HANDLERS_SECTION} sections total more than {MAX_SECTION_BYTES} bytes"
                    ));
                }
                for line in utf8(HANDLERS_SECTION, section.data())?.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if line.len() > MAX_LINE_BYTES {
                        return Err(format!(
                            "a line in the {HANDLERS_SECTION} section is longer than {MAX_LINE_BYTES} bytes"
                        ));
                    }
                    if out.handlers.len() >= MAX_HANDLERS {
                        return Err(format!(
                            "the {HANDLERS_SECTION} section names more than {MAX_HANDLERS} handlers"
                        ));
                    }
                    let (trigger, function) = match line.split_once('\t') {
                        Some((trigger, function)) => {
                            let function = function.trim();
                            if function.is_empty() {
                                return Err(format!(
                                    "the {HANDLERS_SECTION} line for {:?} has a tab but no function name",
                                    trigger.trim()
                                ));
                            }
                            (trigger, Some(function.to_string()))
                        }
                        None => (line, None),
                    };
                    out.handlers.push(ModuleHandler {
                        trigger: trigger.trim().to_string(),
                        function,
                    });
                }
            }
            Payload::CustomSection(section) if section.name() == INTERFACE_SECTION => {
                // Same-named sections are concatenated by the linker, so a
                // second one means two crates emitted it, and joining them
                // would let a split of "1" pass for the real version.
                if interface.is_some() {
                    return Err(format!(
                        "the module has more than one {INTERFACE_SECTION} section"
                    ));
                }
                if section.data().len() > MAX_INTERFACE_BYTES {
                    return Err(format!(
                        "the {INTERFACE_SECTION} section is longer than {MAX_INTERFACE_BYTES} bytes"
                    ));
                }
                interface = Some(utf8(INTERFACE_SECTION, section.data())?.to_string());
            }
            Payload::TagSection(_) => out.uses_exceptions = true,
            Payload::ImportSection(imports) => {
                for import in imports.into_imports() {
                    let import = import.map_err(|e| format!("invalid WASM: {e}"))?;
                    if matches!(import.ty, TypeRef::Tag(_)) {
                        out.uses_exceptions = true;
                    }
                }
            }
            _ => {}
        }
    }
    out.interface = interface.map(|v| v.trim().to_string());
    Ok(out)
}

fn utf8<'a>(section: &str, data: &'a [u8]) -> Result<&'a str, String> {
    std::str::from_utf8(data).map_err(|_| format!("the {section} section is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(trigger: &str, function: &str) -> ModuleHandler {
        ModuleHandler {
            trigger: trigger.into(),
            function: Some(function.into()),
        }
    }

    #[test]
    fn handlers_are_read_line_by_line_across_every_section_of_the_name() {
        let wasm = wat::parse_str(
            r#"(module
                (@custom "happyview.handlers" "xrpc.query:app.test.echo\techo\n")
                (@custom "happyview.handlers" "job.run:sweep\tsweep\nrecord.index:app.test.thing\tindex_thing\n")
                (@custom "happyview.interface" "1"))"#,
        )
        .unwrap();
        let sections = read_sections(&wasm).unwrap();
        assert_eq!(
            sections.handlers,
            vec![
                handler("xrpc.query:app.test.echo", "echo"),
                handler("job.run:sweep", "sweep"),
                handler("record.index:app.test.thing", "index_thing"),
            ]
        );
        assert_eq!(
            sections.interface.as_deref(),
            Some(SCRIPT_INTERFACE_VERSION)
        );
        assert!(!sections.uses_exceptions);
    }

    #[test]
    fn a_line_without_a_function_name_still_names_its_trigger() {
        let wasm =
            wat::parse_str(r#"(module (@custom "happyview.handlers" "xrpc.query:a.b.c\n\n"))"#)
                .unwrap();
        assert_eq!(
            read_sections(&wasm).unwrap().handlers,
            vec![ModuleHandler {
                trigger: "xrpc.query:a.b.c".into(),
                function: None,
            }]
        );
    }

    #[test]
    fn absent_sections_read_as_absent() {
        let sections = read_sections(&wat::parse_str("(module)").unwrap()).unwrap();
        assert_eq!(sections, ModuleSections::default());
    }

    #[test]
    fn a_tag_section_is_reported() {
        let wasm = wat::parse_str("(module (tag))").unwrap();
        assert!(read_sections(&wasm).unwrap().uses_exceptions);
    }

    #[test]
    fn an_imported_tag_is_reported() {
        let wasm = wat::parse_str(r#"(module (import "env" "t" (tag)))"#).unwrap();
        assert!(read_sections(&wasm).unwrap().uses_exceptions);
    }

    #[test]
    fn a_section_that_is_not_utf8_is_refused_by_name() {
        let wasm = wat::parse_str(r#"(module (@custom "happyview.handlers" "\ff"))"#).unwrap();
        let err = read_sections(&wasm).unwrap_err();
        assert!(err.contains("happyview.handlers"), "{err}");
    }

    #[test]
    fn bytes_that_are_not_wasm_are_refused() {
        assert!(
            read_sections(b"not wasm")
                .unwrap_err()
                .contains("invalid WASM")
        );
    }

    #[test]
    fn a_truncated_module_is_refused_without_panicking() {
        let wasm = wat::parse_str(
            r#"(module (@custom "happyview.handlers" "xrpc.query:a.b.c\tf\n") (@custom "happyview.interface" "1"))"#,
        )
        .unwrap();
        for cut in 0..wasm.len() {
            let _ = read_sections(&wasm[..cut]);
        }
        assert!(
            read_sections(&wasm[..wasm.len() - 3])
                .unwrap_err()
                .contains("invalid WASM")
        );
    }

    #[test]
    fn a_second_interface_section_is_refused_even_when_the_split_reads_as_one() {
        let wasm = wat::parse_str(
            r#"(module (@custom "happyview.interface" "") (@custom "happyview.interface" "1"))"#,
        )
        .unwrap();
        let err = read_sections(&wasm).unwrap_err();
        assert!(err.contains("more than one happyview.interface"), "{err}");
    }

    #[test]
    fn a_component_is_refused() {
        let wasm = wat::parse_str("(component)").unwrap();
        assert!(read_sections(&wasm).unwrap_err().contains("component"));
    }

    #[test]
    fn a_leading_tab_does_not_shift_the_trigger() {
        let wasm = wat::parse_str(r#"(module (@custom "happyview.handlers" "\tfn\n"))"#).unwrap();
        assert_eq!(
            read_sections(&wasm).unwrap().handlers,
            vec![ModuleHandler {
                trigger: "".into(),
                function: Some("fn".into())
            }]
        );
    }

    #[test]
    fn a_tab_with_no_function_name_is_refused() {
        let wasm = wat::parse_str(r#"(module (@custom "happyview.handlers" "trig\t\n"))"#).unwrap();
        let err = read_sections(&wasm).unwrap_err();
        assert!(err.contains("no function name"), "{err}");
    }

    #[test]
    fn an_oversized_handlers_section_is_refused() {
        let body = "a".repeat(MAX_SECTION_BYTES + 1);
        let wasm = wat::parse_str(format!(
            r#"(module (@custom "happyview.handlers" "{body}"))"#
        ))
        .unwrap();
        assert!(
            read_sections(&wasm)
                .unwrap_err()
                .contains("total more than")
        );
    }

    #[test]
    fn a_line_over_the_cap_is_refused() {
        let body = "a".repeat(MAX_LINE_BYTES + 1);
        let wasm = wat::parse_str(format!(
            r#"(module (@custom "happyview.handlers" "{body}"))"#
        ))
        .unwrap();
        assert!(read_sections(&wasm).unwrap_err().contains("longer than"));
    }

    #[test]
    fn too_many_handlers_are_refused() {
        let body = "t\\n".repeat(MAX_HANDLERS + 1);
        let wasm = wat::parse_str(format!(
            r#"(module (@custom "happyview.handlers" "{body}"))"#
        ))
        .unwrap();
        assert!(
            read_sections(&wasm)
                .unwrap_err()
                .contains("names more than")
        );
    }

    #[test]
    fn an_oversized_interface_is_refused() {
        let body = "1".repeat(MAX_INTERFACE_BYTES + 1);
        let wasm = wat::parse_str(format!(
            r#"(module (@custom "happyview.interface" "{body}"))"#
        ))
        .unwrap();
        assert!(
            read_sections(&wasm)
                .unwrap_err()
                .contains("happyview.interface")
        );
    }
}
