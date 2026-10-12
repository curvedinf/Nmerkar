//! SPEC drift guard: SPEC.md's opcode-reference table and every numeric
//! slot/live/glyph claim are enforced against the compiler tables in
//! lex.rs (OP_NAMES, OP_GLYPHS, V_SPACE, L_SPACE, text_mnemonic). A spec
//! claim that drifts from the compiler — or a compiler table edit that the
//! spec was not updated for — fails `cargo test` here. See SPEC.md
//! "Opcode reference".

use crate::lex::{opcode_index, OP_GLYPHS, OP_NAMES, L_SPACE, V_SPACE, text_mnemonic};

fn spec_text() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../SPEC.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", path.display(), e))
}

// A slot is live when its name is not a "~NN" retired placeholder and its
// text mnemonic was not retired in place (slot 83, ENDT).
fn is_live(i: usize) -> bool {
    !OP_NAMES[i].starts_with('~') && text_mnemonic(i) != "~retired"
}

fn live_indices() -> Vec<usize> {
    (0..OP_NAMES.len()).filter(|&i| is_live(i)).collect()
}

#[test]
fn table_shape_and_retirement() {
    assert_eq!(OP_NAMES.len(), 222);
    assert_eq!(OP_GLYPHS.len(), OP_NAMES.len());
    // exactly one slot is retired in place: 83 (ENDT) — name kept, mnemonic dead
    let retired_in_place: Vec<usize> = (0..OP_NAMES.len())
        .filter(|&i| !OP_NAMES[i].starts_with('~') && text_mnemonic(i) == "~retired")
        .collect();
    assert_eq!(retired_in_place, vec![83], "exactly slot 83 must be retired in place");
    assert_eq!(OP_NAMES[83], "ENDT");
    let live = live_indices();
    assert_eq!(live.len(), 203, "live-op count");
    // 18 "~NN" retired slots, all glyph-less
    let retired: Vec<usize> = (0..OP_NAMES.len()).filter(|&i| OP_NAMES[i].starts_with('~')).collect();
    assert_eq!(retired.len(), 18, "retired ~NN slot count");
    for &i in &retired {
        assert_eq!(OP_GLYPHS[i], 0, "retired slot {} ({}) must have no glyph", i, OP_NAMES[i]);
    }
    // the only live ops without a dense glyph are cap (86) / caps (87)
    let glyphless: Vec<usize> = live.iter().copied().filter(|&i| OP_GLYPHS[i] == 0).collect();
    assert_eq!(glyphless, vec![86, 87], "cap/caps are the only text-only live ops");
    assert_eq!(OP_NAMES[86], "CAP");
    assert_eq!(OP_NAMES[87], "CAPS");
}

#[test]
fn glyph_spaces_unique_disjoint_roundtrip() {
    // every nonzero opcode glyph is unique (201 live + ENDT's reserved one)
    let mut seen: Vec<(u32, usize)> = Vec::new(); // (glyph, first slot)
    for (i, &g) in OP_GLYPHS.iter().enumerate() {
        if g != 0 {
            if let Some(&(_, first)) = seen.iter().find(|(x, _)| *x == g) {
                panic!("glyph U+{:X} reused: slots {} and {}", g, first, i);
            }
            seen.push((g, i));
        }
    }
    assert_eq!(seen.len(), 202, "nonzero opcode glyph count");
    // v-space / l-space: 64 unique codepoints each
    for (name, space) in [("V_SPACE", &V_SPACE), ("L_SPACE", &L_SPACE)] {
        assert_eq!(space.len(), 64);
        let mut s = space.to_vec();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), 64, "{} must have 64 unique codepoints", name);
    }
    // op glyphs, v-space, l-space are pairwise disjoint
    for &(g, _) in &seen {
        assert!(!V_SPACE.contains(&g), "op glyph U+{:X} also in V_SPACE", g);
        assert!(!L_SPACE.contains(&g), "op glyph U+{:X} also in L_SPACE", g);
    }
    for &v in V_SPACE.iter() {
        assert!(!L_SPACE.contains(&v), "v glyph U+{:X} also in L_SPACE", v);
    }
    // every live-with-glyph index round-trips through the reverse lookup
    for &i in live_indices().iter() {
        if OP_GLYPHS[i] != 0 {
            let c = char::from_u32(OP_GLYPHS[i]).unwrap();
            assert_eq!(opcode_index(c), Some(i), "glyph of slot {} does not round-trip", i);
        }
    }
}

#[test]
fn spec_opcode_table_matches_compiler_tables() {
    let spec = spec_text();
    // row shape: `| <idx> | <glyph-or-empty> | `<mnemonic>` | ...`
    let mut rows: Vec<(usize, Option<char>, String)> = Vec::new();
    for line in spec.lines() {
        let rest = match line.strip_prefix('|') {
            Some(r) => r,
            None => continue,
        };
        let mut cells = rest.split('|');
        let idx_cell = cells.next().unwrap_or("").trim().to_string();
        let glyph_cell = cells.next().unwrap_or("").trim().to_string();
        let mn_cell = cells.next().unwrap_or("").trim().to_string();
        if idx_cell.is_empty() || !idx_cell.bytes().all(|b| b.is_ascii_digit()) {
            continue; // header, separator, or a non-opcode table
        }
        let mn = match mn_cell.strip_prefix('`').and_then(|m| m.strip_suffix('`')) {
            Some(m) => m.to_string(),
            None => continue,
        };
        let idx: usize = idx_cell.parse().unwrap();
        let glyph = if glyph_cell.is_empty() {
            None
        } else {
            let mut cs = glyph_cell.chars();
            let c = cs.next().unwrap();
            assert!(cs.next().is_none(), "SPEC row {}: glyph cell is not one codepoint", idx);
            Some(c)
        };
        rows.push((idx, glyph, mn));
    }
    let live = live_indices();
    assert_eq!(rows.len(), live.len(), "SPEC opcode-row count must equal the live-op count");
    for &(idx, glyph, ref mn) in &rows {
        assert!(idx < OP_NAMES.len(), "SPEC row index {} is out of range", idx);
        assert!(is_live(idx), "SPEC lists row {} (`{}`), but that slot is not live in the compiler", idx, mn);
        assert_eq!(text_mnemonic(idx), mn.as_str(), "SPEC row {} mnemonic drift", idx);
        match glyph {
            Some(c) => assert_eq!(c as u32, OP_GLYPHS[idx], "SPEC row {} glyph codepoint drift", idx),
            None => assert_eq!(OP_GLYPHS[idx], 0, "SPEC row {} shows no glyph but the compiler assigns one", idx),
        }
    }
    let listed: std::collections::HashSet<usize> = rows.iter().map(|r| r.0).collect();
    for &i in &live {
        assert!(listed.contains(&i), "live op {} (`{}`) is missing from SPEC's opcode tables", i, text_mnemonic(i));
    }
}

// every number that immediately precedes `needle` in the line (ASCII scan;
// needle contains no multibyte chars, so byte matching cannot split a char)
fn numbers_before(line: &str, needle: &str) -> Vec<u64> {
    let b = line.as_bytes();
    let nb = needle.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + nb.len() <= b.len() {
        if &b[i..i + nb.len()] == nb {
            let mut j = i;
            while j > 0 && b[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j < i {
                if let Ok(v) = line[j..i].parse() {
                    out.push(v);
                }
            }
            i += nb.len();
        } else {
            i += 1;
        }
    }
    out
}

// every "<N> [opcode ]slots (0..<M>)" claim in the line. Anchored on the
// word "slots" so unrelated ranges like "(0..63)" are not slot claims.
fn slots_claims(line: &str) -> Vec<(u64, u64)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = line[i..].find("slots (0..") {
        let at = i + pos;
        // M: digit run right after "(0.."
        let mut k = at + "slots (0..".len();
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        let m: u64 = line[at + "slots (0..".len()..k].parse().unwrap();
        // N: digits before "… [opcode ]slots (0.."
        let mut j = at;
        while j > 0 && b[j - 1] == b' ' {
            j -= 1;
        }
        if line[..j].ends_with("opcode") {
            j -= "opcode".len();
        }
        while j > 0 && b[j - 1] == b' ' {
            j -= 1;
        }
        let n_end = j;
        while j > 0 && b[j - 1].is_ascii_digit() {
            j -= 1;
        }
        assert!(j < n_end, "malformed slots claim: {}", line);
        let n: u64 = line[j..n_end].parse().unwrap();
        out.push((n, m));
        i = k;
    }
    out
}

#[test]
fn spec_numeric_claims_match_compiler_tables() {
    let slots = OP_NAMES.len() as u64;
    let live = live_indices().len() as u64;
    let glyphed = live_indices().iter().filter(|&&i| OP_GLYPHS[i] != 0).count() as u64;
    let spec = spec_text();

    let mut slot_claims = 0;
    let mut live_claims = 0;
    let mut glyph_claims = 0;
    for line in spec.lines() {
        for (n, m) in slots_claims(line) {
            assert_eq!(n, slots, "SPEC claims {} slots; the compiler has {}: {}", n, slots, line);
            assert_eq!(m, slots - 1, "SPEC slot range must end at {}: {}", slots - 1, line);
            slot_claims += 1;
        }
        for n in numbers_before(line, " live opcodes")
            .into_iter()
            .chain(numbers_before(line, " live ("))
        {
            assert_eq!(n, live, "SPEC claims {} live ops; the compiler has {}: {}", n, live, line);
            live_claims += 1;
        }
        for n in numbers_before(line, " with dense glyphs")
            .into_iter()
            .chain(numbers_before(line, " of them have unique"))
        {
            assert_eq!(n, glyphed, "SPEC claims {} glyphed ops; the compiler has {}: {}", n, glyphed, line);
            glyph_claims += 1;
        }
    }
    // the claim families must stay present — deleting a claim must fail, not pass silently
    assert!(slot_claims >= 1, "SPEC.md lost its slot-count claim");
    assert!(live_claims >= 1, "SPEC.md lost its live-opcode claim");
    assert!(glyph_claims >= 1, "SPEC.md lost its dense-glyph claim");
}
