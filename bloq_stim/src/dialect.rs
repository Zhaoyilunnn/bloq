//! The `HONEST_T` convention: how a non-Clifford `T` survives a round trip
//! through Stim, which has no such gate.
//!
//! Bloq's own circuit text is *Clifft* — Stim's syntax plus the non-Clifford
//! `T`/`T_DAG` gates and `EXP_VAL` probes that the Clifft simulator
//! understands. Stim itself rejects both. Rather than lose the T gates,
//! emission in the [`StimDialect::Stim`] dialect writes each one as the
//! Clifford `S` that shares its axis, tagged `HONEST_T`. The tag is inert to
//! Stim (it round-trips through parse/print) and identifies exactly the
//! instructions to turn back into `T` on the way out, so the two dialects name
//! the same circuit.
//!
//! Owning both directions here is the point: the convention is one table, and
//! a reader and a writer that disagree about it silently corrupt a circuit.

/// The two spellings emission can produce for a non-Clifford `T` gate.
///
/// The dialects differ *only* in how `T`/`T_DAG` are written; every other
/// instruction is identical, so a circuit with no non-Clifford gate emits
/// byte-identically in both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum StimDialect {
    /// Stim proper: `T` becomes `S[HONEST_T]` and `T_DAG` becomes
    /// `S_DAG[HONEST_T]`. The default, because it is the only dialect Stim can
    /// parse.
    #[default]
    Stim,
    /// Clifft: `T`/`T_DAG` are written literally. Stim cannot parse the
    /// result; the Clifft simulator can.
    Clifft,
}

/// The `(stim gate, clifft gate)` pairs the convention maps, longest first so
/// a prefix match cannot mistake `T_DAG` for `T`.
const HONEST_T_GATES: [(&str, &str); 2] = [("S_DAG", "T_DAG"), ("S", "T")];

/// The tag marking an `S` that is really a `T`.
pub const HONEST_T_TAG: &str = "HONEST_T";

/// Rewrite Clifft circuit text so Stim can parse it.
///
/// Two changes, both forced by Stim's grammar: `T`/`T_DAG` become
/// `S[HONEST_T]`/`S_DAG[HONEST_T]`, and **`EXP_VAL` lines are dropped**
/// entirely — they are a Clifft-only probe instruction with no Stim
/// equivalent, so there is nothing to translate them into. That makes the
/// conversion lossy in exactly one way; [`stim_to_clifft_text`] recovers the
/// gates but not the probes.
///
/// Only a gate that opens a line (after indentation) and is followed by its
/// target list is rewritten, so an already-tagged or otherwise decorated `T`
/// is left alone rather than silently re-spelled.
///
/// # Examples
///
/// ```
/// # use bloq_stim::clifft_to_stim_text;
/// let clifft = "H 0\nREPEAT 2 {\n    T_DAG 1\n}\nEXP_VAL X0*Z1\n";
///
/// assert_eq!(
///     clifft_to_stim_text(clifft),
///     "H 0\nREPEAT 2 {\n    S_DAG[HONEST_T] 1\n}\n",
/// );
/// ```
pub fn clifft_to_stim_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let content = line.trim_start();
        let indent = &line[..line.len() - content.len()];
        if content.starts_with("EXP_VAL ") {
            continue;
        }
        out.push_str(indent);
        match HONEST_T_GATES
            .iter()
            .find(|(_, clifft)| opens_instruction(content, clifft))
        {
            Some((stim, clifft)) => {
                out.push_str(stim);
                out.push('[');
                out.push_str(HONEST_T_TAG);
                out.push(']');
                out.push_str(&content[clifft.len()..]);
            }
            None => out.push_str(content),
        }
        out.push('\n');
    }
    // `lines` drops the final terminator; a text that did not end in one keeps
    // that shape so the conversion does not invent a line.
    if !text.ends_with('\n') && out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Rewrite Stim circuit text back into Clifft: every `S[HONEST_T]` and
/// `S_DAG[HONEST_T]` becomes the `T`/`T_DAG` it stands for.
///
/// The inverse of [`clifft_to_stim_text`] up to that function's `EXP_VAL`
/// loss. Untagged `S` gates are genuine Cliffords and are left alone — which
/// is the whole reason the tag exists.
///
/// # Examples
///
/// ```
/// # use bloq_stim::stim_to_clifft_text;
/// // The untagged `S` is a real Clifford and survives untouched.
/// assert_eq!(
///     stim_to_clifft_text("S 0\nS_DAG[HONEST_T] 1\n"),
///     "S 0\nT_DAG 1\n",
/// );
/// ```
pub fn stim_to_clifft_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let content = line.trim_start();
        let indent = &line[..line.len() - content.len()];
        out.push_str(indent);
        let replacement = HONEST_T_GATES.iter().find_map(|(stim, clifft)| {
            let rest = content
                .strip_prefix(stim)?
                .strip_prefix('[')?
                .strip_prefix(HONEST_T_TAG)?
                .strip_prefix(']')?;
            rest.starts_with(' ').then_some((*clifft, rest))
        });
        match replacement {
            Some((clifft, rest)) => {
                out.push_str(clifft);
                out.push_str(rest);
            }
            None => out.push_str(content),
        }
    }
    out
}

/// Whether `content` opens with `gate` used as an instruction mnemonic — the
/// bare name followed by its target list, not a longer name (`T_XZ`, `TICK`)
/// that merely starts with the same letters, and not a decorated one
/// (`T[TAG]`) whose spelling the caller chose deliberately.
fn opens_instruction(content: &str, gate: &str) -> bool {
    content
        .strip_prefix(gate)
        .is_some_and(|rest| rest.starts_with(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honest_t_round_trips_through_stim() {
        let clifft = "QUBIT_COORDS(0, 0) 0\nT 0\nREPEAT 3 {\n    T_DAG 0\n    S 0\n}\nM 0\n";

        let stim = clifft_to_stim_text(clifft);

        assert!(stim.contains("S[HONEST_T] 0"));
        assert!(stim.contains("    S_DAG[HONEST_T] 0"), "indent preserved");
        assert_eq!(stim_to_clifft_text(&stim), clifft);
    }

    #[test]
    fn only_tagged_s_gates_become_t_gates() {
        // `S_DAG` is checked before `S`, and a plain `S` is a real Clifford.
        assert_eq!(stim_to_clifft_text("S 0\nS_DAG 1\n"), "S 0\nS_DAG 1\n");
    }

    #[test]
    fn reverse_conversion_only_rewrites_instruction_mnemonics() {
        let stim = "# S[HONEST_T] is documentation\nS[HONEST_T] 0\n    S_DAG[HONEST_T] 1\n";

        assert_eq!(
            stim_to_clifft_text(stim),
            "# S[HONEST_T] is documentation\nT 0\n    T_DAG 1\n"
        );
    }

    /// A gate name that merely starts with `T` is not a `T` gate.
    #[test]
    fn longer_gate_names_are_not_mistaken_for_t() {
        let clifft = "T_XZ 0\nTICK\n";

        assert_eq!(clifft_to_stim_text(clifft), clifft);
    }

    #[test]
    fn conversion_preserves_the_absence_of_a_trailing_newline() {
        assert_eq!(clifft_to_stim_text("T 0"), "S[HONEST_T] 0");
        assert_eq!(clifft_to_stim_text("T 0\n"), "S[HONEST_T] 0\n");
        assert_eq!(clifft_to_stim_text(""), "");
    }
}
