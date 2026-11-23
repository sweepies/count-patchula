//! The size budget for a body the Count writes. GitHub caps it, and the
//! hidden markers carrying his state are reserved before any visible
//! section is added, so no section can ever crowd them out.

/// A body under construction. `reserved` bytes are set aside for what is
/// appended after the visible text — the hidden markers and the start
/// marker — and every section is capped by both its own cap and what the
/// budget has left, so the result always fits `limit`.
pub(crate) struct Body {
    limit: usize,
    reserved: usize,
    text: String,
}

impl Body {
    /// `limit` is the body's byte budget; `reserved` is what the markers
    /// appended last will take.
    pub(crate) fn new(limit: usize, reserved: usize) -> Self {
        Self {
            limit,
            reserved,
            text: String::new(),
        }
    }
    /// Bytes a section capped at `cap` may still take.
    pub(crate) fn room(&self, cap: usize) -> usize {
        cap.min(self.limit.saturating_sub(self.reserved + self.text.len()))
    }
    /// Append `text`, cut to `room(cap)` on a char boundary.
    pub(crate) fn push(&mut self, text: &str, cap: usize) {
        self.text.push_str(patchula_datasource::upstream::truncate(
            text,
            self.room(cap),
        ));
    }
    /// Append `text` whole out of the reserve, which the caller sized to
    /// hold it: what must never be cut, such as a closing block break.
    pub(crate) fn push_reserved(&mut self, text: &str) {
        self.reserved = self
            .reserved
            .checked_sub(text.len())
            .expect("reserved text within its reserve");
        self.text.push_str(text);
    }
    /// Set `bytes` more aside for what is appended later, when the budget
    /// has that much room left; whether it had.
    pub(crate) fn reserve(&mut self, bytes: usize) -> bool {
        let fits = self.room(usize::MAX) >= bytes;
        if fits {
            self.reserved += bytes;
        }
        fits
    }
    /// The text so far; the caller appends the reserved markers.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
    /// The text so far, taken.
    pub(crate) fn into_text(self) -> String {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_never_pass_the_budget_the_reserve_set_aside() {
        let mut body = Body::new(100, 30);
        body.push(&"x".repeat(80), usize::MAX);
        body.push(&"y".repeat(80), usize::MAX);
        // The first push had the 70 bytes the reserve left; the second had
        // nothing, not its own uncapped length.
        assert_eq!(body.text(), "x".repeat(70));
        // A section's own cap still applies below the room left.
        let mut body = Body::new(100, 0);
        body.push(&"z".repeat(50), 10);
        assert_eq!(body.text(), "z".repeat(10));
        // Cutting lands on a char boundary.
        let mut body = Body::new(4, 0);
        body.push("héllo", usize::MAX);
        assert_eq!(body.text(), "hél");
    }

    #[test]
    fn reserved_text_lands_whole_after_the_budget_is_spent() {
        let mut body = Body::new(10, 2);
        body.push(&"x".repeat(20), usize::MAX);
        assert_eq!(body.text(), "x".repeat(8));
        body.push_reserved("\n\n");
        assert_eq!(body.text().len(), 10);
        assert_eq!(body.room(usize::MAX), 0);
    }
}
