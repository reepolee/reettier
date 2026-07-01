//! Format dispatch by extension.
//!
//! The engine is being built rule-by-rule. Until each language path is wired to
//! the token engine, it passes content through unchanged (never corrupts —
//! graceful degradation is the whole point; see CONTEXT.md).

use crate::config::Config;

pub fn format_source(content: &str, ext: &str, config: &Config) -> String {
    // CRLF handling is centralized here — the single choke point every language
    // path flows through — so no inner engine (ts/js/css/ree) ever sees `\r`.
    // The markup path splits on `'\n'` and keeps a trailing `\r`, while embedded
    // code goes through `str::lines()`, which strips it; feeding both LF-only
    // text avoids the resulting mixed-ending output. Detect the file's dominant
    // convention up front, normalize to LF, format, then restore CRLF on the way
    // out so all-CRLF input round-trips to all-CRLF output.
    let had_crlf = content.contains("\r\n");
    let lf_owned;
    let lf: &str = if had_crlf {
        lf_owned = content.replace("\r\n", "\n");
        &lf_owned
    } else {
        content
    };

    let out = match ext {
        "ts" | "js" => format_js(lf, config),
        "css" => format_css(lf, config),
        "ree" => format_ree(lf, config),
        // Unknown extension: never touch it — return the *original* bytes
        // verbatim (not the normalized copy), so non-formatted files are
        // never modified, line endings included.
        _ => return content.to_string(),
    };

    if had_crlf {
        out.replace('\n', "\r\n")
    } else {
        out
    }
}

fn format_js(content: &str, config: &Config) -> String {
    crate::engine::format_js(content, &config.indent)
}

fn format_css(content: &str, config: &Config) -> String {
    crate::engine::format_css(content, &config.indent)
}

fn format_ree(content: &str, config: &Config) -> String {
    crate::ree::format_ree(content, &config.indent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(content: &str, ext: &str) -> String {
        format_source(content, ext, &Config::default())
    }

    fn count_bare_lf(s: &str) -> usize {
        // `\n` not preceded by `\r`.
        s.as_bytes()
            .iter()
            .enumerate()
            .filter(|(i, b)| **b == b'\n' && (*i == 0 || s.as_bytes()[i - 1] != b'\r'))
            .count()
    }

    #[test]
    fn ree_crlf_round_trips_to_all_crlf() {
        // Markup-only CRLF input must come back all-CRLF, with no bare `\n`.
        let input = "<ul>\r\n{#each outcomes as item}\r\n<li>{~ md(item)}</li>\r\n{/each}\r\n</ul>\r\n";
        let out = fmt(input, "ree");
        assert_eq!(count_bare_lf(&out), 0, "found bare LF in output:\n{out:?}");
        assert!(out.contains("\r\n"), "expected CRLF endings:\n{out:?}");
    }

    #[test]
    fn ree_crlf_with_embedded_script_stays_all_crlf() {
        // The confirmed mixed-ending case: markup kept CRLF while embedded code
        // dropped to LF. After boundary normalization the whole file is CRLF.
        let input = "<ul>\r\n{#each outcomes as item}\r\n<li>{~ md(item)}</li>\r\n{/each}\r\n</ul>\r\n\
                     <script>\r\nconst a = [\r\n1,\r\n2\r\n]\r\n</script>\r\n";
        let out = fmt(input, "ree");
        assert_eq!(
            count_bare_lf(&out),
            0,
            "embedded code produced bare LF (mixed endings):\n{out:?}"
        );
    }

    #[test]
    fn ree_crlf_is_idempotent() {
        let input = "<ul>\r\n{#each outcomes as item}\r\n<li>{~ md(item)}</li>\r\n{/each}\r\n</ul>\r\n\
                     <script>\r\nconst a = [\r\n1,\r\n2\r\n]\r\n</script>\r\n";
        let once = fmt(input, "ree");
        let twice = fmt(&once, "ree");
        assert_eq!(once, twice, "not idempotent on CRLF");
    }

    #[test]
    fn ree_lf_input_stays_lf() {
        // LF-only input must never gain `\r`.
        let input = "<ul>\n{#each outcomes as item}\n<li>{~ md(item)}</li>\n{/each}\n</ul>\n";
        let out = fmt(input, "ree");
        assert!(!out.contains('\r'), "LF input gained CR:\n{out:?}");
    }

    #[test]
    fn ts_crlf_stays_all_crlf() {
        let input = "const a = [\r\n1,\r\n2\r\n]\r\n";
        let out = fmt(input, "ts");
        assert_eq!(count_bare_lf(&out), 0, "ts output has bare LF:\n{out:?}");
    }

    #[test]
    fn css_crlf_stays_all_crlf() {
        let input = ".a {\r\ncolor: red;\r\n}\r\n";
        let out = fmt(input, "css");
        assert_eq!(count_bare_lf(&out), 0, "css output has bare LF:\n{out:?}");
    }

    #[test]
    fn unknown_ext_crlf_is_untouched() {
        // Non-formatted extensions must be returned byte-for-byte, CRLF included.
        let input = "a\r\nb\r\n";
        assert_eq!(fmt(input, "txt"), input);
    }
}
