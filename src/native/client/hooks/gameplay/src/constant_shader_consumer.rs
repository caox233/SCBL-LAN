//! Fail-closed proof for canonical D3DDisassemble PS 4.0/4.1/5.0 text.
//! Input MUST be a complete, successful system D3DDisassemble result for the
//! exact immutable DXBC being associated with a shader (without the final NUL).
//! This is not a bytecode validator or a general assembly parser.
//!
//! Result is the conservative complete-register byte prefix read at b4/b5:
//! every textual operand cbN[i], including unreachable branches, contributes
//! (i + 1) * 16. Declarations contribute nothing. This is NOT a component mask;
//! it must not be used to infer unreferenced holes inside that prefix are safe.
//! No allocation, native access, dependencies, or mutable global state.

pub const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_REGISTERS: usize = 4096;
const SLOTS: usize = 14;

fn decimal(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let start = *pos;
    let mut n = 0usize;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        n = n.checked_mul(10)?.checked_add((bytes[*pos] - b'0') as usize)?;
        *pos += 1;
    }
    (*pos > start).then_some(n)
}

// A declaration is recognized, but its length never becomes a read extent.
fn declaration(args: &str) -> Option<()> {
    let (binding, mode) = args.split_once(',')?;
    if mode.trim() != "immediateIndexed" { return None; }
    let b = binding.trim().as_bytes();
    if !b.starts_with(b"CB") { return None; }
    let mut p = 2;
    let slot = decimal(b, &mut p)?;
    if slot >= SLOTS || b.get(p) != Some(&b'[') { return None; }
    p += 1;
    let count = decimal(b, &mut p)?;
    if count == 0 || count > MAX_REGISTERS || b.get(p) != Some(&b']') {
        return None;
    }
    p += 1;
    (p == b.len()).then_some(())
}

fn word_byte(c: u8) -> bool { c.is_ascii_alphanumeric() || c == b'_' }

fn operands(line: &str, ends: &mut [usize; 2], mut b0_gap_end: Option<&mut usize>) -> Option<()> {
    let b = line.as_bytes();
    let mut p = 0;
    while p < b.len() {
        // Reject alternate/legacy constant register encodings, not silently 0.
        if matches!(b[p], b'c' | b'C' | b'b' | b'B')
            && (p == 0 || !word_byte(b[p - 1]))
            && b.get(p + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'[')
        { return None; }
        if b[p].eq_ignore_ascii_case(&b'c')
            && b.get(p + 1).is_some_and(|c| c.eq_ignore_ascii_case(&b'b')) {
            // Canonical instructions use lowercase cb; declarations use CB.
            if &b[p..p + 2] != b"cb" || (p > 0 && !matches!(b[p - 1], b' ' | b'\t' | b',' | b'-' | b'|' | b'(')) {
                return None;
            }
            p += 2;
            let slot = decimal(b, &mut p)?;
            if slot >= SLOTS || b.get(p) != Some(&b'[') { return None; }
            p += 1;
            let index = decimal(b, &mut p)?;
            if index >= MAX_REGISTERS || b.get(p) != Some(&b']') { return None; }
            p += 1;
            let mut swizzle = &b[0..0];
            if b.get(p) == Some(&b'.') {
                p += 1;
                let start = p;
                while b.get(p).is_some_and(|c| matches!(c, b'x' | b'y' | b'z' | b'w')) { p += 1; }
                if !(1..=4).contains(&(p - start)) { return None; }
                swizzle = &b[start..p];
            }
            if p < b.len() && !matches!(b[p], b' ' | b'\t' | b',' | b'|' | b')') { return None; }
            let end = index.checked_add(1)?.checked_mul(16)?;
            if slot == 4 || slot == 5 { ends[slot - 4] = ends[slot - 4].max(end); }
            if slot == 0 {
                if let Some(read_end) = b0_gap_end.as_deref_mut() {
                    // Exactly one eligible hole [4,16). Never infer component
                    // liveness from the destination mask or the instruction.
                    // Every explicit source swizzle component contributes.
                    if index == 0 {
                        if swizzle.is_empty() || !swizzle.iter().all(|&c| c == b'x') { return None; }
                        *read_end = (*read_end).max(4);
                    } else {
                        *read_end = (*read_end).max(end);
                    }
                }
            }
            continue;
        }
        p += 1;
    }
    Some(())
}

fn accepted_opcode(op: &str) -> bool {
    // Intentionally bounded subset: unsupported linkage/custom data/opcodes
    // return None rather than guessing whether they encode another CB access.
    let (base, annotation) = op.split_once('(').unwrap_or((op, ""));
    let base = base.strip_suffix("_sat").unwrap_or(base);
    let base = if let Some(base) = base.strip_suffix("_indexable") {
        if !matches!(base, "sample" | "sample_b" | "sample_c" | "sample_c_lz" |
            "sample_d" | "sample_l" | "ld" | "ld_ms" | "resinfo" | "gather4" |
            "gather4_c" | "gather4_po" | "gather4_po_c" | "lod") { return false; }
        let Some((dimension, format)) = annotation.split_once(")(") else { return false; };
        if !matches!(dimension, "texture1d" | "texture1darray" | "texture2d" |
            "texture2darray" | "texture2dms" | "texture2dmsarray" | "texture3d" |
            "texturecube" | "texturecubearray" | "buffer") { return false; }
        let Some(format) = format.strip_suffix(')') else { return false; };
        let mut count = 0;
        for component in format.split(',') {
            if !matches!(component, "float" | "sint" | "uint") { return false; }
            count += 1;
        }
        if count != 4 { return false; }
        base
    } else {
        if !annotation.is_empty() { return false; }
        base
    };
    matches!(base,
        "add" | "and" | "break" | "breakc_z" | "breakc_nz" | "case" |
        "continue" | "continuec_z" | "continuec_nz" | "default" |
        "deriv_rtx" | "deriv_rty" | "deriv_rtx_coarse" | "deriv_rtx_fine" |
        "deriv_rty_coarse" | "deriv_rty_fine" | "discard_z" | "discard_nz" |
        "div" | "dp2" | "dp3" | "dp4" | "else" | "endif" | "endloop" |
        "endswitch" | "eq" | "exp" | "frc" | "ftoi" | "ftou" | "ge" |
        "iadd" | "if_z" | "if_nz" | "ieq" | "ige" | "ilt" | "imad" |
        "imax" | "imin" | "imul" | "ine" | "ineg" | "ishl" | "ishr" |
        "itof" | "ld" | "ld_ms" | "log" | "loop" | "lt" | "mad" |
        "min" | "max" | "mov" | "movc" | "mul" | "ne" | "nop" | "not" |
        "or" | "resinfo" | "ret" | "retc_z" | "retc_nz" | "round_ne" |
        "round_ni" | "round_pi" | "round_z" | "rsq" | "sample" | "sample_c" |
        "sample_c_lz" | "sample_l" | "sample_d" | "sample_b" | "sqrt" |
        "switch" | "sincos" | "udiv" | "ult" | "uge" | "umul" | "umad" |
        "umax" | "umin" | "ushr" | "utof" | "xor" | "rcp" | "f32tof16" |
        "f16tof32" | "countbits" | "firstbit_hi" | "firstbit_lo" | "firstbit_shi" |
        "ubfe" | "ibfe" | "bfi" | "bfrev" | "swapc" | "gather4" | "gather4_c" |
        "gather4_po" | "gather4_po_c" | "sample_info" | "sample_pos" | "lod" |
        "dcl_globalFlags" | "dcl_sampler" | "dcl_resource_texture1d" |
        "dcl_resource_texture1darray" | "dcl_resource_texture2d" |
        "dcl_resource_texture2darray" | "dcl_resource_texture2dms" |
        "dcl_resource_texture2dmsarray" | "dcl_resource_texture3d" |
        "dcl_resource_texturecube" | "dcl_resource_texturecubearray" |
        "dcl_input_ps" | "dcl_input_ps_sgv" | "dcl_input_ps_siv" |
        "dcl_output" | "dcl_output_sgv" | "dcl_output_siv" | "dcl_temps" |
        "dcl_indexableTemp" | "dcl_indexRange")
}

fn read_ends_impl(text: &str, mut b0_gap_end: Option<&mut usize>) -> Option<[usize; 2]> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.as_bytes().contains(&0) { return None; }
    let mut profile = false;
    let mut ends = [0; 2];
    let mut instructions = false;
    let mut last_ret = false;
    for raw in text.lines() {
        if raw.len() > MAX_LINE_BYTES { return None; }
        let line = raw.split_once("//").map_or(raw, |(code, _)| code).trim_matches([' ', '\t', '\r']);
        if line.is_empty() { continue; }
        if !line.is_ascii() || line.bytes().any(|c| c.is_ascii_control() && c != b'\t') { return None; }
        if !profile {
            if !matches!(line, "ps_4_0" | "ps_4_1" | "ps_5_0") { return None; }
            profile = true;
            continue;
        }
        let split = line.find([' ', '\t']).unwrap_or(line.len());
        let (op, tail) = line.split_at(split);
        let args = tail.trim();
        if op == "dcl_constantbuffer" {
            if instructions { return None; }
            declaration(args)?;
            continue;
        }
        if !accepted_opcode(op) { return None; }
        if op.starts_with("dcl_") {
            if instructions || args.is_empty() { return None; }
            // CB syntax is accepted only by dcl_constantbuffer, never an opaque dcl.
            if line.as_bytes().windows(2).any(|w| w.eq_ignore_ascii_case(b"cb")) { return None; }
        } else {
            instructions = true;
            if op == "ret" && !args.is_empty() { return None; }
            operands(args, &mut ends, b0_gap_end.as_deref_mut())?;
            last_ret = op == "ret";
        }
    }
    (profile && instructions && last_ret).then_some(ends)
}

/// Conservative maximum full-register read ends for b4/b5; None means fallback.
/// Existing API and eligibility are unchanged by the b0-specific proof.
pub fn read_ends(text: &str) -> Option<[usize; 2]> { read_ends_impl(text, None) }

/// Prove only b0's leading hole [4,16) is not read by this exact PS DXBC.
/// cb0[0] MUST have an explicit swizzle containing x only. Every cb0[i>=1]
/// operand contributes the complete 16-byte register, even a scalar swizzle.
/// Return 0 for no b0 operands, 4 for only register-zero x, or the largest full
/// register end. None means fallback. This is not a general component read mask.
/// The adapter must separately require matching PS lifetime/list/COM, the exact
/// native record coverage [0,4)+[16,len), and read_end <= len. No implicit holes,
/// reflected-size inference, relative addressing or new stage is authorized.
pub fn read_b0_gap_end(text: &str) -> Option<usize> {
    let mut end = 0;
    read_ends_impl(text, Some(&mut end))?;
    Some(end)
}

#[cfg(test)]
mod tests {
use super::{read_ends, MAX_TEXT_BYTES};
fn shader(body: &str) -> String { format!("ps_5_0\n{body}\nret\n") }





#[test]
fn native_style_read_prefixes_include_whole_registers() {
    assert_eq!(read_ends(&shader("dcl_constantbuffer CB4[12], immediateIndexed\ndcl_constantbuffer CB5[3], immediateIndexed\nmul r0.x, cb4[11].y, cb5[2].w")), Some([192, 48]));
    assert_eq!(read_ends(&shader("mov r0.x, cb4[0].x\nmov r0.y, cb4[7].w")), Some([128, 0]));
}

#[test]
fn declarations_alone_do_not_assert_a_read() {
    assert_eq!(read_ends(&shader("dcl_constantbuffer CB4[4096], immediateIndexed\ndcl_constantbuffer CB5[4], immediateIndexed")), Some([0, 0]));
}

#[test]
fn reads_are_never_clipped_to_declared_size() {
    assert_eq!(read_ends(&shader("dcl_constantbuffer CB4[1], immediateIndexed\nmov r0.x, cb4[15].x")), Some([256, 0]));
}

#[test]
fn all_literal_operands_and_branches_count() {
    assert_eq!(read_ends(&shader("mov r0, cb4[1]\nif_z r0.x\n mad r1, -cb4[2].xyzw, |cb5[3].wwww|, cb4[8].x\nendif")), Some([144, 64]));
}

#[test]
fn other_valid_immediate_slots_do_not_change_target_mask() {
    assert_eq!(read_ends(&shader("mov r0, cb0[4095]\nadd r0, cb13[2], cb7[4]")), Some([0, 0]));
}

#[test]
fn known_versions_and_crlf_comments() {
    for profile in ["ps_4_0", "ps_4_1", "ps_5_0"] {
        let text = format!("// valid comment cb4[r0] 茅\r\n{profile}\r\n\tmov r0, cb5[0].x // cb4[4294967295]\r\nret // trailing\r\n");
        assert_eq!(read_ends(&text), Some([0, 16]));
    }
}

#[test]
fn index_4095_is_last_accepted_register() {
    assert_eq!(read_ends(&shader("mov r0, cb4[4095]\nmov r1, cb5[4095]")), Some([65536, 65536]));
}

#[test]
fn dynamic_relative_and_expression_indices_fail_closed() {
    for operand in ["cb4[r0.x]", "cb4[r0.x+1]", "cb5[1+r1.x]", "cb4[1+2]", "cb7[r0.x]", "cb4[-1]", "cb4[+1]", "cb4[0x1]", "cb4[1.0]", "cb4[]", "cb4[ 1 ]"] {
        assert_eq!(read_ends(&shader(&format!("mov r0, {operand}"))), None, "{operand}");
    }
    assert_eq!(read_ends(&shader("dcl_constantbuffer CB4[12], dynamicIndexed")), None);
}

#[test]
fn unknown_slot_and_encoding_fail_closed() {
    for operand in ["cb14[0]", "cb99[0]", "cb[4][0]", "CB4[0]", "Cb4[0]", "cb_4[0]", "cb4.0", "cb4[0][1]", "cb4[0].q", "cb4[0].xyzwx", "cb4[0].", "cb4[0]suffix", "cb4[0]+1", "icb[0]", "c4[0]", "b4[0]", "cb4[0x]", "cb4{0}"] {
        assert_eq!(read_ends(&shader(&format!("mov r0, {operand}"))), None, "{operand}");
    }
}

#[test]
fn index_and_decimal_overflow_fail_closed() {
    for operand in ["cb4[4096]", "cb4[4294967295]", "cb4[18446744073709551615]", "cb4[9999999999999999999999999999]", "cb999999999999999999999999999[0]"] {
        assert_eq!(read_ends(&shader(&format!("mov r0, {operand}"))), None, "{operand}");
    }
}

#[test]
fn malformed_declaration_fail_closed() {
    for line in ["dcl_constantbuffer CB4[0], immediateIndexed", "dcl_constantbuffer CB4[4097], immediateIndexed", "dcl_constantbuffer CB14[1], immediateIndexed", "dcl_constantbuffer cb4[1], immediateIndexed", "dcl_constantbuffer CB4[1]", "dcl_constantbuffer CB4[1], immediateIndexed, junk", "dcl_constantbuffer CB4[r0], immediateIndexed", "dcl_temps cb4[1]"] {
        assert_eq!(read_ends(&shader(line)), None, "{line}");
    }
    assert_eq!(read_ends(&shader("mov r0, cb4[0]\ndcl_constantbuffer CB4[1], immediateIndexed")), None);
}

#[test]
fn unknown_stage_version_opcode_and_linkage_fail_closed() {
    for profile in ["ps_3_0", "ps_4_2", "ps_5_1", "ps_6_0", "vs_5_0", "cs_5_0", "PS_5_0", "ps_5_0 extra", ""] {
        assert_eq!(read_ends(&format!("{profile}\nret")), None, "{profile}");
    }
    for line in ["unknown r0, cb4[0]", "unknown r0, r1", "call l0", "interface_call fp0[0][0]", "dcl_interface fp0[1][1] = {ft0}", "dcl_immediateConstantBuffer {{1,2,3,4}}", "ps_5_0", "sample_indexable(bad{encoding}) r0, r1", "mov_indexable(texture2d)(float,float,float,float) r0, cb4[0]", "sample_indexable(nonsense)(float,float,float,float) r0, r1", "sample_indexable(texture2d)(float,float,float,unknown) r0, r1"] {
        assert_eq!(read_ends(&shader(line)), None, "{line}");
    }
}

#[test]
fn missing_profile_truncation_and_nul_fail_closed() {
    for text in ["", "// empty", "ret", "ps_5_0", "ps_5_0\ndcl_temps 1", "ps_5_0\nmov r0, cb4[1]", "ps_5_0\nret cb4[1]", "ps_5_0\nret\0", "\u{feff}ps_5_0\nret", "ps_5_0\n/* cb4[1] */\nret", "ps_5_0\nmov r0, cb锛擺1]\nret"] { assert_eq!(read_ends(text), None, "{text:?}"); }
}

#[test]
fn length_and_line_budgets_fail_closed() {
    let too_long = format!("{}\nps_5_0\nret", "//x\n".repeat(MAX_TEXT_BYTES / 4));
    assert_eq!(read_ends(&too_long), None);
    assert_eq!(read_ends(&format!("//{}\nps_5_0\nret", "x".repeat(4095))), None);
    let mut accepted = String::from("ps_5_0\n");
    while accepted.len() + 7 < MAX_TEXT_BYTES { accepted.push_str("//\n"); }
    accepted.push_str("ret\n");
    assert!(accepted.len() <= MAX_TEXT_BYTES);
    assert_eq!(read_ends(&accepted), Some([0, 0]));
}

#[test]
fn deterministic_and_no_input_mutation() {
    let text = shader("mov r0, cb4[11].w");
    let saved = text.clone();
    for _ in 0..50 { assert_eq!(read_ends(&text), Some([192, 0])); }
    assert_eq!(text, saved);
}

}

#[cfg(test)]
mod b0_gap_tests {
    use super::{read_b0_gap_end, read_ends};
    fn ps(body: &str) -> String { format!("ps_5_0\n{body}\nret\n") }
    #[test] fn only_explicit_x_swizzles_at_register_zero() {
        for swizzle in ["x", "xx", "xxx", "xxxx"] {
            assert_eq!(read_b0_gap_end(&ps(&format!("mov r0, cb0[0].{swizzle}"))), Some(4));
        }
        for operand in ["cb0[0]","cb0[0].y","cb0[0].xy","cb0[0].zzzz","cb0[0].w"] {
            let text=ps(&format!("mov r0.x, {operand}"));
            assert_eq!(read_b0_gap_end(&text), None);
            assert_eq!(read_ends(&text), Some([0,0]));
        }
    }
    #[test] fn bound_is_whole_register_after_the_gap_and_not_reflection() {
        assert_eq!(read_b0_gap_end(&ps("mov r0, cb0[0].xxxx\nadd r0, r0, cb0[11].x")),Some(192));
        assert_eq!(read_b0_gap_end(&ps("dcl_constantbuffer CB0[1], immediateIndexed\nmov r0, cb0[12].x")),Some(208));
        assert_eq!(read_b0_gap_end(&ps("mov r0, cb0[1]")),Some(32));
        assert_eq!(read_b0_gap_end(&ps("mov r0, cb4[0].y")),Some(0));
    }
    #[test] fn every_branch_and_operand_counts() {
        assert_eq!(read_b0_gap_end(&ps("if_z r0.x\nadd r1, -cb0[0].xxxx, |cb0[11].xyzw|\nendif")),Some(192));
        assert_eq!(read_b0_gap_end(&ps("if_z r0.x\nmov r1, cb0[0].yyyy\nendif")),None);
        assert_eq!(read_b0_gap_end(&ps("dp4 r0.x, cb0[0].xyzw, r1.xyzw")),None);
    }
    #[test] fn legacy_failure_rules_still_apply() {
        for line in ["mov r0, cb0[r1.x]","mov r0, cb0[1+r1.x]","dcl_constantbuffer CB0[12], dynamicIndexed","unknown r0, cb0[0].x","mov r0, cb0[0].q","mov r0, cb7[r0.x]"] {
            assert_eq!(read_b0_gap_end(&ps(line)),None,"{line}");
        }
    }
}
