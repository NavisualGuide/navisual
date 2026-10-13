/// How long an answer that has begun may go without growing before the stream counts as
/// stalled. Measured on L1's first scored pass (2026-10-12, 963 calls):
/// - 18 of 653 GPT-6 calls began the answer -- first instruction token at 4.3-9.1 s -- and then
///   sent nothing until the client's 90 s timeout ("error decoding response body"); Gemini, 310
///   calls, none;
/// - in every successful call the whole answer, first token to last, took at most 5.6 s, so 20 s
///   of silence once it has begun is over three times anything a working stream did;
/// - the slowest successful first token came at 22.7 s. A slow model thinks before it answers,
///   so the clock does not run until the answer begins: before that only the client's overall
///   timeout applies, and no slow model is cut off for thinking.
///
/// The clock measures the ANSWER, not the bytes. The first version timed silence, and on the
/// verification run the same day a stall began 5.5 s in and still ran to 90 s with no
/// `[stream]` line: bytes kept arriving -- keep-alives or other events -- while the answer
/// stopped growing, so silence never came.
pub const STREAM_IDLE: std::time::Duration = std::time::Duration::from_secs(20);

/// The longest answer a stream may carry before it counts as a runaway. Measured on L1, 2026-10-12
/// and 13: 1,358 answers that parsed, median 1,129 chars, 99th percentile 1,764, longest 2,263 --
/// and two that did not, still growing when the client's 90 s timeout cut them, at 25,125 and
/// 94,841 chars of tool arguments in about 12,000 deltas each. The stall rule rightly leaves an
/// answer that keeps growing alone, so these ran the full 90 s. Seven times the longest real
/// answer ends them in a few seconds. This covers the streaming (BYOK) clients, which send no
/// output cap. The managed client does not stream: there the relay caps OpenAI's output
/// (`max_completion_tokens` 1024) and sends Gemini none, since its thinking counts against it.
pub const MAX_ANSWER_CHARS: usize = 16_000;

/// When the answer last grew, for [`next_or_stall`]. Each client reports its answer's length
/// (tool-call arguments, or answer text) before every wait.
#[derive(Debug, Default)]
pub struct AnswerProgress {
    len: usize,
    grew_at: Option<std::time::Instant>,
}

/// What [`AnswerProgress::update`] hands [`next_or_stall`]: how long the answer is, and when it
/// last grew -- `None` until it has begun, which leaves the wait unbounded.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnswerState {
    pub len: usize,
    pub grew_at: Option<std::time::Instant>,
}

impl AnswerProgress {
    /// Record the answer's current length.
    pub fn update(&mut self, len: usize) -> AnswerState {
        if len > self.len {
            self.len = len;
            self.grew_at = Some(std::time::Instant::now());
        }
        AnswerState { len: self.len, grew_at: self.grew_at }
    }

    /// When the answer last grew, without recording anything.
    pub fn grew_at(&self) -> Option<std::time::Instant> {
        self.grew_at
    }
}

/// The `steps_seen` an `on_chunk` callback receives when everything streamed so far is void: the
/// request stalled and is being sent again, so the caller drops its partial caption.
pub const STREAM_RESET: usize = usize::MAX;

/// A stream that went silent mid-answer. Displays as the bare code the panel translates
/// (`ERROR_CODES` in App.svelte), so it never reaches the screen as English.
#[derive(Debug)]
pub struct StreamStalled;

impl std::fmt::Display for StreamStalled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stream_stalled")
    }
}

impl std::error::Error for StreamStalled {}

/// An answer past [`MAX_ANSWER_CHARS`]: the model is repeating itself, not answering. Displays as
/// the bare code the panel translates.
#[derive(Debug)]
pub struct StreamRunaway;

impl std::fmt::Display for StreamRunaway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stream_runaway")
    }
}

impl std::error::Error for StreamRunaway {}

/// The stream's next item. Once the answer has begun (`grew_at` from [`AnswerProgress`]),
/// [`STREAM_IDLE`] without it growing is a [`StreamStalled`] rather than a wait for the
/// client's 90 s timeout -- whether the stream went silent or kept sending something else. An
/// answer past [`MAX_ANSWER_CHARS`] is a [`StreamRunaway`], however fast it grows.
pub async fn next_or_stall<S>(
    stream: &mut S,
    answer: AnswerState,
    provider: &str,
) -> anyhow::Result<Option<S::Item>>
where
    S: futures_util::Stream + Unpin,
{
    if answer.len > MAX_ANSWER_CHARS {
        log::warn!(
            "[stream] {provider}: the answer passed {MAX_ANSWER_CHARS} chars ({} so far) -- a runaway",
            answer.len
        );
        return Err(StreamRunaway.into());
    }
    next_or_stall_within(stream, answer.grew_at, provider, STREAM_IDLE).await
}

async fn next_or_stall_within<S>(
    stream: &mut S,
    grew_at: Option<std::time::Instant>,
    provider: &str,
    idle: std::time::Duration,
) -> anyhow::Result<Option<S::Item>>
where
    S: futures_util::Stream + Unpin,
{
    use futures_util::StreamExt;
    let Some(grew_at) = grew_at else {
        return Ok(stream.next().await);
    };
    let stalled = || {
        log::warn!(
            "[stream] {provider}: the answer has not grown for {} ms -- stalled",
            idle.as_millis()
        );
        Err(StreamStalled.into())
    };
    // Checked before polling: a stream that keeps sending keep-alives always has an item ready,
    // and a timeout polls its future first, so a ready item would win every time.
    let left = idle.saturating_sub(grew_at.elapsed());
    if left.is_zero() {
        return stalled();
    }
    match tokio::time::timeout(left, stream.next()).await {
        Ok(item) => Ok(item),
        Err(_) => stalled(),
    }
}

/// Extract the decoded text of the **first** `"instruction"` value from a partial
/// (still-streaming) JSON string, up to (but not including) its closing quote.
///
/// Why the FIRST, not the last (audit 2026-07-12 C1): `execute_step` renders and speaks
/// `steps[0]`, so the first instruction is the one the user actually acts on — and a
/// multi-step response (Rule 1 allows 1–4 steps) contains several `"instruction":`
/// fields. Using `rfind` (the last one) meant that as step 2+ streamed in, the extracted
/// text jumped to a *different, later* string while the callers computed their emit-delta
/// against the previous length — splicing a mid-word tail of step 2 onto step 1's caption
/// ("Click the File menu" + "firm the dialog"), and byte-slicing across two different
/// strings, which **panics** when the cut lands mid-character (routine for the CJK replies
/// the LANGUAGE rule produces). Anchoring on the first instruction makes the returned prefix grow
/// **monotonically** (bytes, once decoded, never change), so every caller's recorded
/// length stays a valid char boundary and the delta is always a clean suffix — fixing both
/// the garble and the panic at the source.
///
/// Returns the decoded prefix (escape sequences resolved) — an empty string if no
/// `"instruction"` field has appeared yet, or its value hasn't started.
pub fn extract_visible_instruction(partial_json: &str) -> String {
    let prefix = "\"instruction\":";
    // FIRST occurrence — see the doc comment. Must be `find`, never `rfind`.
    if let Some(idx) = partial_json.find(prefix) {
        let remainder = &partial_json[idx + prefix.len()..];
        let trimmed = remainder.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('"') {
            let mut result = String::new();
            let mut chars = stripped.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    if let Some(next_ch) = chars.next() {
                        if next_ch == '"' {
                            result.push('"');
                        } else if next_ch == 'n' {
                            result.push('\n');
                        } else if next_ch == 't' {
                            result.push('\t');
                        } else {
                            result.push(next_ch);
                        }
                    }
                } else if ch == '"' {
                    break;
                } else {
                    result.push(ch);
                }
            }
            return result;
        }
    }
    String::new()
}

/// The clean streaming delta for a caller holding `already_emitted` bytes: the new text to
/// emit, and the new emitted-length to store. Char-boundary-safe by construction (see
/// [`extract_visible_instruction`] — the prefix grows monotonically, so `already_emitted`
/// is always a valid boundary), with a `get`-based guard so even an unexpected mid-char
/// index yields `""` instead of a panic. Callers replace the
/// `if visible.len() > emitted { on_chunk(&visible[emitted..]); emitted = visible.len() }`
/// idiom with this.
pub fn instruction_delta(partial_json: &str, already_emitted: usize) -> (String, usize) {
    let visible = extract_visible_instruction(partial_json);
    if visible.len() > already_emitted {
        let new_text = visible.get(already_emitted..).unwrap_or("").to_string();
        (new_text, visible.len())
    } else {
        (String::new(), already_emitted)
    }
}

/// How many steps have STARTED streaming in a partial `navigate_step` JSON: the count
/// of `"instruction"` keys seen so far. Monotonic (the buffer only appends), so the
/// panel can show "Step 1 of ~N" live as later steps arrive instead of discarding that
/// signal until the response completes (design suggestion #2, 2026-07-13).
///
/// Exact against values-that-mention-instructions: inside a JSON string value the
/// quotes would be escaped (`\"instruction\"`), so the literal `"instruction"`-then-`:`
/// pattern can only match a real key. The count is still an estimate of the FINAL step
/// count (hence "~N" in the UI) — more steps may follow.
pub fn count_streamed_steps(partial_json: &str) -> usize {
    let key = "\"instruction\"";
    let mut count = 0;
    let mut rest = partial_json;
    while let Some(idx) = rest.find(key) {
        let after = &rest[idx + key.len()..];
        if after.trim_start().starts_with(':') {
            count += 1;
        }
        rest = after;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::{count_streamed_steps, extract_visible_instruction, instruction_delta};

    /// The stall rule never cuts a model that is still thinking: before the answer begins the
    /// wait is unbounded here (the client's overall timeout is the only limit). Once it has
    /// begun, the idle limit without growth is a `StreamStalled` -- silent or not -- and an
    /// answer that keeps growing is never one.
    #[tokio::test]
    async fn a_stall_is_an_answer_that_stopped_growing() {
        use std::time::{Duration, Instant};
        let idle = Duration::from_millis(40);

        // Thinking: no data for five idle limits, then the answer arrives.
        let mut slow = Box::pin(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            1
        }));
        let got = super::next_or_stall_within(&mut slow, None, "test", idle).await.unwrap();
        assert_eq!(got, Some(1), "a slow first token is waited for");

        // Answering, then silence: a stall, as the bare code the panel maps.
        let mut silent = futures_util::stream::pending::<u8>();
        let err = super::next_or_stall_within(&mut silent, Some(Instant::now()), "test", idle)
            .await
            .unwrap_err();
        assert!(err.is::<super::StreamStalled>());
        assert_eq!(err.to_string(), "stream_stalled");

        // Answering, then keep-alives: an item is always ready, the answer is not growing.
        let mut keepalive = futures_util::stream::repeat(0u8);
        let long_ago = Instant::now() - Duration::from_millis(100);
        let err = super::next_or_stall_within(&mut keepalive, Some(long_ago), "test", idle)
            .await
            .unwrap_err();
        assert!(err.is::<super::StreamStalled>(), "bytes without growth are a stall");

        // An answer that grows a little with every chunk, each just under the limit: never.
        let mut steady = Box::pin(futures_util::stream::unfold(0u8, |n| async move {
            if n == 3 {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            Some((n, n + 1))
        }));
        let mut progress = super::AnswerProgress::default();
        let mut answer = 0usize;
        let mut seen = 0;
        while let Some(_) = super::next_or_stall_within(&mut steady, progress.update(answer).grew_at, "test", idle)
            .await
            .unwrap()
        {
            answer += 10;
            seen += 1;
        }
        assert_eq!(seen, 3);
    }

    /// A runaway is cut by length, however fast it grows; an answer at the limit is not one.
    #[tokio::test]
    async fn a_runaway_is_an_answer_past_any_real_step() {
        use super::{next_or_stall, AnswerState, StreamRunaway, MAX_ANSWER_CHARS};
        let now = Some(std::time::Instant::now());
        let mut fast = futures_util::stream::repeat(0u8);
        let err = next_or_stall(&mut fast, AnswerState { len: MAX_ANSWER_CHARS + 1, grew_at: now }, "test")
            .await
            .unwrap_err();
        assert!(err.is::<StreamRunaway>());
        assert_eq!(err.to_string(), "stream_runaway");
        let got = next_or_stall(&mut fast, AnswerState { len: MAX_ANSWER_CHARS, grew_at: now }, "test")
            .await
            .unwrap();
        assert_eq!(got, Some(0));
    }

    #[test]
    fn extracts_first_instruction_not_last() {
        // A multi-step response mid-stream: step 1 complete, step 2 partial. The
        // extractor must stay on step 1 (what execute_step renders), never jump to step 2.
        let partial = r#"{"steps":[{"instruction":"Click the File menu","checkpoint":true},{"instruction":"Confi"#;
        assert_eq!(extract_visible_instruction(partial), "Click the File menu");
    }

    #[test]
    fn grows_monotonically_as_first_instruction_streams() {
        let a = r#"{"steps":[{"instruction":"Click the"#;
        let b = r#"{"steps":[{"instruction":"Click the File menu"#;
        assert_eq!(extract_visible_instruction(a), "Click the");
        assert_eq!(extract_visible_instruction(b), "Click the File menu");
        // b's decoded prefix starts with a's — the invariant the delta logic relies on.
        assert!(extract_visible_instruction(b).starts_with(&extract_visible_instruction(a)));
    }

    #[test]
    fn delta_is_a_clean_suffix() {
        let a = r#"{"steps":[{"instruction":"Click the"#;
        let (t1, n1) = instruction_delta(a, 0);
        assert_eq!(t1, "Click the");
        assert_eq!(n1, 9);
        let b = r#"{"steps":[{"instruction":"Click the File menu"#;
        let (t2, n2) = instruction_delta(b, n1);
        assert_eq!(t2, " File menu");
        assert_eq!(n2, 19);
    }

    #[test]
    fn delta_never_panics_on_cjk() {
        // A Chinese first instruction streaming in char by char — byte lengths land on
        // multi-byte boundaries. With the old rfind + raw-slice this could panic; here
        // every delta must be valid UTF-8 and reassemble to the full string.
        let full = "点击文件菜单"; // 6 chars, 18 bytes
        let mut emitted = 0usize;
        let mut assembled = String::new();
        for i in 1..=full.chars().count() {
            let sofar: String = full.chars().take(i).collect();
            let partial = format!(r#"{{"steps":[{{"instruction":"{sofar}"#);
            let (delta, n) = instruction_delta(&partial, emitted);
            assembled.push_str(&delta);
            emitted = n;
        }
        assert_eq!(assembled, full);
    }

    #[test]
    fn empty_before_instruction_appears() {
        assert_eq!(extract_visible_instruction(r#"{"steps":[{"#), "");
        assert_eq!(instruction_delta(r#"{"steps":[{"#, 0), (String::new(), 0));
    }

    #[test]
    fn counts_steps_as_they_stream() {
        assert_eq!(count_streamed_steps(r#"{"steps":[{"#), 0);
        assert_eq!(count_streamed_steps(r#"{"steps":[{"instruction":"Click"#), 1);
        assert_eq!(
            count_streamed_steps(
                r#"{"steps":[{"instruction":"Click File","checkpoint":true},{"instruction":"Con"#
            ),
            2
        );
        // Key mentioned INSIDE a value is escaped in raw JSON — must not count.
        assert_eq!(
            count_streamed_steps(r#"{"steps":[{"instruction":"type \"instruction\": here"#),
            1
        );
    }
}
