use crate::condition::{parse_conditions, replace_fields};
use crate::ident;
use crate::isa::{modifiers_iter, modifiers_valid, HexLiteral, Isa, Opcode};
use anyhow::{bail, ensure, Result};
use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use std::collections::HashMap;
use std::num::NonZeroU8;

pub fn gen_disasm(isa: &Isa, max_args: usize) -> Result<TokenStream> {
    // The entry table allows us to quickly find the range of possible opcodes
    // for a given 6-bit prefix. 2*64 bytes should fit in a cache line (or two).
    struct OpcodeEntry {
        start: u8,
        count: u8,
    }
    let mut sorted_ops = Vec::<Opcode>::new();
    let mut entries = Vec::<OpcodeEntry>::new();
    for i in 0..64 {
        let mut entry = OpcodeEntry { start: 0, count: 0 };
        for opcode in &isa.opcodes {
            if (opcode.pattern >> 26) as u8 == i {
                if entry.count == 0 {
                    entry.start = sorted_ops.len() as u8;
                }
                // Sanity check for duplicate opcodes
                if sorted_ops.iter().any(|op| op.name == opcode.name) {
                    bail!("Duplicate opcode: {}", opcode.name);
                }
                sorted_ops.push(opcode.clone());
                entry.count += 1;
            }
        }
        if entry.count > 1 {
            log::info!("{:#X}: {} opcodes", i, entry.count);
        } else if let Some(op) = (entry.count == 1).then(|| &sorted_ops[entry.start as usize]) {
            log::info!("{:#X}: {}", i, op.name);
        } else {
            log::info!("{:#X}: <invalid>", i);
        }
        entries.push(entry);
    }
    ensure!(sorted_ops.len() == isa.opcodes.len());

    // Generate the opcode entries table
    let mut opcode_entries = TokenStream::new();
    for entry in &entries {
        let start = Literal::u8_unsuffixed(entry.start);
        let end = Literal::u8_unsuffixed(entry.start + entry.count);
        opcode_entries.extend(quote! { (#start, #end), });
    }

    // Generate the opcode tables
    let mut opcode_patterns = TokenStream::new();
    let mut opcode_enum = TokenStream::new();
    let mut opcode_names = TokenStream::new();
    for (idx, opcode) in sorted_ops.iter().enumerate() {
        let bitmask = HexLiteral(opcode.mask(isa));
        let pattern = HexLiteral(opcode.pattern);
        let enum_idx = Literal::u8_unsuffixed(idx as u8);
        let name = &opcode.name;
        opcode_patterns.extend(quote! { (#bitmask, #pattern), });
        opcode_names.extend(quote! { #name, });
        let doc = opcode.doc();
        let variant = opcode.variant();
        opcode_enum.extend(quote! {
            #[doc = #doc]
            #variant = #enum_idx,
        });
    }

    // Generate field and modifier accessors
    let mut ins_fields = TokenStream::new();
    for field in &isa.fields {
        let Some(bits) = field.bits else {
            continue;
        };
        // TODO get rid of .nz hack
        if field.name.ends_with(".nz") {
            continue;
        }

        let mut shift_right = bits.shift();
        let mut shift_left = field.shift_left;
        if shift_right == shift_left {
            // Optimization: these cancel each other out
            // Adjust subsequent operations to operate on the full value
            shift_right = 0;
            shift_left = 0;
        }

        // Shift right and mask
        let mut inner = quote! { self.code };
        if shift_right > 0 {
            let shift = Literal::u8_unsuffixed(shift_right);
            inner = quote! { (#inner >> #shift) };
        }
        let mask = HexLiteral(bits.mask() >> shift_right);
        inner = quote! { #inner & #mask };

        // Determine the smallest integer type that can hold the value
        let num_bits = bits.len() + field.shift_left;
        let (out_type, cast, sign_shift) = match (num_bits, field.signed) {
            (1..=8, false) => (ident!(u8), true, None),
            (9..=16, false) => (ident!(u16), true, None),
            (17..=32, false) => (ident!(u32), false, None),
            (1..=8, true) => (ident!(i8), true, NonZeroU8::new(8 - num_bits)),
            (9..=16, true) => (ident!(i16), true, NonZeroU8::new(16 - num_bits)),
            (17..=32, true) => (ident!(i32), true, NonZeroU8::new(32 - num_bits)),
            (v, _) => bail!("Unsupported field size {v}"),
        };

        // Handle sign extension
        if let Some(sign_shift) = sign_shift {
            let sign_shift = Literal::u8_unsuffixed(sign_shift.get());
            inner = quote! { (((#inner) << #sign_shift) as #out_type) >> #sign_shift };
        } else if cast {
            inner = quote! { (#inner) as #out_type };
        }

        // Handle left shift
        if shift_left > 0 {
            let shift_left = Literal::u8_unsuffixed(shift_left);
            inner = quote! { (#inner) << #shift_left };
        }

        // Swap 5-bit halves (SPR, TBR)
        if field.split {
            inner = quote! {
                let value = #inner;
                ((value & 0b11111_00000) >> 5) | ((value & 0b00000_11111) << 5)
            };
        }

        // Write the accessor
        let doc = field.doc();
        ins_fields.extend(quote! {
            #[doc = #doc]
            #[inline(always)]
            pub const fn #field(&self) -> #out_type { #inner }
        });
    }
    for modifier in &isa.modifiers {
        let mask = HexLiteral(modifier.mask());
        let mut inner = quote! { (self.code & #mask) == #mask };
        if let Some(condition) = &modifier.condition {
            for condition in parse_conditions(condition, isa)? {
                let stream = condition.to_token_stream(isa, ident!(self))?;
                inner.extend(quote! { && #stream });
            }
        };

        // Write the accessor
        let doc = modifier.doc();
        ins_fields.extend(quote! {
            #[doc = #doc]
            #[inline(always)]
            pub const fn #modifier(&self) -> bool { #inner }
        });
    }

    // Generate simplified mnemonics
    let mut mnemonic_functions = TokenStream::new();
    let mut basic_functions_ref = TokenStream::new();
    let mut simplified_functions_ref = TokenStream::new();
    for opcode in &sorted_ops {
        let mnemonics =
            isa.mnemonics.iter().filter(|m| m.opcode == opcode.name).collect::<Vec<_>>();
        let mut mnemonic_conditions = TokenStream::new();

        // Generate conditions for each simplified mnemonic
        for mnemonic in &mnemonics {
            let conditions = parse_conditions(&mnemonic.condition, isa)?
                .iter()
                .map(|c| c.to_token_stream(isa, ident!(ins)))
                .collect::<Result<Vec<TokenStream>>>()?;
            let modifiers = mnemonic.modifiers.as_deref().unwrap_or(&opcode.modifiers);
            let inner = gen_mnemonic(
                &mnemonic.name,
                &mnemonic.args,
                modifiers,
                isa,
                max_args,
                Some(&mnemonic.replace),
            )?;
            mnemonic_conditions.extend(quote! {
                if #(#conditions)&&* {
                    *out = #inner;
                    return;
                }
            });
        }

        // Fallback to the basic opcode name if no mnemonic matches
        let inner =
            gen_mnemonic(&opcode.name, &opcode.args, &opcode.modifiers, isa, max_args, None)?;
        let basic_name = format_ident!("basic_{}", opcode.ident());
        if mnemonics.is_empty() {
            mnemonic_functions.extend(quote! {
                fn #basic_name(out: &mut ParsedIns, ins: Ins) {
                    *out = #inner;
                }
            });
            basic_functions_ref.extend(quote! { #basic_name, });
            simplified_functions_ref.extend(quote! { #basic_name, });
        } else {
            let simplified_name = format_ident!("simplified_{}", opcode.ident());
            mnemonic_functions.extend(quote! {
                fn #basic_name(out: &mut ParsedIns, ins: Ins) {
                    *out = #inner;
                }
                fn #simplified_name(out: &mut ParsedIns, ins: Ins) {
                    #mnemonic_conditions
                    #basic_name(out, ins)
                }
            });
            basic_functions_ref.extend(quote! { #basic_name, });
            simplified_functions_ref.extend(quote! { #simplified_name, });
        }
    }
    mnemonic_functions.extend(quote! {
        fn mnemonic_illegal(out: &mut ParsedIns, _ins: Ins) {
            *out = ParsedIns::new();
        }
    });

    // TODO rework defs/uses to account for modifiers and special registers (CTR, LR, etc)
    let mut defs_uses_functions = TokenStream::new();
    let mut defs_refs = TokenStream::new();
    let mut uses_refs = TokenStream::new();
    for opcode in &sorted_ops {
        let mut defs = TokenStream::new();
        let mut uses = TokenStream::new();
        let mut defs_count = 0;
        for def in &opcode.defs {
            if isa.find_field(def).is_some_and(|f| f.arg.is_none()) {
                continue;
            }
            let arg = gen_argument(def, isa, None)?;
            defs.extend(quote! { #arg, });
            defs_count += 1;
        }
        let mut use_count = 0;
        for use_ in &opcode.uses {
            if let Some(use_) = use_.strip_suffix(".nz") {
                let Some(field) = isa.find_field(use_) else { bail!("Unknown field {}", use_) };
                let ident = field.ident();
                let arg = gen_argument(use_, isa, None)?;
                uses.extend(quote! { if ins.#ident() != 0 { #arg } else { Argument::None }, });
                use_count += 1;
                continue;
            } else if isa.find_field(use_).is_some_and(|f| f.arg.is_none()) {
                continue;
            }
            let arg = gen_argument(use_, isa, None)?;
            uses.extend(quote! { #arg, });
            use_count += 1;
        }

        if defs_count > 0 {
            for _ in defs_count..max_args {
                defs.extend(quote! { Argument::None, });
            }
            let defs_name = format_ident!("defs_{}", opcode.ident());
            defs_uses_functions.extend(quote! {
                fn #defs_name(out: &mut Arguments, ins: Ins) { *out = [#defs]; }
            });
            defs_refs.extend(quote! { #defs_name, });
        } else {
            defs_refs.extend(quote! { defs_uses_empty, });
        }

        if use_count > 0 {
            for _ in use_count..max_args {
                uses.extend(quote! { Argument::None, });
            }
            let uses_name = format_ident!("uses_{}", opcode.ident());
            defs_uses_functions.extend(quote! {
                fn #uses_name(out: &mut Arguments, ins: Ins) { *out = [#uses]; }
            });
            uses_refs.extend(quote! { #uses_name, });
        } else {
            uses_refs.extend(quote! { defs_uses_empty, });
        }
    }
    defs_uses_functions.extend(quote! {
        fn defs_uses_empty(out: &mut Arguments, _ins: Ins) { *out = EMPTY_ARGS; }
    });

    // Filling the tables to 256 entries to avoid bounds checks
    for _ in sorted_ops.len()..256 {
        opcode_patterns.extend(quote! { (0, 0), });
        opcode_names.extend(quote! { "<illegal>", });
        basic_functions_ref.extend(quote! { mnemonic_illegal, });
        simplified_functions_ref.extend(quote! { mnemonic_illegal, });
        defs_refs.extend(quote! { defs_uses_empty, });
        uses_refs.extend(quote! { defs_uses_empty, });
    }

    let mut none_args = TokenStream::new();
    for _ in 0..max_args {
        none_args.extend(quote! { Argument::None, });
    }

    let max_args = Literal::usize_unsuffixed(max_args);
    Ok(quote! {
        #![allow(unused)]
        #![cfg_attr(rustfmt, rustfmt_skip)]
        #[comment = " Code generated by ppc750-genisa. DO NOT EDIT."]
        use crate::disasm::*;
        #[doc = " The entry table allows us to quickly find the range of possible opcodes for a"]
        #[doc = " given 6-bit prefix. 2*64 bytes should fit in a cache line (or two)."]
        static OPCODE_ENTRIES: [(u8, u8); 64] = [#opcode_entries];
        #[doc = " The bitmask and pattern for each opcode."]
        static OPCODE_PATTERNS: [(u32, u32); 256] = [#opcode_patterns];
        #[doc = " The name of each opcode."]
        static OPCODE_NAMES: [&str; 256] = [#opcode_names];

        #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
        #[repr(u8)]
        #[non_exhaustive]
        pub enum Opcode {
            #[doc = " An illegal or unknown opcode"]
            #[default]
            Illegal = u8::MAX,
            #opcode_enum
        }
        impl Opcode {
            #[inline]
            pub fn _mnemonic(self) -> &'static str {
                OPCODE_NAMES[self as usize]
            }

            #[inline]
            pub fn _detect(code: u32) -> Self {
                let entry = OPCODE_ENTRIES[(code >> 26) as usize];
                for i in entry.0..entry.1 {
                    let pattern = OPCODE_PATTERNS[i as usize];
                    if (code & pattern.0) == pattern.1 {
                        #[comment = " Safety: The enum is repr(u8) and marked non_exhaustive"]
                        return unsafe { core::mem::transmute::<u8, Opcode>(i) };
                    }
                }
                Self::Illegal
            }
        }

        impl Ins {
            #ins_fields
        }

        pub type Arguments = [Argument; #max_args];
        pub const EMPTY_ARGS: Arguments = [#none_args];

        type MnemonicFunction = fn(&mut ParsedIns, Ins);
        #mnemonic_functions
        static BASIC_MNEMONICS: [MnemonicFunction; 256] = [#basic_functions_ref];
        #[inline]
        pub fn parse_basic(out: &mut ParsedIns, ins: Ins) {
            BASIC_MNEMONICS[ins.op as usize](out, ins)
        }
        static SIMPLIFIED_MNEMONICS: [MnemonicFunction; 256] = [#simplified_functions_ref];
        #[inline]
        pub fn parse_simplified(out: &mut ParsedIns, ins: Ins) {
            SIMPLIFIED_MNEMONICS[ins.op as usize](out, ins)
        }

        type DefsUsesFunction = fn(&mut Arguments, Ins);
        #defs_uses_functions
        static DEFS_FUNCTIONS: [DefsUsesFunction; 256] = [#defs_refs];
        #[inline]
        pub fn parse_defs(out: &mut Arguments, ins: Ins) {
            DEFS_FUNCTIONS[ins.op as usize](out, ins)
        }
        static USES_FUNCTIONS: [DefsUsesFunction; 256] = [#uses_refs];
        #[inline]
        pub fn parse_uses(out: &mut Arguments, ins: Ins) {
            USES_FUNCTIONS[ins.op as usize](out, ins)
        }
    })
}

fn modifier_names(name: &str, modifiers: &[String], isa: &Isa) -> Vec<String> {
    // For every combination of modifiers, generate a name
    let mut names = Vec::with_capacity(1 << modifiers.len());
    for v in modifiers_iter(modifiers, isa) {
        if modifiers_valid(&v) {
            let mut name = name.to_string();
            for modifier in &v {
                name.push(modifier.suffix);
            }
            names.push(name);
        } else {
            names.push("<illegal>".to_string());
        }
    }
    names
}

fn gen_argument(field: &str, isa: &Isa, replace: Option<&String>) -> Result<TokenStream> {
    let Some(field) = isa.find_field(field) else { bail!("Unknown field {}", field) };
    let Some(arg) = &field.arg else { bail!("Field {} has no argument", field.name) };
    let value = if let Some(replace) = replace {
        let stream = replace_fields(replace, isa, |f| Ok(quote! { ins.#f() }))?;
        quote! { (#stream) }
    } else {
        quote! { ins.#field() }
    };
    let arg = format_ident!("{}", arg);
    Ok(quote! { Argument::#arg(#arg(#value as _)) })
}

fn gen_mnemonic(
    name: &str,
    args: &[String],
    modifiers: &[String],
    isa: &Isa,
    max_args: usize,
    replace: Option<&HashMap<String, String>>,
) -> Result<TokenStream> {
    let arguments = if args.is_empty() {
        quote! { EMPTY_ARGS }
    } else {
        let mut inner = TokenStream::new();
        for field in args {
            let arg = gen_argument(field, isa, replace.and_then(|m| m.get(field)))?;
            inner.extend(quote! { #arg, });
        }
        for _ in args.len()..max_args {
            inner.extend(quote! { Argument::None, });
        }
        quote! { [#inner] }
    };

    if modifiers.is_empty() {
        Ok(quote! { ParsedIns { mnemonic: #name, args: #arguments } })
    } else {
        let names = modifier_names(name, modifiers, isa);
        let mut bitset = quote! { 0 };
        for (i, modifier) in modifiers.iter().enumerate() {
            let modifier = isa.find_modifier(modifier).unwrap();
            if i == 0 {
                bitset = quote! { ins.#modifier() as usize };
            } else {
                let i = Literal::u8_unsuffixed(i as u8);
                bitset.extend(quote! { | (ins.#modifier() as usize) << #i });
            }
        }
        Ok(quote! { ParsedIns { mnemonic: [#(#names),*][#bitset], args: #arguments } })
    }
}
