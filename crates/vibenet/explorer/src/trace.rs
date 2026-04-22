//! callTracer output → renderable tree.
//!
//! `debug_traceTransaction` with `{"tracer": "callTracer"}` returns a nested
//! JSON tree where each node has at minimum `type`, `from`, `to`, `input`,
//! `output`, `gas`, `gasUsed`, `value`, and an optional `calls` array of
//! children. Some nodes also carry `error` / `revertReason`.
//!
//! Rather than render this schema through Askama directly (which doesn't do
//! recursive macros ergonomically), we parse it into [`TraceNode`] and then
//! emit pre-built HTML with native `<details>` elements for collapse/expand.
//! No JavaScript, and the tree degrades gracefully if a user disables it.

use crate::models::{AddrLabel, format_eth};
use alloy_primitives::{Address, U256};
use serde_json::Value;
use std::fmt::Write;

/// One call frame in the trace tree.
pub struct TraceNode {
    pub call_type: String,
    pub from: AddrLabel,
    pub to: Option<AddrLabel>,
    /// Only populated when `value > 0`.
    pub value_eth: Option<String>,
    pub gas_used: Option<u64>,
    /// First 4 bytes of input as `0x########`, if input is at least that long.
    pub selector: Option<String>,
    /// `0x{hex}` preview (first 64 hex chars) followed by `… (N bytes)`, or
    /// the full hex if it's short.
    pub input_preview: String,
    pub input_full: String,
    pub input_bytes: usize,
    pub output_preview: Option<String>,
    pub output_full: Option<String>,
    pub output_bytes: usize,
    pub error: Option<String>,
    pub revert_reason: Option<String>,
    pub children: Vec<TraceNode>,
}

impl TraceNode {
    /// Parse a callTracer JSON node. Returns `None` if required fields
    /// are missing — the caller should treat that as "trace not available".
    pub fn from_json(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let call_type = obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("CALL")
            .to_string();
        let from_str = obj.get("from").and_then(Value::as_str).unwrap_or("");
        let from = parse_addr_label(from_str)?;
        let to = obj.get("to").and_then(Value::as_str).and_then(parse_addr_label);

        let value = obj.get("value").and_then(Value::as_str).and_then(parse_u256_hex);
        let value_eth = match value {
            Some(v) if v > U256::ZERO => Some(format_eth(v)),
            _ => None,
        };

        let gas_used = obj.get("gasUsed").and_then(Value::as_str).and_then(parse_u64_hex);

        let input_hex = obj.get("input").and_then(Value::as_str).unwrap_or("0x");
        let (input_full, input_preview, input_bytes, selector) = split_hex(input_hex);

        let (output_full, output_preview, output_bytes) = match obj
            .get("output")
            .and_then(Value::as_str)
        {
            Some(s) if !s.is_empty() && s != "0x" => {
                let (full, preview, bytes, _) = split_hex(s);
                (Some(full), Some(preview), bytes)
            }
            _ => (None, None, 0),
        };

        let error = obj.get("error").and_then(Value::as_str).map(String::from);
        let revert_reason = obj.get("revertReason").and_then(Value::as_str).map(String::from);

        let children = obj
            .get("calls")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(TraceNode::from_json).collect())
            .unwrap_or_default();

        Some(Self {
            call_type,
            from,
            to,
            value_eth,
            gas_used,
            selector,
            input_preview,
            input_full,
            input_bytes,
            output_preview,
            output_full,
            output_bytes,
            error,
            revert_reason,
            children,
        })
    }

    /// Total number of call frames in the subtree (including self).
    pub fn total_calls(&self) -> usize {
        1 + self.children.iter().map(TraceNode::total_calls).sum::<usize>()
    }

    /// Render the whole tree to HTML. Top-level node is open by default;
    /// nested nodes are collapsed. Addresses are linked to their address
    /// pages.
    pub fn render_html(&self) -> String {
        let mut out = String::with_capacity(1024);
        self.write_html(&mut out, true);
        out
    }

    fn write_html(&self, out: &mut String, is_root: bool) {
        let open = if is_root { " open" } else { "" };
        let cls = format!("call-{}", self.call_type.to_lowercase());
        let has_error = self.error.is_some() || self.revert_reason.is_some();

        let _ = write!(out, r#"<details class="trace-node{err}"{open}>"#,
            err = if has_error { " trace-node-err" } else { "" },
            open = open);

        // --- summary line ---
        let _ = write!(out, r#"<summary><span class="call-type {cls}">{ty}</span>"#,
            cls = cls,
            ty = html_escape(&self.call_type));

        // Use the full 40-char address in trace summaries so devs can diff
        // frames by eye without hovering each row. `code.addr` matches the
        // rule used in the home/block tables (smaller font, allowed to wrap).
        let _ = write!(
            out,
            r#" <a href="/address/{ffull}"><code class="addr">{ffull}</code></a>"#,
            ffull = html_escape(&self.from.full),
        );

        if let Some(to) = &self.to {
            let _ = write!(
                out,
                r#" → <a href="/address/{tfull}"><code class="addr">{tfull}</code></a>"#,
                tfull = html_escape(&to.full),
            );
        }

        if let Some(sel) = &self.selector {
            let _ = write!(out, r#" <code class="selector">{}</code>"#, html_escape(sel));
        }

        if let Some(v) = &self.value_eth {
            let _ = write!(out, r#" <span class="dim">· {}</span>"#, html_escape(v));
        }
        if let Some(g) = self.gas_used {
            let _ = write!(out, r#" <span class="dim">· {g} gas</span>"#);
        }
        if !self.children.is_empty() {
            let _ = write!(out, r#" <span class="dim">· {} subcall{}</span>"#,
                self.children.len(),
                if self.children.len() == 1 { "" } else { "s" });
        }
        if has_error {
            out.push_str(r#" <span class="trace-err-badge">error</span>"#);
        }
        out.push_str("</summary>");

        // --- body: inputs/outputs/errors, then nested children ---
        out.push_str(r#"<div class="trace-body">"#);

        if self.input_bytes > 0 {
            let _ = write!(
                out,
                r#"<div class="trace-row"><span class="trace-label">input</span>"#
            );
            if self.input_full == self.input_preview {
                let _ = write!(out, r#"<code class="wrap">{}</code>"#, html_escape(&self.input_full));
            } else {
                let _ = write!(
                    out,
                    r#"<details class="inline-hex"><summary><code>{}</code> <span class="dim">· {} bytes</span></summary><pre class="raw"><code>{}</code></pre></details>"#,
                    html_escape(&self.input_preview),
                    self.input_bytes,
                    html_escape(&self.input_full),
                );
            }
            out.push_str("</div>");
        }

        if let (Some(full), Some(preview)) = (&self.output_full, &self.output_preview) {
            let _ = write!(
                out,
                r#"<div class="trace-row"><span class="trace-label">output</span>"#
            );
            if full == preview {
                let _ = write!(out, r#"<code class="wrap">{}</code>"#, html_escape(full));
            } else {
                let _ = write!(
                    out,
                    r#"<details class="inline-hex"><summary><code>{}</code> <span class="dim">· {} bytes</span></summary><pre class="raw"><code>{}</code></pre></details>"#,
                    html_escape(preview),
                    self.output_bytes,
                    html_escape(full),
                );
            }
            out.push_str("</div>");
        }

        if let Some(err) = &self.error {
            let _ = write!(
                out,
                r#"<div class="trace-row trace-err"><span class="trace-label">error</span>{}</div>"#,
                html_escape(err)
            );
        }
        if let Some(reason) = &self.revert_reason {
            let _ = write!(
                out,
                r#"<div class="trace-row trace-err"><span class="trace-label">revert</span>{}</div>"#,
                html_escape(reason)
            );
        }

        if !self.children.is_empty() {
            out.push_str(r#"<div class="trace-children">"#);
            for c in &self.children {
                c.write_html(out, false);
            }
            out.push_str("</div>");
        }

        out.push_str("</div></details>");
    }
}

fn parse_addr_label(s: &str) -> Option<AddrLabel> {
    let clean = s.strip_prefix("0x").unwrap_or(s);
    if clean.len() != 40 {
        return None;
    }
    let bytes = hex::decode(clean).ok()?;
    Some(AddrLabel::from_addr(&Address::from_slice(&bytes)))
}

fn parse_u64_hex(s: &str) -> Option<u64> {
    let clean = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(clean, 16).ok()
}

fn parse_u256_hex(s: &str) -> Option<U256> {
    let clean = s.strip_prefix("0x").unwrap_or(s);
    U256::from_str_radix(clean, 16).ok()
}

/// Returns (full `0x` string, short preview, byte count, optional selector).
fn split_hex(hex_str: &str) -> (String, String, usize, Option<String>) {
    let inner = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = inner.len() / 2;
    let full = format!("0x{inner}");
    // Preview: up to 64 hex chars, else truncate with "…".
    let preview = if inner.len() <= 64 {
        full.clone()
    } else {
        format!("0x{}…", &inner[..64])
    };
    let selector = if inner.len() >= 8 {
        Some(format!("0x{}", &inner[..8]))
    } else {
        None
    };
    (full, preview, bytes, selector)
}

/// Minimal HTML-escape for the fields we interpolate. The tx data we're
/// rendering is hex-only or fixed labels, so we only need to handle the
/// five metacharacters correctly; we don't attempt to normalize unicode.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

