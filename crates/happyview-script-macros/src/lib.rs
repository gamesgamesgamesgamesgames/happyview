//! The attributes that turn an `async fn` into a HappyView handler, and the
//! `export!` that makes a crate a script module. Used through
//! `happyview-script`, which re-exports them.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote, quote_spanned};
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::{parse2, FnArg, ItemFn, LitByteStr, LitStr, Path, Token};

/// Kept equal to `happyview_script::INTERFACE_VERSION`; a test there pins it.
const INTERFACE_VERSION: &str = "1";

#[derive(Clone, Copy)]
enum Kind {
    Query,
    Procedure,
    RecordIndex,
    RecordCreate,
    RecordUpdate,
    RecordDelete,
    Job,
    Label,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Query => "xrpc.query",
            Kind::Procedure => "xrpc.procedure",
            Kind::RecordIndex => "record.index",
            Kind::RecordCreate => "record.create",
            Kind::RecordUpdate => "record.update",
            Kind::RecordDelete => "record.delete",
            Kind::Job => "job.run",
            Kind::Label => "labeler.apply",
        }
    }

    /// The suffix checks that cannot drift from the host: an NSID through
    /// the same crate the host uses. A job type's rules are the host's own,
    /// and the snapshot build applies them.
    fn check(self, suffix: &str) -> Result<(), String> {
        match self {
            Kind::Job if suffix.is_empty() => Err("a job type cannot be empty".into()),
            Kind::Job => Ok(()),
            Kind::Label if suffix == "_actor" => Ok(()),
            _ => happyview_nsid::validate_nsid(suffix).map_err(|e| e.to_string()),
        }
    }
}

macro_rules! attribute {
    ($name:ident, $kind:expr) => {
        #[proc_macro_attribute]
        pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
            handler($kind, attr.into(), item.into()).into()
        }
    };
}

attribute!(query, Kind::Query);
attribute!(procedure, Kind::Procedure);
attribute!(record_index, Kind::RecordIndex);
attribute!(record_create, Kind::RecordCreate);
attribute!(record_update, Kind::RecordUpdate);
attribute!(record_delete, Kind::RecordDelete);
attribute!(job, Kind::Job);
attribute!(label, Kind::Label);

fn handler(kind: Kind, attr: TokenStream2, item: TokenStream2) -> TokenStream2 {
    let func: ItemFn = match parse2(item.clone()) {
        Ok(func) => func,
        Err(e) => return rejected(e, &item),
    };
    // A refused handler keeps its function, so the one error is all the
    // author sees: no "cannot find function" at its callers, no unused
    // imports from its signature.
    let suffix: LitStr = match parse2(attr) {
        Ok(lit) => lit,
        Err(e) => {
            return rejected(
                syn::Error::new(
                    e.span(),
                    format!(
                        "{e}: a handler attribute takes one string literal, \
                         e.g. #[query(\"app.example.getThing\")]"
                    ),
                ),
                &func,
            )
        }
    };
    if let Err(e) = kind.check(&suffix.value()) {
        let message = format!(
            "'{}' is not a valid {} suffix: {e}",
            suffix.value(),
            kind.prefix()
        );
        return rejected(syn::Error::new(suffix.span(), message), &func);
    }
    if func.sig.asyncness.is_none() {
        return rejected(
            syn::Error::new(func.sig.fn_token.span, "a handler must be an async fn"),
            &func,
        );
    }
    if func.sig.inputs.len() != 2
        || func
            .sig
            .inputs
            .iter()
            .any(|a| matches!(a, FnArg::Receiver(_)))
    {
        return rejected(
            syn::Error::new(func.sig.ident.span(), "a handler takes (input, ctx)"),
            &func,
        );
    }

    let name = &func.sig.ident;
    let trigger = format!("{}:{}", kind.prefix(), suffix.value());
    // `r#type` is the function `type`, and the dashboard shows this name.
    let shown = name.unraw().to_string();
    let line = LitByteStr::new(
        format!("{trigger}\t{shown}\n").as_bytes(),
        Span::call_site(),
    );
    let len = trigger.len() + shown.len() + 2;
    let section = format_ident!("__happyview_section_{}", name);
    let run = format_ident!("__happyview_run_{}", name);
    // rustc reports no dead code on a name starting with `_`, nor on an
    // item spanned inside a macro expansion, so the descriptor has neither:
    // its `deny(dead_code)` is what fails a handler missing from `export!`.
    let descriptor = format_ident!("happyview_handler_{}", name, span = name.span());

    let descriptor_item = quote_spanned! {name.span()=>
        #[doc(hidden)]
        #[deny(dead_code)]
        #[allow(non_upper_case_globals)]
        pub(crate) const #descriptor: ::happyview_script::Handler = ::happyview_script::Handler {
            trigger: #trigger,
            run: #run,
        };
    };

    quote! {
        #func

        // One line per handler; the linker concatenates every static placed
        // in this section, which is why the format is lines.
        #[used]
        #[allow(non_upper_case_globals)]
        #[cfg_attr(target_arch = "wasm32", link_section = "happyview.handlers")]
        static #section: [u8; #len] = *#line;

        #[doc(hidden)]
        fn #run(input: ::happyview_script::Value, ctx: ::happyview_script::Ctx) -> ::happyview_script::HandlerFuture {
            ::std::boxed::Box::pin(async move {
                let input = ::happyview_script::__private::decode_input(input)?;
                ::happyview_script::IntoOutput::into_output(#name(input, ctx).await?)
            })
        }

        #descriptor_item
    }
}

/// `export!(handler, module::other_handler, ...)`: the module's exports, its
/// dispatch table and its interface section. Exactly once per crate.
#[proc_macro]
pub fn export(input: TokenStream) -> TokenStream {
    export_impl(input.into()).into()
}

fn export_impl(input: TokenStream2) -> TokenStream2 {
    let paths =
        match syn::parse::Parser::parse2(Punctuated::<Path, Token![,]>::parse_terminated, input) {
            Ok(paths) => paths,
            Err(e) => return e.to_compile_error(),
        };
    let descriptors: Vec<Path> = paths
        .iter()
        .map(|path| {
            let mut path = path.clone();
            let last = path.segments.last_mut().expect("a path has a last segment");
            last.ident = format_ident!("happyview_handler_{}", last.ident);
            path
        })
        .collect();
    let version = LitByteStr::new(INTERFACE_VERSION.as_bytes(), Span::call_site());
    let version_len = INTERFACE_VERSION.len();
    quote! {
        #[used]
        #[cfg_attr(target_arch = "wasm32", link_section = "happyview.interface")]
        static __HAPPYVIEW_INTERFACE: [u8; #version_len] = *#version;

        static __HAPPYVIEW_HANDLERS: &[::happyview_script::Handler] = &[#(#descriptors),*];

        // Dispatch takes the first handler for a trigger, so a second one
        // would never run; refusing it at compile time says so.
        const _: () = {
            const TRIGGERS: &[&str] = &[#(#descriptors.trigger),*];
            const DUPLICATE: ::core::option::Option<usize> =
                ::happyview_script::__private::duplicate_trigger(TRIGGERS);
            const LEN: usize = ::happyview_script::__private::duplicate_message_len(TRIGGERS, DUPLICATE);
            const MESSAGE: [u8; LEN] = ::happyview_script::__private::duplicate_message(TRIGGERS, DUPLICATE);
            if DUPLICATE.is_some() {
                match ::core::str::from_utf8(&MESSAGE) {
                    ::core::result::Result::Ok(message) => ::core::panic!("{}", message),
                    ::core::result::Result::Err(_) => ::core::panic!("two handlers in export! answer the same trigger"),
                }
            }
        };

        fn __happyview_execute(
            input: &::happyview_script::__private::ExecuteInput,
        ) -> ::core::result::Result<::happyview_script::__private::ExecuteOutput, ::happyview_script::__private::PluginError> {
            ::happyview_script::__private::execute(input, __HAPPYVIEW_HANDLERS)
        }

        fn __happyview_validate(
            input: &::happyview_script::__private::ValidateInput,
        ) -> ::core::result::Result<::happyview_script::__private::ValidateOutput, ::happyview_script::__private::PluginError> {
            ::happyview_script::__private::validate(input)
        }

        ::happyview_script::__private::interpreter_plugin! {
            info: ::happyview_script::__private::PluginInfo::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ),
            execute: __happyview_execute,
            validate: __happyview_validate,
        }
    }
}

fn rejected(error: syn::Error, item: &impl quote::ToTokens) -> TokenStream2 {
    let error = error.to_compile_error();
    quote! { #error #item }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn expand(
        kind: Kind,
        attr: proc_macro2::TokenStream,
        item: proc_macro2::TokenStream,
    ) -> String {
        handler(kind, attr, item).to_string()
    }

    #[test]
    fn a_query_emits_its_section_line_and_descriptor() {
        let out = expand(
            Kind::Query,
            quote!("app.test.echo"),
            quote!(
                async fn echo(input: Value, ctx: Ctx) -> Result<Value> {
                    Ok(input)
                }
            ),
        );
        assert!(
            out.contains("link_section = \"happyview.handlers\""),
            "{out}"
        );
        assert!(
            out.contains(r#"b"xrpc.query:app.test.echo\techo\n""#),
            "{out}"
        );
        assert!(out.contains("const happyview_handler_echo"), "{out}");
        assert!(out.contains("deny (dead_code)"), "{out}");
        assert!(
            out.contains("trigger : \"xrpc.query:app.test.echo\""),
            "{out}"
        );
    }

    #[test]
    fn each_kind_maps_to_its_trigger_prefix() {
        for (kind, suffix, trigger) in [
            (
                Kind::Procedure,
                "app.test.make",
                "xrpc.procedure:app.test.make",
            ),
            (
                Kind::RecordIndex,
                "app.test.thing",
                "record.index:app.test.thing",
            ),
            (
                Kind::RecordCreate,
                "app.test.thing",
                "record.create:app.test.thing",
            ),
            (
                Kind::RecordUpdate,
                "app.test.thing",
                "record.update:app.test.thing",
            ),
            (
                Kind::RecordDelete,
                "app.test.thing",
                "record.delete:app.test.thing",
            ),
            (Kind::Job, "sweep.daily", "job.run:sweep.daily"),
            (Kind::Label, "_actor", "labeler.apply:_actor"),
            (
                Kind::Label,
                "app.bsky.feed.post",
                "labeler.apply:app.bsky.feed.post",
            ),
        ] {
            let out = expand(
                kind,
                quote!(#suffix),
                quote!(
                    async fn h(i: Value, c: Ctx) -> Result<()> {
                        Ok(())
                    }
                ),
            );
            assert!(
                out.contains(&format!("trigger : \"{trigger}\"")),
                "{trigger}: {out}"
            );
        }
    }

    #[test]
    fn an_invalid_nsid_is_a_compile_error() {
        let out = expand(
            Kind::Query,
            quote!("not an nsid"),
            quote!(
                async fn h(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(out.contains("compile_error"), "{out}");
    }

    #[test]
    fn a_handler_that_is_not_async_is_a_compile_error() {
        let out = expand(
            Kind::Query,
            quote!("app.test.echo"),
            quote!(
                fn h(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(
            out.contains("compile_error") && out.contains("async fn"),
            "{out}"
        );
    }

    #[test]
    fn a_refused_handler_keeps_its_function() {
        let out = expand(
            Kind::Query,
            quote!("app.test.echo"),
            quote!(
                fn kept(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(out.contains("compile_error"), "{out}");
        assert!(out.contains("fn kept (i : Value , c : Ctx)"), "{out}");
        let out = expand(
            Kind::Query,
            quote!("not an nsid"),
            quote!(
                async fn kept(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(out.contains("async fn kept"), "{out}");
        let out = expand(
            Kind::Query,
            quote!(app.test.echo),
            quote!(
                async fn kept(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(out.contains("one string literal"), "{out}");
        assert!(out.contains("async fn kept"), "{out}");
    }

    #[test]
    fn a_raw_identifier_handler_is_listed_without_its_prefix() {
        let out = expand(
            Kind::Query,
            quote!("app.test.echo"),
            quote!(
                async fn r#type(i: Value, c: Ctx) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(
            out.contains(r#"b"xrpc.query:app.test.echo\ttype\n""#),
            "{out}"
        );
        assert!(out.contains("[u8 ; 30usize]"), "{out}");
        assert!(out.contains("const happyview_handler_type"), "{out}");
    }

    #[test]
    fn a_handler_with_the_wrong_arguments_is_a_compile_error() {
        let out = expand(
            Kind::Query,
            quote!("app.test.echo"),
            quote!(
                async fn h(i: Value) -> Result<()> {
                    Ok(())
                }
            ),
        );
        assert!(
            out.contains("compile_error") && out.contains("(input, ctx)"),
            "{out}"
        );
    }

    #[test]
    fn export_lists_each_descriptor_by_its_path_and_emits_the_interface() {
        let out = export_impl(quote!(echo, handlers::index_thing)).to_string();
        assert!(out.contains("[happyview_handler_echo ,"), "{out}");
        assert!(
            out.contains("handlers :: happyview_handler_index_thing"),
            "{out}"
        );
        assert!(
            out.contains("link_section = \"happyview.interface\""),
            "{out}"
        );
        assert!(out.contains(r#"b"1""#), "{out}");
        assert!(out.contains("interpreter_plugin"), "{out}");
        assert!(out.contains("duplicate_trigger"), "{out}");
    }
}
