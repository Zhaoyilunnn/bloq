//! Decoder runtime calls used by the Adaptive QIR target.

use crate::emit::Emitter;

pub(super) const DECLARATIONS: &str = "declare void @reset_decoder_ui64(i64)\ndeclare void @enqueue_syndromes_ui64(i64, i64, i64, i64)\ndeclare i64 @get_corrections_ui64(i64, i64, i64)\ndeclare i1 @decoder_ready_ui64(i64)\n";

impl Emitter<'_> {
    pub(super) fn decoder_reset(&mut self, decoder: u32) {
        self.line(format!("call void @reset_decoder_ui64(i64 {decoder})"));
    }

    pub(super) fn decoder_enqueue(&mut self, decoder: u32, count: usize, bits: &str, tag: usize) {
        self.line(format!(
            "call void @enqueue_syndromes_ui64(i64 {decoder}, i64 {count}, i64 {bits}, i64 {tag})"
        ));
    }

    pub(super) fn decoder_ready(&mut self, decoder: u32) -> String {
        self.value(format!("call i1 @decoder_ready_ui64(i64 {decoder})"))
    }

    pub(super) fn decoder_consume(&mut self, decoder: u32, count: u32) -> String {
        self.value(format!(
            "call i64 @get_corrections_ui64(i64 {decoder}, i64 {count}, i64 1)"
        ))
    }
}
