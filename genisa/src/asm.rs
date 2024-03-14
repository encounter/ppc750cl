use crate::condition::{parse_conditions, replace_fields, ConditionOp, ConditionValue};
use crate::isa::{
    modifiers_iter, modifiers_valid, to_ident, Field, HexLiteral, Isa, Mnemonic, Opcode,
    SignedHexLiteral,
};
use anyhow::{bail, Context, Result};
use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use std::collections::HashMap;

pub fn gen_asm(isa: &Isa, max_args: usize) -> Result<TokenStream> {
    let mut functions = TokenStream::new();

    let mut func_map = phf_codegen::Map::new();
    for opcode in &isa.opcodes {
        let name = format_ident!("gen_{}", opcode.ident());
        let inner = gen_opcode(opcode, isa)?;
        functions.extend(quote! {
            fn #name(args: &Arguments, modifiers: u32) -> Result<u32, ArgumentError> { #inner }
        });
    }

    let mut mnemonic_map = HashMap::<String, Vec<Mnemonic>>::new();
    for mnemonic in &isa.mnemonics {
        mnemonic_map.entry(mnemonic.name.clone()).or_default().push(mnemonic.clone());
    }
    for (name, mnemonics) in &mnemonic_map {
        let fn_name = format!("gen_{}", to_ident(name));
        let fn_ident = format_ident!("{}", fn_name);
        let mut inner;
        if mnemonics.len() > 1 {
            inner = TokenStream::new();
            let mut max_args = 0;
            for mnemonic in mnemonics {
                let gen = gen_mnemonic(mnemonic, isa, false)?;
                let arg_n = Literal::usize_unsuffixed(mnemonic.args.len());
                inner.extend(quote! {
                    #arg_n => { #gen }
                });
                max_args = max_args.max(mnemonic.args.len());
            }
            let max_args = Literal::usize_unsuffixed(max_args);
            inner.extend(quote! {
                value => Err(ArgumentError::ArgCount { value, expected: #max_args })
            });
            inner = quote! { match arg_count(args) { #inner } };
        } else {
            inner = gen_mnemonic(mnemonics.first().unwrap(), isa, true)?;
        }
        functions.extend(quote! {
            fn #fn_ident(args: &Arguments, modifiers: u32) -> Result<u32, ArgumentError> { #inner }
        });
    }

    for (opcode, modifiers) in isa.opcodes.iter().flat_map(|o| {
        modifiers_iter(&o.modifiers, isa).filter(|m| modifiers_valid(m)).map(move |m| (o, m))
    }) {
        let suffix = modifiers.iter().map(|m| m.suffix).collect::<String>();
        let mut pattern = 0;
        for modifier in &modifiers {
            pattern |= modifier.mask();
        }
        func_map.entry(
            format!("{}{}", opcode.name, suffix),
            &format!("(gen_{}, {:#x})", opcode.ident(), pattern),
        );
    }

    for (mnemonic, modifiers) in mnemonic_map.iter().flat_map(|(_, mnemonics)| {
        let mnemonic = mnemonics.first().unwrap();
        let opcode = isa.find_opcode(&mnemonic.opcode).unwrap();
        let modifiers = mnemonic.modifiers.as_deref().unwrap_or(&opcode.modifiers);
        modifiers_iter(modifiers, isa).filter(|m| modifiers_valid(m)).map(move |m| (mnemonic, m))
    }) {
        let suffix = modifiers.iter().map(|m| m.suffix).collect::<String>();
        let mut pattern = 0;
        for modifier in &modifiers {
            pattern |= modifier.mask();
        }
        func_map.entry(
            format!("{}{}", mnemonic.name, suffix),
            &format!("(gen_{}, {:#x})", to_ident(&mnemonic.name), pattern),
        );
    }

    let func_map = syn::parse_str::<TokenStream>(&func_map.build().to_string())?;
    let max_args = Literal::usize_unsuffixed(max_args);
    Ok(quote! {
        #![allow(unused)]
        #![cfg_attr(rustfmt, rustfmt_skip)]
        use crate::types::*;
        pub type Arguments = [Argument; #max_args];
        #functions
        type MnemonicFn = fn(&Arguments, u32) -> Result<u32, ArgumentError>;
        const MNEMONIC_MAP: phf::Map<&'static str, (MnemonicFn, u32)> = #func_map;
        pub fn assemble(mnemonic: &str, args: &Arguments) -> Result<u32, ArgumentError> {
            if let Some(&(fn_ptr, modifiers)) = MNEMONIC_MAP.get(mnemonic) {
                fn_ptr(args, modifiers)
            } else {
                Err(ArgumentError::UnknownMnemonic)
            }
        }
    })
}

fn gen_parse_field(field: &Field, i: usize) -> Result<(TokenStream, bool)> {
    let Some(bits) = field.bits else { bail!("Field {} has no bits", field.name) };
    let i = Literal::usize_unsuffixed(i);
    Ok(if field.signed {
        let max_value = 1 << (bits.len() - 1 + field.shift_left);
        let min_value = SignedHexLiteral(-max_value);
        let max_value = SignedHexLiteral(max_value);
        (quote! { parse_signed(args, #i, #min_value, #max_value)? }, true)
    } else {
        let min_value = HexLiteral(0);
        let max_value = HexLiteral(bits.max_value() << field.shift_left);
        (quote! { parse_unsigned(args, #i, #min_value, #max_value)? }, false)
    })
}

fn gen_field(
    field: &Field,
    mut accessor: TokenStream,
    finalize: fn(TokenStream) -> TokenStream,
    signed: bool,
) -> Result<TokenStream> {
    let Some(bits) = field.bits else { bail!("Field {} has no bits", field.name) };
    let mut shift_right = bits.shift();
    let mut shift_left = field.shift_left;
    if shift_right == shift_left {
        // Optimization: these cancel each other out
        // Adjust subsequent operations to operate on the full value
        shift_right = 0;
        shift_left = 0;
    }

    // Handle the operations (in reverse order from disassembly)
    let mut operations = TokenStream::new();
    let mut inner;

    if signed {
        accessor = quote! { #accessor as u32 };
    }

    // Swap 5-bit halves (SPR, TBR)
    if field.split {
        operations.extend(quote! {
            value = ((value & 0b11111_00000) >> 5) | ((value & 0b00000_11111) << 5);
        });
        inner = quote! { value };
    } else {
        inner = accessor.clone();
    }

    // Handle left shift
    if shift_left > 0 {
        let shift_left = Literal::u8_unsuffixed(shift_left);
        inner = quote! { (#inner >> #shift_left) };
    }

    // Mask
    let mask = HexLiteral(bits.mask() >> shift_right);
    inner = quote! { #inner & #mask };

    // Shift right
    if shift_right > 0 {
        let shift = Literal::u8_unsuffixed(shift_right);
        inner = quote! { (#inner) << #shift };
    }

    if operations.is_empty() {
        Ok(finalize(inner))
    } else {
        inner = finalize(inner);
        Ok(quote! {{
            let mut value = #accessor;
            #operations
            #inner
        }})
    }
}

fn gen_opcode(opcode: &Opcode, isa: &Isa) -> Result<TokenStream> {
    let mut args = TokenStream::new();
    for (i, arg) in opcode.args.iter().enumerate() {
        let field = isa.find_field(arg).unwrap();
        let comment = format!(" {}", field.name);
        let (accessor, signed) = gen_parse_field(field, i)?;
        let value = gen_field(field, accessor, |s| s, signed)?;
        args.extend(quote! {
            #[comment = #comment]
            code |= #value;
        });
    }

    let arg_count = Literal::usize_unsuffixed(opcode.args.len());
    let pattern = HexLiteral(opcode.pattern);
    Ok(quote! {
        check_arg_count(args, #arg_count)?;
        let mut code = #pattern | modifiers;
        #args
        Ok(code)
    })
}

fn gen_mnemonic(mnemonic: &Mnemonic, isa: &Isa, check_arg_count: bool) -> Result<TokenStream> {
    let Some(opcode) = isa.find_opcode(&mnemonic.opcode) else {
        bail!("Unknown opcode {}", mnemonic.opcode)
    };
    let mut args = TokenStream::new();
    for (i, arg) in mnemonic.args.iter().enumerate() {
        let comment = format!(" {}", arg);
        let arg = gen_argument(&mnemonic.args, i, isa, mnemonic.replace_assemble.get(arg))?;
        args.extend(quote! {
            #[comment = #comment]
            code |= #arg;
        });
    }

    let mut pattern = opcode.pattern;
    for condition in parse_conditions(&mnemonic.condition, isa)? {
        if condition.op == ConditionOp::Eq {
            match condition.value {
                ConditionValue::ConstantUnsigned(value) => {
                    pattern |= condition.field.shift_value(value);
                }
                ConditionValue::ConstantSigned(value) => {
                    pattern |= condition.field.shift_value(value as u32);
                }
                ConditionValue::Field(in_field) => {
                    let comment = format!(" {}", condition.field.name);
                    let arg_n = mnemonic
                        .args
                        .iter()
                        .position(|a| a == &in_field.name)
                        .with_context(|| {
                            format!("Mnemonic {}: unknown field {}", mnemonic.name, in_field.name)
                        })?;
                    let (accessor, signed) = gen_parse_field(in_field, arg_n)?;
                    let arg = gen_field(condition.field, accessor, |s| s, signed)?;
                    args.extend(quote! {
                        #[comment = #comment]
                        code |= #arg;
                    });
                }
                ConditionValue::Complex(c) => {
                    let comment = format!(" {}", condition.field.name);
                    let mut any_signed = false;
                    let arg = replace_fields(c, isa, |f| {
                        let arg_n =
                            mnemonic.args.iter().position(|a| a == &f.name).with_context(|| {
                                format!("Mnemonic {}: unknown field {}", mnemonic.name, f.name)
                            })?;
                        let (s, signed) = gen_parse_field(f, arg_n)?;
                        any_signed |= signed;
                        Ok(s)
                    })?;
                    let arg = gen_field(condition.field, quote! { (#arg) }, |s| s, any_signed)?;
                    args.extend(quote! {
                        #[comment = #comment]
                        code |= #arg;
                    });
                }
            }
        }
    }

    let arg_count = Literal::usize_unsuffixed(mnemonic.args.len());
    let mut result = TokenStream::new();
    if check_arg_count {
        result.extend(quote! { check_arg_count(args, #arg_count)?; });
    }

    let pattern = HexLiteral(pattern);
    result.extend(quote! {
        let mut code = #pattern | modifiers;
        #args
        Ok(code)
    });
    Ok(result)
}

fn gen_argument(
    args: &[String],
    arg_n: usize,
    isa: &Isa,
    replace: Option<&String>,
) -> Result<TokenStream> {
    let field = &args[arg_n];
    let Some(field) = isa.find_field(field) else { bail!("Unknown field {}", field) };
    if let Some(replace) = replace {
        let mut any_signed = false;
        let stream = replace_fields(replace, isa, |f| {
            let arg_n = args.iter().position(|a| a == &f.name).with_context(|| {
                format!("Field {} references unknown argument {}", field.name, f.name)
            })?;
            let (parse, signed) = gen_parse_field(field, arg_n)?;
            any_signed |= signed;
            Ok(parse)
        })?;
        gen_field(field, quote! { (#stream) }, |s| s, any_signed)
    } else {
        let (accessor, signed) = gen_parse_field(field, arg_n)?;
        gen_field(field, accessor, |s| s, signed)
    }
}
