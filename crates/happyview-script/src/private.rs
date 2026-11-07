//! What `export!` and the handler attributes expand to call. Not an API.

use std::sync::Once;

pub use happyview_plugin_sdk::interpreter_plugin;
use happyview_plugin_sdk::wire::PanicReport;
use happyview_plugin_sdk::{host, ScriptErrorKind, ValidateError, Value};
pub use happyview_plugin_sdk::{
    ExecuteInput, ExecuteOutput, PluginError, PluginInfo, ValidateInput, ValidateOutput,
};
use serde::de::DeserializeOwned;

use crate::executor::block_on;
use crate::{Ctx, Error, Handler, Output};

/// A handler's input in the type it asked for.
pub fn decode_input<T: DeserializeOwned>(input: Value) -> Result<T, Error> {
    serde_json::from_value(input).map_err(|e| {
        Error::msg(format!(
            "input does not match the handler's input type: {e}"
        ))
    })
}

const DUPLICATE_PREFIX: &str = "two handlers in export! answer ";

/// The index of the first handler whose trigger an earlier one already
/// answers. Const, because `export!` refuses a duplicate at compile time.
pub const fn duplicate_trigger(triggers: &[&str]) -> Option<usize> {
    let mut later = 1;
    while later < triggers.len() {
        let mut earlier = 0;
        while earlier < later {
            if same_bytes(triggers[earlier], triggers[later]) {
                return Some(later);
            }
            earlier += 1;
        }
        later += 1;
    }
    None
}

pub const fn duplicate_message_len(triggers: &[&str], duplicate: Option<usize>) -> usize {
    match duplicate {
        Some(i) => DUPLICATE_PREFIX.len() + triggers[i].len(),
        None => 0,
    }
}

/// The compile error's text, built in a const array because a const panic
/// formats nothing but a single `&str`.
pub const fn duplicate_message<const N: usize>(
    triggers: &[&str],
    duplicate: Option<usize>,
) -> [u8; N] {
    let mut out = [0u8; N];
    let Some(i) = duplicate else {
        return out;
    };
    let prefix = DUPLICATE_PREFIX.as_bytes();
    let trigger = triggers[i].as_bytes();
    let mut k = 0;
    while k < prefix.len() {
        out[k] = prefix[k];
        k += 1;
    }
    let mut k = 0;
    while k < trigger.len() {
        out[prefix.len() + k] = trigger[k];
        k += 1;
    }
    out
}

const fn same_bytes(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut k = 0;
    while k < a.len() {
        if a[k] != b[k] {
            return false;
        }
        k += 1;
    }
    true
}

pub fn execute(input: &ExecuteInput, handlers: &[Handler]) -> Result<ExecuteOutput, PluginError> {
    install_panic_hook();
    let trigger = input.context.trigger.as_str();
    let Some(handler) = handlers.iter().find(|h| h.trigger == trigger) else {
        let message = format!("this module has no handler for {trigger}");
        return Ok(ExecuteOutput::Error {
            kind: ScriptErrorKind::MissingHandle,
            raw: message.clone(),
            message,
            line: None,
            chunk: None,
        });
    };
    let outcome = block_on((handler.run)(input.input.clone(), Ctx::new(input))).and_then(|r| r);
    Ok(match outcome {
        Ok(Output { value, kind }) => ExecuteOutput::Returned {
            value,
            value_kind: kind,
        },
        Err(e) => ExecuteOutput::Error {
            kind: ScriptErrorKind::Runtime,
            message: e.to_string(),
            line: None,
            raw: e.to_string(),
            chunk: None,
        },
    })
}

/// A module has no source text, so there is nothing to validate; saying so
/// is truer than answering `valid`.
pub fn validate(_input: &ValidateInput) -> Result<ValidateOutput, PluginError> {
    Ok(ValidateOutput::invalid(vec![ValidateError {
        kind: ScriptErrorKind::Syntax,
        line: None,
        message: "a script module is compiled, so it has no source to validate".into(),
    }]))
}

/// Report a panic's message and location to the host before the guest
/// aborts. The default hook would print to stderr, which no caller reads.
fn install_panic_hook() {
    // Off wasm the report reaches nothing, and replacing the hook would
    // swallow a native test's own panic message.
    if !cfg!(target_arch = "wasm32") {
        return;
    }
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            let (file, line, column) = info
                .location()
                .map(|l| (l.file().to_string(), l.line(), l.column()))
                .unwrap_or_default();
            let message = info
                .payload_as_str()
                .unwrap_or("the handler panicked")
                .to_string();
            host::script_panic(&PanicReport {
                message,
                file,
                line,
                column,
            });
        }));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::with_host;
    use crate::{Ctx, Handler, HandlerFuture, IntoOutput, Json, Value};
    use happyview_plugin_sdk::{
        ExecuteContext, ExecuteLimits, ScriptErrorKind, ScriptKind, ScriptValueKind,
        DEFAULT_CHUNK_NAME,
    };
    use serde_json::json;
    use std::rc::Rc;

    #[derive(serde::Deserialize, serde::Serialize)]
    struct Params {
        q: String,
    }

    fn echo(input: Value, ctx: Ctx) -> HandlerFuture {
        Box::pin(async move {
            let params: Params = decode_input(input)?;
            Json(json!({"q": params.q, "trigger": ctx.trigger()})).into_output()
        })
    }

    fn fails(_input: Value, _ctx: Ctx) -> HandlerFuture {
        Box::pin(async { Err(crate::Error::msg("nope")) })
    }

    fn nothing(_input: Value, _ctx: Ctx) -> HandlerFuture {
        Box::pin(async { ().into_output() })
    }

    const HANDLERS: &[Handler] = &[
        Handler {
            trigger: "xrpc.query:app.test.echo",
            run: echo,
        },
        Handler {
            trigger: "xrpc.query:app.test.fails",
            run: fails,
        },
        Handler {
            trigger: "record.index:app.test.thing",
            run: nothing,
        },
    ];

    fn input(trigger: &str, value: Value) -> ExecuteInput {
        ExecuteInput {
            source: String::new(),
            kind: ScriptKind::XrpcQuery,
            input: value,
            context: ExecuteContext {
                trigger: trigger.into(),
                ..Default::default()
            },
            libraries: vec![],
            limits: ExecuteLimits {
                instructions: Some(1_000_000),
                memory_bytes: 1 << 24,
            },
            removed_globals: vec![],
            chunk_name: DEFAULT_CHUNK_NAME.into(),
            artifact: None,
        }
    }

    #[test]
    fn the_trigger_picks_the_handler() {
        with_host(Rc::new(crate::executor::tests::FakeHost::default()));
        let out = execute(
            &input("xrpc.query:app.test.echo", json!({"q": "hi"})),
            HANDLERS,
        )
        .unwrap();
        assert_eq!(
            out,
            ExecuteOutput::Returned {
                value: json!({"q": "hi", "trigger": "xrpc.query:app.test.echo"}),
                value_kind: ScriptValueKind::Object,
            }
        );
    }

    #[test]
    fn a_returned_error_is_a_runtime_script_error() {
        let out = execute(&input("xrpc.query:app.test.fails", json!({})), HANDLERS).unwrap();
        assert_eq!(
            out,
            ExecuteOutput::Error {
                kind: ScriptErrorKind::Runtime,
                message: "nope".into(),
                line: None,
                raw: "nope".into(),
                chunk: None,
            }
        );
    }

    #[test]
    fn input_that_does_not_fit_the_handler_is_a_runtime_error_naming_why() {
        let out = execute(
            &input("xrpc.query:app.test.echo", json!({"q": 3})),
            HANDLERS,
        )
        .unwrap();
        let ExecuteOutput::Error { kind, message, .. } = out else {
            panic!("{out:?}")
        };
        assert_eq!(kind, ScriptErrorKind::Runtime);
        assert!(message.contains("input does not match"), "{message}");
        assert!(message.contains("invalid type"), "{message}");
    }

    #[test]
    fn returning_nothing_reads_as_none() {
        let out = execute(&input("record.index:app.test.thing", json!({})), HANDLERS).unwrap();
        assert_eq!(
            out,
            ExecuteOutput::Returned {
                value: Value::Null,
                value_kind: ScriptValueKind::None
            }
        );
    }

    #[test]
    fn an_unknown_trigger_is_a_missing_handler() {
        let out = execute(&input("xrpc.query:app.test.other", json!({})), HANDLERS).unwrap();
        let ExecuteOutput::Error { kind, message, .. } = out else {
            panic!("{out:?}")
        };
        assert_eq!(kind, ScriptErrorKind::MissingHandle);
        assert!(message.contains("xrpc.query:app.test.other"), "{message}");
    }

    #[test]
    fn validate_says_a_module_has_no_source() {
        let out = validate(&ValidateInput::default()).unwrap();
        assert!(!out.valid);
        assert!(
            out.errors[0].message.contains("compiled"),
            "{:?}",
            out.errors
        );
    }

    #[test]
    fn a_repeated_trigger_is_found_and_named() {
        const TRIGGERS: &[&str] = &["xrpc.query:a.b.c", "job.run:x", "xrpc.query:a.b.c"];
        const DUPLICATE: Option<usize> = duplicate_trigger(TRIGGERS);
        const LEN: usize = duplicate_message_len(TRIGGERS, DUPLICATE);
        const MESSAGE: [u8; LEN] = duplicate_message(TRIGGERS, DUPLICATE);
        assert_eq!(DUPLICATE, Some(2));
        assert_eq!(
            std::str::from_utf8(&MESSAGE).unwrap(),
            "two handlers in export! answer xrpc.query:a.b.c"
        );
    }

    #[test]
    fn distinct_triggers_are_not_duplicates() {
        assert_eq!(
            duplicate_trigger(&["xrpc.query:a.b.c", "xrpc.query:a.b.cd"]),
            None
        );
        assert_eq!(duplicate_trigger(&[]), None);
        assert_eq!(duplicate_message_len(&["x"], None), 0);
    }

    #[test]
    fn the_macros_speak_this_crates_interface_version() {
        assert_eq!(crate::INTERFACE_VERSION, "1");
    }
}
