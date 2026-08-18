#![no_std]

//! An env_logger style `RUST_LOG` filter and an alloc-free tracing
//! subscriber.
//!
//! Filter directives are comma separated `target=level` pairs or bare
//! levels. Parsing is no-alloc and stores everything in fixed size arrays so
//! it works before a heap exists. The subscriber (installed via [`init`]) is
//! statically sized as well and renders through an [`Environment`] provided
//! by the kernel.

use tracing::Level;
use tracing::level_filters::LevelFilter;

mod subscriber;
pub use subscriber::{Environment, SpanStack, init};

const MAX_DIRECTIVES: usize = 16;
const BUF_SIZE: usize = 256;

/// A directive is a target prefix mapped to a level.
///
/// An empty span (`start == end`) is the global directive matching every target.
#[derive(Copy, Clone)]
struct Directive {
    start: u16,
    end: u16,
    level: LevelFilter,
}

/// An env_logger style filter parsed once into a fixed buffer, no allocation.
pub struct Filter {
    buf: [u8; BUF_SIZE],
    directives: [Directive; MAX_DIRECTIVES],
    count: usize,
    max: LevelFilter,
}

impl Filter {
    pub fn parse(input: &str) -> Self {
        let mut buf = [0u8; BUF_SIZE];
        let len = input.len().min(BUF_SIZE);
        buf[..len].copy_from_slice(&input.as_bytes()[..len]);

        let mut directives = [Directive {
            start: 0,
            end: 0,
            level: LevelFilter::OFF,
        }; MAX_DIRECTIVES];
        let mut count = 0;
        let mut max = LevelFilter::OFF;

        let mut pos = 0;
        while pos < len && count < MAX_DIRECTIVES {
            let seg_end = buf[pos..len]
                .iter()
                .position(|&c| c == b',')
                .map_or(len, |i| pos + i);
            if let Some(directive) = parse_directive(&buf, pos, seg_end) {
                if directive.level > max {
                    max = directive.level;
                }
                directives[count] = directive;
                count += 1;
            }
            pos = seg_end + 1;
        }

        Self {
            buf,
            directives,
            count,
            max,
        }
    }

    pub fn enabled(&self, target: &str, level: &Level) -> bool {
        let mut best: Option<LevelFilter> = None;
        let mut best_len = 0;
        for directive in &self.directives[..self.count] {
            let prefix = &self.buf[directive.start as usize..directive.end as usize];
            if target.as_bytes().starts_with(prefix) && (best.is_none() || prefix.len() >= best_len)
            {
                best = Some(directive.level);
                best_len = prefix.len();
            }
        }
        best.is_some_and(|allowed| allowed >= *level)
    }

    /// The highest level across all directives, used as the global max level hint.
    pub fn max(&self) -> LevelFilter {
        self.max
    }
}

fn parse_directive(buf: &[u8], start: usize, end: usize) -> Option<Directive> {
    let (start, end) = trim(buf, start, end);
    if start == end {
        return None;
    }

    if let Some(eq) = buf[start..end]
        .iter()
        .position(|&c| c == b'=')
        .map(|i| start + i)
    {
        let (ts, te) = trim(buf, start, eq);
        let (ls, le) = trim(buf, eq + 1, end);
        let level = parse_level(&buf[ls..le])?;
        Some(Directive {
            start: ts as u16,
            end: te as u16,
            level,
        })
    } else if let Some(level) = parse_level(&buf[start..end]) {
        Some(Directive {
            start: start as u16,
            end: start as u16,
            level,
        })
    } else {
        Some(Directive {
            start: start as u16,
            end: end as u16,
            level: LevelFilter::TRACE,
        })
    }
}

fn trim(buf: &[u8], mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && buf[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && buf[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    (start, end)
}

fn parse_level(s: &[u8]) -> Option<LevelFilter> {
    if s.eq_ignore_ascii_case(b"off") {
        Some(LevelFilter::OFF)
    } else if s.eq_ignore_ascii_case(b"error") {
        Some(LevelFilter::ERROR)
    } else if s.eq_ignore_ascii_case(b"warn") {
        Some(LevelFilter::WARN)
    } else if s.eq_ignore_ascii_case(b"info") {
        Some(LevelFilter::INFO)
    } else if s.eq_ignore_ascii_case(b"debug") {
        Some(LevelFilter::DEBUG)
    } else if s.eq_ignore_ascii_case(b"trace") {
        Some(LevelFilter::TRACE)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use tracing::Level;
    use tracing::level_filters::LevelFilter;

    use super::Filter;

    #[test]
    fn bare_level_global() {
        let f = Filter::parse("debug");
        assert!(
            f.enabled("anything", &Level::DEBUG),
            "bare debug enables DEBUG for any target"
        );
        assert!(
            !f.enabled("anything", &Level::TRACE),
            "bare debug disables TRACE"
        );
    }

    #[test]
    fn target_level_pair() {
        let f = Filter::parse("kernel::mem=trace,info");
        assert!(
            f.enabled("kernel::mem", &Level::DEBUG),
            "TRACE enabled under kernel::mem"
        );
        assert!(
            f.enabled("other", &Level::INFO),
            "INFO enabled elsewhere via global fallback"
        );
        assert!(
            !f.enabled("other", &Level::DEBUG),
            "DEBUG disabled elsewhere"
        );
    }

    #[test]
    fn longest_prefix_wins() {
        let f = Filter::parse("kernel=warn,kernel::mem=trace");
        assert!(
            f.enabled("kernel::mem", &Level::DEBUG),
            "longer prefix kernel::mem chooses TRACE"
        );
        assert!(
            !f.enabled("kernel::io", &Level::DEBUG),
            "shorter prefix kernel keeps WARN elsewhere"
        );
    }

    #[test]
    fn later_directive_wins_on_equal_prefix() {
        let f = Filter::parse("debug,off");
        assert!(
            !f.enabled("anything", &Level::ERROR),
            "later off silences the earlier debug on equal prefix"
        );
    }

    #[test]
    fn bare_target_traces_only_itself() {
        let f = Filter::parse("kernel");
        assert!(
            f.enabled("kernel::mem", &Level::DEBUG),
            "bare target enables TRACE for matching targets"
        );
        assert!(
            !f.enabled("other", &Level::ERROR),
            "foreign target has no matching directive"
        );
    }

    #[test]
    fn malformed_directives_skipped() {
        let f = Filter::parse("bogus=nope,info");
        assert!(
            f.enabled("anything", &Level::INFO),
            "valid info directive survives the malformed one"
        );
        assert!(
            !f.enabled("anything", &Level::DEBUG),
            "malformed directive did not widen the filter"
        );
    }

    #[test]
    fn empty_input_disables_everything() {
        let f = Filter::parse("");
        assert!(
            !f.enabled("anything", &Level::ERROR),
            "empty input yields no directives"
        );
    }

    #[test]
    fn whitespace_only_disables_everything() {
        let f = Filter::parse("   \t  ");
        assert!(
            !f.enabled("anything", &Level::ERROR),
            "whitespace only input yields no directives"
        );
    }

    #[test]
    fn long_input_truncated_without_panic() {
        let mut input = [b'a'; 300];
        input[298] = b'=';
        input[299] = b'?';
        let s = core::str::from_utf8(&input).expect("ascii input is valid utf8");
        let f = Filter::parse(s);
        assert!(
            !f.enabled("aaa", &Level::ERROR),
            "truncated input parses without panic and matches nothing"
        );
    }

    #[test]
    fn extra_directives_ignored_without_panic() {
        let f = Filter::parse(
            "a1=info,a2=info,a3=info,a4=info,a5=info,a6=info,a7=info,a8=info,a9=info,aa=info,ab=info,ac=info,ad=info,ae=info,af=info,ag=info,ah=trace",
        );
        assert!(
            f.enabled("a1", &Level::INFO),
            "the first sixteen directives still apply"
        );
        assert!(
            !f.enabled("ah", &Level::DEBUG),
            "the seventeenth directive is ignored"
        );
    }

    #[test]
    fn off_level() {
        let f = Filter::parse("kernel=off");
        assert!(
            !f.enabled("kernel::mem", &Level::ERROR),
            "off disables even ERROR for the target"
        );
    }

    #[test]
    fn case_insensitive() {
        let f = Filter::parse("INFO,Kernel::Mem=TRACE");
        assert!(
            f.enabled("other", &Level::INFO),
            "uppercase INFO parses to the info level"
        );
        assert!(
            f.enabled("Kernel::Mem", &Level::DEBUG),
            "mixed case target and level parse"
        );
    }

    #[test]
    fn max_reflects_highest_directive() {
        let f = Filter::parse("warn,kernel::mem=trace,info");
        assert_eq!(
            f.max(),
            LevelFilter::TRACE,
            "max reflects the highest directive level"
        );
    }
}
